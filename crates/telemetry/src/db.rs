// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The usage store: one global SQLite database (not repo-scoped, since spend
//! is only meaningful as a total across projects). Connection setup mirrors
//! `gate::db::open_db`: the terminal view reads while `ingest` writes from
//! other processes, which is what WAL mode and `busy_timeout` are for.

use super::{ToolInvocationEvent, ToolKind, UsageEvent};
use crate::machine_rows::LOCAL_MACHINE_SQL;
use crate::sync_tables::account_source;
use crate::util::atomic::{ensure_private_dir, make_db_file_private};
use playbook_store::{params, Connection, OptionalExtension};
use std::path::Path;
use std::time::Duration;

/// The usage database layout this build writes (2 added the `machine` column,
/// 3 the one-row `machine` table, 4 the `session_account_spans` table, 5 the
/// sync tables and the `usage_events_all` view). A database without the row
/// counts as version 0 and is stamped on open.
pub const SCHEMA_VERSION: i64 = 5;

const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS usage_events (
    event_id TEXT PRIMARY KEY,
    timestamp INTEGER NOT NULL,
    session_id TEXT NOT NULL,
    account TEXT NOT NULL,
    model TEXT NOT NULL,
    effort TEXT NOT NULL,
    repo TEXT NOT NULL,
    branch TEXT NOT NULL,
    input_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    cache_creation_tokens INTEGER NOT NULL,
    cache_read_tokens INTEGER NOT NULL,
    cost_usd REAL NOT NULL CHECK(cost_usd >= 0),
    cache_creation_1h_tokens INTEGER NOT NULL DEFAULT 0,
    agent TEXT NOT NULL DEFAULT 'claude-code'
);
CREATE INDEX IF NOT EXISTS usage_events_by_time ON usage_events (timestamp, event_id);
CREATE INDEX IF NOT EXISTS usage_events_by_session ON usage_events (session_id, timestamp);
CREATE TABLE IF NOT EXISTS tool_invocation_events (
    event_id TEXT PRIMARY KEY,
    timestamp INTEGER NOT NULL,
    session_id TEXT NOT NULL,
    account TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('skill','agent')),
    name TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS tool_invocation_events_by_kind_time
    ON tool_invocation_events (kind, timestamp, name);
CREATE TABLE IF NOT EXISTS usage_backfill_files (
    path TEXT PRIMARY KEY,
    mtime INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS usage_watermarks (
    source TEXT PRIMARY KEY,
    watermark INTEGER NOT NULL
);";

const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

const BUSY_PAUSE: Duration = Duration::from_millis(20);

/// Retries `op` while SQLite reports BUSY, until `budget` has elapsed. The busy
/// handler is skipped when waiting could deadlock (for example while another
/// opener switches to WAL), so `busy_timeout` alone returns BUSY at once.
fn retry_busy<T>(
    mut op: impl FnMut() -> playbook_store::Result<T>,
    budget: Duration,
) -> playbook_store::Result<T> {
    let start = std::time::Instant::now();
    loop {
        match op() {
            Err(playbook_store::Error::SqliteFailure(err, _))
                if err.code == playbook_store::ErrorCode::DatabaseBusy
                    && start.elapsed() < budget =>
            {
                std::thread::sleep(BUSY_PAUSE);
            }
            other => return other,
        }
    }
}

/// Before the standard data file is opened, moves in any older database. Other
/// paths (a test database, say) are never migrated.
fn migrate_if_standard(path: &Path) {
    let standard = super::host::state_dir().join("usage.db");
    if path == standard {
        super::migrate::run_standard(path);
    }
}

/// Open (creating if missing) the usage database at `path`.
pub fn open_db(path: &Path) -> Result<Connection, String> {
    migrate_if_standard(path);
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)
            .map_err(|e| format!("failed to create directory {}: {e}", parent.display()))?;
    }
    make_db_file_private(path)?;
    let conn = open_prepared(path)?;
    if let Some(parent) = path.parent() {
        super::machine_rows::adopt_local_machine(&conn, parent)?;
    }
    Ok(conn)
}

/// Opens a usage database file the user named, as it is: no folder or
/// permission is changed, and nothing is moved in. Used to rehearse on a copy.
pub fn open_existing_db(path: &Path) -> Result<Connection, String> {
    if !path.is_file() {
        return Err(format!("no usage database at {}", path.display()));
    }
    open_prepared(path)
}

