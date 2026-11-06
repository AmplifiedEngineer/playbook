// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The usage store: one global SQLite database (not repo-scoped, since spend
//! is only meaningful as a total across projects). Connection setup mirrors
//! `gate::db::open_db`: the terminal view reads while `ingest` writes from
//! other processes, which is what WAL mode and `busy_timeout` are for.

use super::{ToolInvocationEvent, ToolKind, UsageEvent};
use crate::util::atomic::{ensure_private_dir, make_db_file_private};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::time::Duration;

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
    mut op: impl FnMut() -> rusqlite::Result<T>,
    budget: Duration,
) -> rusqlite::Result<T> {
    let start = std::time::Instant::now();
    loop {
        match op() {
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::DatabaseBusy && start.elapsed() < budget =>
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
    retry_busy(
        || conn.execute_batch(super::account_store::SCHEMA_SQL),
        BUSY_TIMEOUT,
    )
    .map_err(|e| format!("failed to create account names table: {e}"))?;
    retry_busy(
        || conn.execute_batch(super::account_move::SCHEMA_SQL),
        BUSY_TIMEOUT,
    )
    .map_err(|e| format!("failed to create account move tables: {e}"))?;
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
        "INSERT INTO usage_events (event_id, timestamp, session_id, account, model, effort,
            repo, branch, input_tokens, output_tokens, cache_creation_tokens,
            cache_read_tokens, cost_usd, cache_creation_1h_tokens, agent)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(event_id) DO UPDATE SET
            input_tokens = excluded.input_tokens,
            output_tokens = excluded.output_tokens,
            cache_creation_tokens = excluded.cache_creation_tokens,
            cache_read_tokens = excluded.cache_read_tokens,
            cost_usd = excluded.cost_usd,
            cache_creation_1h_tokens = excluded.cache_creation_1h_tokens
         WHERE excluded.output_tokens > usage_events.output_tokens",
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
        rusqlite::params![path, mtime],
    )
    .map(|_| ())
    .map_err(|e| format!("failed to record the backfill progress: {e}"))
}

pub fn clear_backfilled_files(conn: &Connection) -> Result<(), String> {
    conn.execute("DELETE FROM usage_backfill_files", [])
        .map(|_| ())
        .map_err(|e| format!("failed to clear the backfill progress: {e}"))
}

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
        "INSERT INTO tool_invocation_events (event_id, timestamp, session_id, account, kind, name)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
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
    cache_creation_1h_tokens, agent";

const TAIL_ALL: &str = "ORDER BY timestamp, event_id";
const TAIL_SINCE: &str = "WHERE timestamp >= ?1 ORDER BY timestamp, event_id";
const TAIL_LATEST: &str = "ORDER BY timestamp DESC, event_id DESC LIMIT ?1";

fn tail_of_sessions(marks: &str) -> String {
    format!("WHERE session_id IN ({marks}) ORDER BY timestamp, event_id")
}

fn query_usage(
    conn: &Connection,
    tail: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Vec<UsageEvent>, String> {
    let mut stmt = conn
        .prepare(&format!("SELECT {USAGE_COLUMNS} FROM usage_events {tail}"))
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
    query_usage(conn, TAIL_ALL, &[])
}

/// Events at or after `since` (epoch seconds), oldest first.
pub fn load_usage_events_since(conn: &Connection, since: i64) -> Result<Vec<UsageEvent>, String> {
    query_usage(conn, TAIL_SINCE, &[&since])
}

/// The newest `limit` events, newest first.
pub fn load_latest_usage_events(conn: &Connection, limit: i64) -> Result<Vec<UsageEvent>, String> {
    query_usage(conn, TAIL_LATEST, &[&limit])
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
    let params: Vec<&dyn rusqlite::ToSql> =
        sessions.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    query_usage(conn, &tail_of_sessions(&marks), &params)
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

    fn busy() -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
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
        let out: rusqlite::Result<()> = retry_busy(
            || {
                tries.set(tries.get() + 1);
                Err(busy())
            },
            Duration::from_millis(100),
        );
        assert!(out.is_err());
        assert!(tries.get() > 1, "a busy answer is retried before giving up");

        let tries = std::cell::Cell::new(0);
        let out: rusqlite::Result<()> = retry_busy(
            || {
                tries.set(tries.get() + 1);
                Err(rusqlite::Error::QueryReturnedNoRows)
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
                "session_reconcile",
                "session_sources",
                "telemetry_errors",
                "telemetry_metrics",
                "telemetry_requests",
                "telemetry_tools",
                "tool_invocation_events",
                "usage_backfill_files",
                "usage_events",
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

    fn plan(conn: &Connection, tail: &str, params: &[&dyn rusqlite::ToSql]) -> String {
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
        let id_params: Vec<&dyn rusqlite::ToSql> =
            ids.iter().map(|s| s as &dyn rusqlite::ToSql).collect();

        let all = plan(&conn, TAIL_ALL, &[]);
        let since = plan(&conn, TAIL_SINCE, &[&10_i64]);
        let latest = plan(&conn, TAIL_LATEST, &[&5_i64]);
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
        assert_eq!(ids(load_latest_usage_events(&conn, 2).unwrap()), ["d", "c"]);
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