fn open_prepared(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open(path)
        .map_err(|e| format!("failed to open usage database at {}: {e}", path.display()))?;
    // Before any statement that can contend for the write lock; a fresh
    // connection's default timeout is 0, which fails instead of retrying.
    conn.busy_timeout(BUSY_TIMEOUT)
        .map_err(|e| format!("failed to set busy_timeout: {e}"))?;
    retry_busy(
        || conn.pragma_update(None, "journal_mode", "WAL"),
        BUSY_TIMEOUT,
    )
    .map_err(|e| format!("failed to set journal_mode=WAL: {e}"))?;
    // Before any table is created, so a newer database is left untouched.
    playbook_store::schema::check_and_stamp(&conn, "usage.db", SCHEMA_VERSION)
        .map_err(|e| e.to_string())?;
    retry_busy(|| conn.execute_batch(SCHEMA_SQL), BUSY_TIMEOUT)
        .map_err(|e| format!("failed to create usage schema: {e}"))?;
    ensure_cache_1h_column(&conn)?;
    ensure_agent_column(&conn)?;
    retry_busy(
        || conn.execute_batch(super::telemetry_db::UP_SQL),
        BUSY_TIMEOUT,
    )
    .map_err(|e| format!("failed to create telemetry schema: {e}"))?;
    super::telemetry_db::ensure_columns(&conn)?;
    super::machine_rows::ensure_machine_column(&conn)?;
    retry_busy(
        || conn.execute_batch(super::machine::SCHEMA_SQL),
        BUSY_TIMEOUT,
    )
    .map_err(|e| format!("failed to create the machine table: {e}"))?;
    retry_busy(
        || conn.execute_batch(super::account_store::SCHEMA_SQL),
        BUSY_TIMEOUT,
    )
    .map_err(|e| format!("failed to create account names table: {e}"))?;
    retry_busy(
        || conn.execute_batch(super::session_account::SCHEMA_SQL),
        BUSY_TIMEOUT,
    )
    .map_err(|e| format!("failed to create the session accounts table: {e}"))?;
    super::session_account::copy_legacy_rows(&conn)?;
    retry_busy(
        || conn.execute_batch(super::account_move::SCHEMA_SQL),
        BUSY_TIMEOUT,
    )
    .map_err(|e| format!("failed to create account move tables: {e}"))?;
    let sync_sql = super::sync_tables::schema_sql();
    retry_busy(|| conn.execute_batch(&sync_sql), BUSY_TIMEOUT)
        .map_err(|e| format!("failed to create the sync tables: {e}"))?;
    Ok(conn)
}

/// Adds `cache_creation_1h_tokens` to a database created before it existed.
/// A concurrent caller winning the race is harmless ("duplicate column").
fn ensure_cache_1h_column(conn: &Connection) -> Result<(), String> {
    let has: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('usage_events')
                           WHERE name = 'cache_creation_1h_tokens')",
            [],
            |row| row.get(0),
        )
        .map_err(|e| format!("failed to inspect usage_events: {e}"))?;
    if has {
        return Ok(());
    }
    match conn.execute(
        "ALTER TABLE usage_events ADD COLUMN cache_creation_1h_tokens INTEGER NOT NULL DEFAULT 0",
        [],
    ) {
        Ok(_) => Ok(()),
        Err(e) if e.to_string().contains("duplicate column") => Ok(()),
        Err(e) => Err(format!("failed to add cache_creation_1h_tokens: {e}")),
    }
}

/// Adds `agent` to a database created before it existed; old rows are Claude Code.
fn ensure_agent_column(conn: &Connection) -> Result<(), String> {
    let has: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('usage_events') WHERE name = 'agent')",
            [],
            |row| row.get(0),
        )
        .map_err(|e| format!("failed to inspect usage_events: {e}"))?;
    if has {
        return Ok(());
    }
    match conn.execute(
        "ALTER TABLE usage_events ADD COLUMN agent TEXT NOT NULL DEFAULT 'claude-code'",
        [],
    ) {
        Ok(_) => Ok(()),
        Err(e) if e.to_string().contains("duplicate column") => Ok(()),
        Err(e) => Err(format!("failed to add agent: {e}")),
    }
}

/// Last ingested timestamp for `source`; 0 when it has never been ingested.
pub fn get_watermark(conn: &Connection, source: &str) -> Result<i64, String> {
    conn.query_row(
        "SELECT watermark FROM usage_watermarks WHERE source = ?1",
        [source],
        |row| row.get(0),
    )
    .optional()
    .map(|found| found.unwrap_or(0))
    .map_err(|e| format!("failed to read watermark for {source}: {e}"))
}

/// Raises the watermark but never moves it backwards: a slower ingest
/// that read an older watermark must not undo a newer one.
pub fn raise_watermark(conn: &Connection, source: &str, watermark: i64) -> Result<(), String> {
    conn.execute(
        "INSERT INTO usage_watermarks (source, watermark) VALUES (?1, ?2)
         ON CONFLICT(source) DO UPDATE SET watermark = MAX(watermark, excluded.watermark)",
        params![source, watermark],
    )
    .map(|_| ())
    .map_err(|e| format!("failed to raise watermark for {source}: {e}"))
}

fn exists(conn: &Connection, table: &str, event_id: &str) -> Result<bool, String> {
    conn.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE event_id = ?1)"),
        [event_id],
        |row| row.get(0),
    )
    .map_err(|e| format!("failed to look up {event_id} in {table}: {e}"))
}

pub fn has_usage_event(conn: &Connection, event_id: &str) -> Result<bool, String> {
    exists(conn, "usage_events", event_id)
}

pub fn has_tool_event(conn: &Connection, event_id: &str) -> Result<bool, String> {
    exists(conn, "tool_invocation_events", event_id)
}

fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Inserts the event, or when its id is already stored raises the token
/// counts to the larger values. A message is written as several lines whose
/// output count can grow, so a poll that saw it early must not freeze a
/// partial count. Returns the number of rows written (0 when nothing grew).
pub fn upsert_usage_event(conn: &Connection, e: &UsageEvent) -> Result<usize, String> {
    conn.execute(
        &format!(
            "INSERT INTO usage_events (event_id, timestamp, session_id, account, model, effort,
            repo, branch, input_tokens, output_tokens, cache_creation_tokens,
            cache_read_tokens, cost_usd, cache_creation_1h_tokens, agent, machine)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, {LOCAL_MACHINE_SQL})
         ON CONFLICT(event_id) DO UPDATE SET
            input_tokens = excluded.input_tokens,
            output_tokens = excluded.output_tokens,
            cache_creation_tokens = excluded.cache_creation_tokens,
            cache_read_tokens = excluded.cache_read_tokens,
            cost_usd = excluded.cost_usd,
            cache_creation_1h_tokens = excluded.cache_creation_1h_tokens
         WHERE excluded.output_tokens > usage_events.output_tokens"
        ),
        params![
            e.event_id,
            e.timestamp,
            e.session_id,
            e.account,
            e.model,
            e.effort,
            e.repo,
            e.branch,
            to_i64(e.input_tokens),
            to_i64(e.output_tokens),
            to_i64(e.cache_creation_tokens),
            to_i64(e.cache_read_tokens),
            e.cost_usd,
            to_i64(e.cache_creation_1h_tokens),
            e.agent
        ],
    )
    .map_err(|e2| format!("failed to upsert usage event {}: {e2}", e.event_id))
}

pub fn count_usage_events(conn: &Connection) -> Result<i64, String> {
    conn.query_row("SELECT COUNT(*) FROM usage_events", [], |row| row.get(0))
        .map_err(|e| format!("failed to count usage events: {e}"))
}

/// The transcripts a first-run backfill has finished, with the modification
/// time each had when it was read.
pub fn backfilled_files(
    conn: &Connection,
) -> Result<std::collections::HashMap<String, i64>, String> {
    let mut stmt = conn
        .prepare("SELECT path, mtime FROM usage_backfill_files")
        .map_err(|e| format!("failed to read the backfill progress: {e}"))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .map_err(|e| format!("failed to read the backfill progress: {e}"))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| format!("failed to read the backfill progress: {e}"))
}

pub fn record_backfilled_file(conn: &Connection, path: &str, mtime: i64) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO usage_backfill_files (path, mtime) VALUES (?1, ?2)",
        playbook_store::params![path, mtime],
    )
    .map(|_| ())
    .map_err(|e| format!("failed to record the backfill progress: {e}"))
}

pub fn clear_backfilled_files(conn: &Connection) -> Result<(), String> {
    conn.execute("DELETE FROM usage_backfill_files", [])
        .map(|_| ())
        .map_err(|e| format!("failed to clear the backfill progress: {e}"))
}

/// Empties the usage history in one transaction: the events, the tool
/// events and the read progress, so the next ingest reads every transcript
/// again like a first run. Settings, the budget, the limit readings, the
/// account names, the account moves and the telemetry rows stay, and a move
/// labels its events again when they are read. A reader sees the history
/// before the call or none of it, never a part.
pub fn reset_usage(conn: &Connection) -> Result<(), String> {
    let tx = playbook_store::Transaction::new_unchecked(
        conn,
        playbook_store::TransactionBehavior::Immediate,
    )
    .map_err(|e| format!("failed to begin the usage reset: {e}"))?;
    for table in RESET_TABLES {
        tx.execute(&format!("DELETE FROM {table}"), [])
            .map_err(|e| format!("failed to clear {table}: {e}"))?;
    }
    tx.commit()
        .map_err(|e| format!("failed to commit the usage reset: {e}"))
}

/// The tables `reset_usage` empties: the history and where its reading got to.
const RESET_TABLES: [&str; 4] = [
    "usage_events",
    "tool_invocation_events",
    "usage_backfill_files",
    "usage_watermarks",
];

/// The newest usage timestamp stored for an agent, or 0.
pub fn newest_usage_of(conn: &Connection, agent: &str) -> Result<i64, String> {
    conn.query_row(
        "SELECT COALESCE(MAX(timestamp), 0) FROM usage_events WHERE agent = ?1",
        [agent],
        |r| r.get(0),
    )
    .map_err(|e| format!("failed to read the newest usage event: {e}"))
}

/// The message count and the first and last timestamp of the whole store.
pub fn usage_span(conn: &Connection) -> Result<super::tui::data::StoreSpan, String> {
    conn.query_row(
        "SELECT COUNT(*), MIN(timestamp), MAX(timestamp) FROM usage_events",
        [],
        |row| {
            Ok(super::tui::data::StoreSpan {
                events: row.get(0)?,
                first: row.get(1)?,
                last: row.get(2)?,
            })
        },
    )
    .map_err(|e| format!("failed to read the usage span: {e}"))
}

/// Corrects the two fields an older ingest got wrong. Returns rows changed.
pub fn set_repo_and_cache_tier(
    conn: &Connection,
    event_id: &str,
    repo: &str,
    cache_creation_1h_tokens: u64,
) -> Result<usize, String> {
    conn.execute(
        "UPDATE usage_events SET repo = ?2, cache_creation_1h_tokens = ?3 WHERE event_id = ?1",
        params![event_id, repo, to_i64(cache_creation_1h_tokens)],
    )
    .map_err(|e| format!("failed to update usage event {event_id}: {e}"))
}

pub fn insert_tool_event(conn: &Connection, e: &ToolInvocationEvent) -> Result<(), String> {
    conn.execute(
        &format!(
            "INSERT INTO tool_invocation_events
                (event_id, timestamp, session_id, account, kind, name, machine)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, {LOCAL_MACHINE_SQL})"
        ),
        params![
            e.event_id,
            e.timestamp,
            e.session_id,
            e.account,
            e.kind.as_str(),
            e.name
        ],
    )
    .map(|_| ())
    .map_err(|e2| format!("failed to insert tool event {}: {e2}", e.event_id))
}

const USAGE_COLUMNS: &str = "event_id, timestamp, session_id, account, model, effort, repo, branch,
    input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
    cache_creation_1h_tokens, agent, machine";

const LOCAL_TABLE: &str = "usage_events";
const TAIL_ALL: &str = "ORDER BY timestamp, event_id";
const TAIL_SINCE: &str = "WHERE timestamp >= ?1 ORDER BY timestamp, event_id";
const TAIL_LATEST: &str = "WHERE (?2 IS NULL OR machine = ?2)
    ORDER BY timestamp DESC, event_id DESC LIMIT ?1";

fn tail_of_sessions(marks: &str) -> String {
    format!("WHERE session_id IN ({marks}) ORDER BY timestamp, event_id")
}

fn query_usage(
    conn: &Connection,
    table: &str,
    tail: &str,
    params: &[&dyn playbook_store::ToSql],
) -> Result<Vec<UsageEvent>, String> {
    let mut stmt = conn
        .prepare(&format!("SELECT {USAGE_COLUMNS} FROM {table} {tail}"))
        .map_err(|e| format!("failed to prepare usage query: {e}"))?;
    let rows = stmt
        .query_map(params, |r| {
            let mut event = UsageEvent {
                event_id: r.get(0)?,
                timestamp: r.get(1)?,
                session_id: r.get(2)?,
                account: r.get(3)?,
                model: r.get(4)?,
                effort: r.get(5)?,
                repo: r.get(6)?,
                branch: r.get(7)?,
                input_tokens: r.get::<_, i64>(8)?.max(0) as u64,
                output_tokens: r.get::<_, i64>(9)?.max(0) as u64,
                cache_creation_tokens: r.get::<_, i64>(10)?.max(0) as u64,
                cache_read_tokens: r.get::<_, i64>(11)?.max(0) as u64,
                cache_creation_1h_tokens: r.get::<_, i64>(12)?.max(0) as u64,
                agent: r.get(13)?,
                machine: r.get(14)?,
                ..UsageEvent::default()
            };
            event.apply_pricing();
            Ok(event)
        })
        .map_err(|e| format!("failed to read usage events: {e}"))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| format!("failed to read a usage event row: {e}"))
}

/// Loads every usage event, pricing each one now from its stored token counts
/// and the current price table. The stored `cost_usd` column is ignored, so a
/// price correction never needs a re-ingest.
pub fn load_usage_events(conn: &Connection) -> Result<Vec<UsageEvent>, String> {
    query_usage(conn, LOCAL_TABLE, TAIL_ALL, &[])
}

/// Every event of the account, this machine's and the synced ones of other
/// machines, priced like `load_usage_events`.
pub fn load_account_events(conn: &Connection) -> Result<Vec<UsageEvent>, String> {
    query_usage(conn, account_source(conn), TAIL_ALL, &[])
}

/// The account's events at or after `since` (epoch seconds), oldest first.
pub fn load_account_events_since(conn: &Connection, since: i64) -> Result<Vec<UsageEvent>, String> {
    query_usage(conn, account_source(conn), TAIL_SINCE, &[&since])
}

/// Events at or after `since` (epoch seconds), oldest first.
pub fn load_usage_events_since(conn: &Connection, since: i64) -> Result<Vec<UsageEvent>, String> {
    query_usage(conn, LOCAL_TABLE, TAIL_SINCE, &[&since])
}

/// The newest `limit` events, newest first. `machine` keeps one machine's
/// events, `None` keeps all of them.
pub fn load_latest_usage_events(
    conn: &Connection,
    limit: i64,
    machine: Option<&str>,
) -> Result<Vec<UsageEvent>, String> {
    query_usage(conn, LOCAL_TABLE, TAIL_LATEST, &[&limit, &machine])
}

/// Every event of the given sessions, oldest first.
pub fn load_usage_events_of_sessions(
    conn: &Connection,
    sessions: &[String],
) -> Result<Vec<UsageEvent>, String> {
    if sessions.is_empty() {
        return Ok(Vec::new());
    }
    let marks = vec!["?"; sessions.len()].join(",");
    let params: Vec<&dyn playbook_store::ToSql> = sessions
        .iter()
        .map(|s| s as &dyn playbook_store::ToSql)
        .collect();
    query_usage(conn, LOCAL_TABLE, &tail_of_sessions(&marks), &params)
}

pub fn load_tool_events(conn: &Connection) -> Result<Vec<ToolInvocationEvent>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT event_id, timestamp, session_id, account, kind, name
             FROM tool_invocation_events ORDER BY timestamp, event_id",
        )
        .map_err(|e| format!("failed to prepare tool query: {e}"))?;
    let rows = stmt
        .query_map([], |r| {
            let kind: String = r.get(4)?;
            Ok(ToolInvocationEvent {
                event_id: r.get(0)?,
                timestamp: r.get(1)?,
                session_id: r.get(2)?,
                account: r.get(3)?,
                kind: if kind == "skill" {
                    ToolKind::Skill
                } else {
                    ToolKind::Agent
                },
                name: r.get(5)?,
            })
        })
        .map_err(|e| format!("failed to read tool events: {e}"))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| format!("failed to read a tool event row: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::scratch_dir;
    use std::fs;

    #[cfg(unix)]
    #[test]
    fn the_database_and_its_directory_are_owner_only_and_an_older_file_is_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;

        // Arrange: a database from before this change, readable by others.
        let dir = scratch_dir("usage-private").join("usage");
        let path = dir.join("usage.db");
        drop(open_db(&path).unwrap());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

        // Act
        let conn = open_db(&path).unwrap();
        conn.execute("INSERT INTO usage_watermarks VALUES ('s', 1)", [])
            .unwrap();

        // Assert
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&path), 0o600);
        let wal = dir.join("usage.db-wal");
        assert!(
            wal.exists() && mode(&wal) & 0o077 == 0,
            "the WAL must be private too"
        );
        let _ = fs::remove_dir_all(dir.parent().unwrap());
    }

    fn stored_schema_version(path: &Path) -> Option<String> {
        let conn = Connection::open(path).unwrap();
        conn.query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .ok()
    }

    #[test]
    fn a_fresh_database_is_stamped_with_the_current_schema_version() {
        // Arrange
        let path = scratch_dir("usage-schema-fresh").join("usage.db");

        // Act
        drop(open_db(&path).unwrap());

        // Assert
        assert_eq!(
            stored_schema_version(&path),
            Some(SCHEMA_VERSION.to_string())
        );
    }

    #[test]
    fn a_database_without_the_version_row_opens_and_is_stamped() {
        // Arrange: a database from before the row existed.
        let path = scratch_dir("usage-schema-old").join("usage.db");
        drop(open_db(&path).unwrap());
        Connection::open(&path)
            .unwrap()
            .execute("DROP TABLE meta", [])
            .unwrap();

        // Act
        let result = open_db(&path);

        // Assert
        assert!(result.is_ok());
        assert_eq!(
            stored_schema_version(&path),
            Some(SCHEMA_VERSION.to_string())
        );
    }

    #[test]
    fn a_version_2_database_upgrades_to_current_with_its_rows_and_id_intact() {
        // Arrange: a database as the previous build left it, with no machine table.
        let path = scratch_dir("usage-schema-v2").join("usage.db");
        let conn = open_db(&path).unwrap();
        conn.execute_batch(
            "INSERT INTO usage_events (event_id, timestamp, session_id, account, model, effort,
                 repo, branch, input_tokens, output_tokens, cache_creation_tokens,
                 cache_read_tokens, cost_usd)
             VALUES ('e', 1, 's', '', 'm', '', 'r', 'b', 1, 1, 0, 0, 0.5);
             DROP TABLE machine;
             UPDATE meta SET value = '2' WHERE key = 'schema_version';",
        )
        .unwrap();
        drop(conn);

        // Act
        let conn = open_db(&path).unwrap();

        // Assert
        assert_eq!(
            stored_schema_version(&path),
            Some(SCHEMA_VERSION.to_string())
        );
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM usage_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
        let id = crate::machine::stored(&conn).unwrap().unwrap().id;
        assert_eq!(crate::machine_rows::local_machine_id(&conn).unwrap(), id);
    }

    #[test]
    fn a_version_3_database_upgrades_to_4_with_its_rows_and_spans_copied() {
        // Arrange: a database as version 3 left it, with a session account row.
        let path = scratch_dir("usage-schema-v3").join("usage.db");
        let conn = open_db(&path).unwrap();
        conn.execute_batch(
            "INSERT INTO usage_events (event_id, timestamp, session_id, account, model, effort,
                 repo, branch, input_tokens, output_tokens, cache_creation_tokens,
                 cache_read_tokens, cost_usd)
             VALUES ('e', 1, 's', 'a', 'm', '', 'r', 'b', 1, 1, 0, 0, 0.5);
             INSERT INTO session_accounts VALUES ('s', 'token:aaaa1111');
             DROP TABLE session_account_spans;
             DELETE FROM usage_watermarks WHERE source = 'session-account-spans-copied';
             UPDATE meta SET value = '3' WHERE key = 'schema_version';",
        )
        .unwrap();
        drop(conn);

        // Act
        let conn = open_db(&path).unwrap();
        let again = open_db(&path).unwrap();

        // Assert
        assert_eq!(
            stored_schema_version(&path),
            Some(SCHEMA_VERSION.to_string())
        );
        let count = |c: &Connection, table: &str| -> i64 {
            c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(count(&conn, "usage_events"), 1);
        assert_eq!(count(&conn, "session_account_spans"), 1);
        assert_eq!(count(&again, "session_account_spans"), 1);
        assert_eq!(
            crate::session_account::of_session_at(&conn, "s", 5)
                .unwrap()
                .as_deref(),
            Some("token:aaaa1111")
        );
    }

    #[test]
    fn a_version_4_database_upgrades_to_5_with_its_rows_and_gains_the_sync_tables() {
        // Arrange: a database as version 4 left it, without the sync objects.
        let path = scratch_dir("usage-schema-v4").join("usage.db");
        let conn = open_db(&path).unwrap();
        conn.execute_batch(
            "INSERT INTO usage_events (event_id, timestamp, session_id, account, model, effort,
                 repo, branch, input_tokens, output_tokens, cache_creation_tokens,
                 cache_read_tokens, cost_usd)
             VALUES ('e', 1, 's', 'a', 'm', '', 'r', 'b', 1, 1, 0, 0, 0.5);
             DROP VIEW usage_events_all;
             DROP TABLE usage_events_remote;
             DROP TABLE account_aliases;
             DROP TABLE sync_state;
             DROP TABLE sync_watermarks;
             DROP TABLE sync_cursors;
             UPDATE meta SET value = '4' WHERE key = 'schema_version';",
        )
        .unwrap();
        drop(conn);

        // Act
        let conn = open_db(&path).unwrap();

        // Assert
        assert_eq!(
            stored_schema_version(&path),
            Some(SCHEMA_VERSION.to_string())
        );
        assert_eq!(SCHEMA_VERSION, 5);
        let names: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE name LIKE 'sync_%' OR name IN ('usage_events_remote', 'account_aliases', 'usage_events_all') ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            names,
            [
                "account_aliases",
                "sync_cursors",
                "sync_state",
                "sync_watermarks",
                "usage_events_all",
                "usage_events_remote"
            ]
        );
        assert_eq!(load_account_events(&conn).unwrap().len(), 1);
    }

    #[test]
    fn a_second_open_keeps_the_machine_id() {
        // Arrange
        let path = scratch_dir("usage-machine-twice").join("usage.db");
        let first = crate::machine::stored(&open_db(&path).unwrap()).unwrap();

        // Act
        let second = crate::machine::stored(&open_db(&path).unwrap()).unwrap();

        // Assert
        assert!(first.is_some());
        assert_eq!(first, second);
    }

    #[test]
    fn a_newer_database_is_refused_naming_both_versions_and_stays_as_it_was() {
        // Arrange
        let path = scratch_dir("usage-schema-newer").join("usage.db");
        drop(open_db(&path).unwrap());
        let newer = (SCHEMA_VERSION + 1).to_string();
        Connection::open(&path)
            .unwrap()
            .execute("UPDATE meta SET value = ?1", [&newer])
            .unwrap();

        // Act
        let err = open_db(&path).unwrap_err();

        // Assert
        assert!(err.contains(&format!("version {newer}")), "{err}");
        assert!(
            err.contains(&format!("understands {SCHEMA_VERSION}")),
            "{err}"
        );
        assert!(err.contains("playbook upgrade"), "{err}");
        assert_eq!(stored_schema_version(&path), Some(newer));
    }

    #[test]
    fn a_reset_empties_the_history_and_the_read_progress_but_keeps_the_rest() {
        // Arrange
        let dir = scratch_dir("usage-reset");
        let conn = open_db(&dir.join("usage.db")).unwrap();
        conn.execute_batch(
            "INSERT INTO usage_events (event_id, timestamp, session_id, account, model, effort, repo,
                branch, input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, cost_usd)
             VALUES ('e', 1, 's', 'a', 'm', '', 'r', 'b', 1, 1, 0, 0, 1.0);
             INSERT INTO tool_invocation_events VALUES ('t', 1, 's', 'a', 'skill', 'n', '');
             INSERT INTO usage_backfill_files VALUES ('/f', 1);
             INSERT INTO usage_watermarks VALUES ('claude-code', 5);
             INSERT INTO account_names (key, name) VALUES ('k', 'Work');",
        )
        .unwrap();
        let count = |t: &str| -> i64 {
            conn.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
                .unwrap()
        };

        // Act
        reset_usage(&conn).unwrap();
        reset_usage(&conn).unwrap();

        // Assert
        for table in RESET_TABLES {
            assert_eq!(count(table), 0, "{table}");
        }
        assert_eq!(count("account_names"), 1);
        let _ = fs::remove_dir_all(dir);
    }

    fn busy() -> playbook_store::Error {
        playbook_store::Error::SqliteFailure(
            playbook_store::ffi::Error::new(playbook_store::ffi::SQLITE_BUSY),
            Some("database is locked".into()),
        )
    }

    #[test]
    fn a_busy_answer_is_retried_until_it_clears() {
        let tries = std::cell::Cell::new(0);
        let out = retry_busy(
            || {
                tries.set(tries.get() + 1);
                if tries.get() <= 3 {
                    Err(busy())
                } else {
                    Ok("done")
                }
            },
            Duration::from_secs(5),
        );
        assert_eq!(out.unwrap(), "done");
        assert_eq!(tries.get(), 4);
    }

    #[test]
    fn a_busy_answer_that_never_clears_gives_up_and_other_errors_are_not_retried() {
        let tries = std::cell::Cell::new(0);
        let out: playbook_store::Result<()> = retry_busy(
            || {
                tries.set(tries.get() + 1);
                Err(busy())
            },
            Duration::from_millis(100),
        );
        assert!(out.is_err());
        assert!(tries.get() > 1, "a busy answer is retried before giving up");

        let tries = std::cell::Cell::new(0);
        let out: playbook_store::Result<()> = retry_busy(
            || {
                tries.set(tries.get() + 1);
                Err(playbook_store::Error::QueryReturnedNoRows)
            },
            Duration::from_secs(5),
        );
        assert!(out.is_err());
        assert_eq!(tries.get(), 1);
    }

    #[test]
    fn open_creates_the_usage_and_telemetry_tables_in_wal_mode() {
        let dir = scratch_dir("usage-open");
        let conn = open_db(&dir.join("usage.db")).unwrap();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();

        assert_eq!(
            tables,
            vec![
                "account_aliases",
                "account_assignment_rows",
                "account_assignments",
                "account_names",
                "machine",
                "meta",
                "session_account_spans",
                "session_accounts",
                "session_reconcile",
                "session_sources",
                "sync_cursors",
                "sync_state",
                "sync_watermarks",
                "telemetry_errors",
                "telemetry_metrics",
                "telemetry_requests",
                "telemetry_tools",
                "tool_invocation_events",
                "usage_backfill_files",
                "usage_events",
                "usage_events_remote",
                "usage_watermarks"
            ]
        );
        assert_eq!(mode, "wal");
        let _ = fs::remove_dir_all(dir);
    }

    fn event(id: &str, model: &str, stored_cost: f64) -> UsageEvent {
        UsageEvent {
            event_id: id.into(),
            timestamp: 100,
            session_id: "s".into(),
            account: "a".into(),
            model: model.into(),
            input_tokens: 1_000_000,
            cost_usd: stored_cost,
            ..UsageEvent::default()
        }
    }

    #[test]
    fn load_prices_from_tokens_and_ignores_the_stored_cost() {
        let dir = scratch_dir("usage-read-pricing");
        let conn = open_db(&dir.join("usage.db")).unwrap();
        upsert_usage_event(&conn, &event("a", "claude-sonnet-5", 99.0)).unwrap();
        upsert_usage_event(&conn, &event("b", "mystery-model", 99.0)).unwrap();

        let loaded = load_usage_events(&conn).unwrap();

        // Sonnet 5 input is $2 per million; the stored 99.0 is not used.
        assert!((loaded[0].cost_usd - 2.0).abs() < 1e-9 && !loaded[0].unpriced);
        assert_eq!((loaded[1].cost_usd, loaded[1].unpriced), (0.0, true));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_database_made_before_the_cache_tier_column_gains_it_with_zero() {
        let dir = scratch_dir("usage-old-schema");
        let path = dir.join("usage.db");
        fs::create_dir_all(&dir).unwrap();
        let old = Connection::open(&path).unwrap();
        old.execute_batch(
            "CREATE TABLE usage_events (
                event_id TEXT PRIMARY KEY, timestamp INTEGER NOT NULL,
                session_id TEXT NOT NULL, account TEXT NOT NULL, model TEXT NOT NULL,
                effort TEXT NOT NULL, repo TEXT NOT NULL, branch TEXT NOT NULL,
                input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,
                cache_creation_tokens INTEGER NOT NULL, cache_read_tokens INTEGER NOT NULL,
                cost_usd REAL NOT NULL CHECK(cost_usd >= 0));
             INSERT INTO usage_events VALUES
                ('old', 1, 's', 'a', 'claude-sonnet-5', '', 'r', 'b', 1, 2, 3, 4, 0.5);",
        )
        .unwrap();
        drop(old);

        let conn = open_db(&path).unwrap();
        let again = open_db(&path).unwrap();

        let loaded = load_usage_events(&conn).unwrap();
        assert_eq!(loaded[0].cache_creation_1h_tokens, 0);
        assert_eq!(loaded[0].cache_creation_tokens, 3);
        assert_eq!(
            loaded[0].agent, "claude-code",
            "old rows default to Claude Code"
        );
        assert_eq!(count_usage_events(&again).unwrap(), 1);
        let _ = fs::remove_dir_all(dir);
    }

    fn index_names(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'usage_events'
                 AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn an_older_database_gains_the_indexes_and_reopening_is_harmless() {
        let dir = scratch_dir("usage-old-indexes");
        let path = dir.join("usage.db");
        fs::create_dir_all(&dir).unwrap();
        let old = Connection::open(&path).unwrap();
        old.execute_batch(
            "CREATE TABLE usage_events (
                event_id TEXT PRIMARY KEY, timestamp INTEGER NOT NULL,
                session_id TEXT NOT NULL, account TEXT NOT NULL, model TEXT NOT NULL,
                effort TEXT NOT NULL, repo TEXT NOT NULL, branch TEXT NOT NULL,
                input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,
                cache_creation_tokens INTEGER NOT NULL, cache_read_tokens INTEGER NOT NULL,
                cost_usd REAL NOT NULL CHECK(cost_usd >= 0));
             INSERT INTO usage_events VALUES
                ('old', 1, 's', 'a', 'claude-sonnet-5', '', 'r', 'b', 1, 2, 3, 4, 0.5);",
        )
        .unwrap();
        assert!(index_names(&old).is_empty());
        drop(old);

        let conn = open_db(&path).unwrap();
        let expected = vec!["usage_events_by_session", "usage_events_by_time"];
        assert_eq!(index_names(&conn), expected);
        assert_eq!(index_names(&open_db(&path).unwrap()), expected);
        assert_eq!(count_usage_events(&conn).unwrap(), 1);
        let _ = fs::remove_dir_all(dir);
    }

    fn plan(conn: &Connection, tail: &str, params: &[&dyn playbook_store::ToSql]) -> String {
        let sql = format!("EXPLAIN QUERY PLAN SELECT {USAGE_COLUMNS} FROM usage_events {tail}");
        let mut stmt = conn.prepare(&sql).unwrap();
        let rows: Vec<String> = stmt
            .query_map(params, |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        rows.join(" | ")
    }

    #[test]
    fn every_loader_query_uses_an_index_instead_of_scanning_the_table() {
        let dir = scratch_dir("usage-plan");
        let conn = open_db(&dir.join("usage.db")).unwrap();
        for i in 0..50 {
            let mut e = event(&format!("e{i}"), "claude-sonnet-5", 0.0);
            e.timestamp = i;
            e.session_id = format!("s{}", i % 5);
            upsert_usage_event(&conn, &e).unwrap();
        }
        let ids = ["s1".to_string(), "s2".to_string()];
        let id_params: Vec<&dyn playbook_store::ToSql> = ids
            .iter()
            .map(|s| s as &dyn playbook_store::ToSql)
            .collect();

        let all = plan(&conn, TAIL_ALL, &[]);
        let since = plan(&conn, TAIL_SINCE, &[&10_i64]);
        let latest = plan(&conn, TAIL_LATEST, &[&5_i64, &None::<&str>]);
        let sessions = plan(&conn, &tail_of_sessions("?,?"), &id_params);

        assert!(all.contains("usage_events_by_time"), "{all}");
        assert!(since.contains("usage_events_by_time"), "{since}");
        assert!(latest.contains("usage_events_by_time"), "{latest}");
        assert!(sessions.contains("usage_events_by_session"), "{sessions}");
        for (name, text) in [("all", &all), ("since", &since), ("latest", &latest)] {
            assert!(!text.contains("TEMP B-TREE"), "{name} sorts: {text}");
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn watermark_defaults_to_zero_and_only_moves_when_raised() {
        let dir = scratch_dir("usage-watermark");
        let conn = open_db(&dir.join("usage.db")).unwrap();

        assert_eq!(get_watermark(&conn, "claude-code").unwrap(), 0);
        raise_watermark(&conn, "claude-code", 500).unwrap();
        assert_eq!(get_watermark(&conn, "claude-code").unwrap(), 500);
        assert_eq!(get_watermark(&conn, "other").unwrap(), 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn the_live_queries_filter_order_and_limit_as_documented() {
        let dir = scratch_dir("usage-live-queries");
        let conn = open_db(&dir.join("usage.db")).unwrap();
        for (id, ts, session) in [
            ("a", 100, "s1"),
            ("b", 200, "s2"),
            ("c", 300, "s3"),
            ("d", 400, "s1"),
        ] {
            let event = UsageEvent {
                event_id: id.into(),
                timestamp: ts,
                session_id: session.into(),
                model: "m".into(),
                ..UsageEvent::default()
            };
            upsert_usage_event(&conn, &event).unwrap();
        }
        let ids = |events: Vec<UsageEvent>| -> Vec<String> {
            events.into_iter().map(|e| e.event_id).collect()
        };

        assert_eq!(
            ids(load_usage_events_since(&conn, 200).unwrap()),
            ["b", "c", "d"]
        );
        assert_eq!(
            ids(load_usage_events_since(&conn, 401).unwrap()),
            Vec::<String>::new()
        );
        assert_eq!(
            ids(load_latest_usage_events(&conn, 2, None).unwrap()),
            ["d", "c"]
        );
        let wanted = vec!["s1".to_string(), "s3".to_string()];
        assert_eq!(
            ids(load_usage_events_of_sessions(&conn, &wanted).unwrap()),
            ["a", "c", "d"]
        );
        assert!(load_usage_events_of_sessions(&conn, &[])
            .unwrap()
            .is_empty());
        let _ = fs::remove_dir_all(dir);
    }
}
