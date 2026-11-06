// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Schema and connection layer for the gate-check database at the repo and worktree-scoped path under `$HOME/.config/playbook`, backed by `playbook_store` (the `bundled` feature compiles SQLite from source, so no system SQLite install is required).
//!
//! An earlier version used `libsql` (tokio-based, async). `gate check`
//! reliably segfaulted inside `sqlite3Close` on real musl release builds
//! (x86_64 and aarch64 both, confirmed via `gdb`), even after fixing every
//! runtime-lifecycle issue that could explain it: a `Connection`'s open,
//! use, and drop were kept inside one tokio runtime, and every `Rows`
//! cursor was drained to completion. Neither resolved it, and the crash
//! matched an open, maintainer-filed bug in `libsql`'s own connection
//! lifecycle code. `playbook_store` needs none of that: it is fully synchronous,
//! is the de facto standard Rust SQLite binding, and this binary has no
//! other reason to be async.

use crate::common::atomic::{with_dir_lock_aged, LONG_LOCK_STALE_AGE};
use crate::common::sqlite::{retry_busy, OPEN_BUDGET};
use playbook_store::OptionalExtension;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const SCHEMA_SQL: &str = "CREATE TABLE IF NOT EXISTS gate_phases (
    plan_slug TEXT NOT NULL,
    phase TEXT NOT NULL,
    verdict TEXT NOT NULL CHECK(verdict IN ('PASS','FAIL','WARN','INCONCLUSIVE','STUCK')),
    evidence TEXT NOT NULL,
    command TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    source_hash TEXT,
    PRIMARY KEY (plan_slug, phase)
) WITHOUT ROWID";

/// Append-only round history: one row per `gate record` call of a revise and
/// re-run series, so the iteration count survives the upsert on `gate_phases`.
const ROUNDS_SQL: &str = "CREATE TABLE IF NOT EXISTS gate_rounds (
    plan_slug TEXT NOT NULL,
    phase TEXT NOT NULL,
    iteration INTEGER NOT NULL,
    verdict TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (plan_slug, phase, iteration)
) WITHOUT ROWID";

/// One row of `gate_phases`. Returned whole rather than just the verdict:
/// a single-row lookup by primary key costs nothing extra to carry the full
/// record, and a future consumer (e.g. a `gate show`) can use it as-is.
pub struct GatePhaseRow {
    pub verdict: String,
    pub evidence: String,
    pub command: String,
    pub recorded_at: String,
    pub source_hash: Option<String>,
}

/// Adds `gate_phases.source_hash` to a database created before this column
/// existed, so an old database file keeps opening instead of erroring.
fn ensure_source_hash_column(conn: &playbook_store::Connection) -> Result<(), String> {
    let has_column: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('gate_phases') WHERE name = 'source_hash')",
            [],
            |row| row.get(0),
        )
        .map_err(|e| format!("failed to inspect gate_phases schema: {e}"))?;
    if !has_column {
        if let Err(e) = conn.execute_batch("ALTER TABLE gate_phases ADD COLUMN source_hash TEXT") {
            // Two processes can both see the column absent and race the
            // ALTER; the loser's "duplicate column" failure means it exists.
            if !e.to_string().contains("duplicate column") {
                return Err(format!("failed to add source_hash column: {e}"));
            }
        }
    }
    Ok(())
}

/// Rebuilds a `gate_phases` table created before STUCK existed, whose CHECK
/// constraint rejects it. SQLite cannot alter a CHECK in place, so the rows are
/// copied into a fresh table. A table with no verdict CHECK needs nothing.
fn ensure_stuck_verdict(conn: &playbook_store::Connection) -> Result<(), String> {
    let needs_rebuild = |conn: &playbook_store::Connection| -> Result<bool, String> {
        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'gate_phases'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("failed to inspect gate_phases schema: {e}"))?;
        Ok(sql.contains("CHECK(verdict") && !sql.contains("'STUCK'"))
    };
    if !needs_rebuild(conn)? {
        return Ok(());
    }
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| format!("failed to lock gate_phases for migration: {e}"))?;
    let rebuilt = needs_rebuild(conn).and_then(|again| {
        if !again {
            return Ok(());
        }
        conn.execute_batch(&format!(
            "ALTER TABLE gate_phases RENAME TO gate_phases_old;
             {SCHEMA_SQL};
             INSERT INTO gate_phases SELECT plan_slug, phase, verdict, evidence, command, \
                 recorded_at, source_hash FROM gate_phases_old;
             DROP TABLE gate_phases_old;"
        ))
        .map_err(|e| format!("failed to add STUCK to gate_phases: {e}"))
    });
    match rebuilt {
        Ok(()) => conn
            .execute_batch("COMMIT")
            .map_err(|e| format!("failed to commit gate_phases migration: {e}")),
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// How the move merges the two gate tables when an older playbook wrote the
/// old file again: a phase row with the later `recorded_at` wins, and a round
/// series for one plan and phase is taken whole from one file.
const GATE_RULES: &[(&str, crate::common::data::migrate::Rule)] = &[
    (
        "gate_phases",
        crate::common::data::migrate::Rule::Newer("recorded_at"),
    ),
    (
        "gate_rounds",
        crate::common::data::migrate::Rule::Series(&["plan_slug", "phase"]),
    ),
];

/// The gate database of the current worktree, inside the data folder. First
/// completes the legacy repo-local move into `dest_base`, then moves
/// `dest_base/state.db` (where earlier versions kept it) to
/// `data/gate/<owner>/<repo>/<worktree-id>.db`.
///
/// While the old file cannot be moved (another process holds it, a lock is
/// pending, the move failed) the old path is returned for this call, so a
/// verdict recorded earlier is never hidden behind an empty new file. A
/// corrupt old file is left in place and the new path is used. A repo whose
/// name has no safe data folder path keeps using the old path.
pub fn resolve_db_path(repo_root: &Path, dest_base: &Path) -> Result<PathBuf, String> {
    use crate::common::data::migrate::{self, Outcome};
    migrate_legacy_repo_local(repo_root, dest_base)?;
    let old = dest_base.join("state.db");
    let Some(new) = crate::common::data::scoped_db("gate") else {
        return Ok(old);
    };
    let step = migrate::Move {
        old: old.clone(),
        new: new.clone(),
        policy: migrate::Policy::Merge(GATE_RULES),
    };
    match migrate::migrate_with_notice(&step, "gate") {
        Ok(Outcome::Corrupt(_)) => Ok(new),
        Ok(_) if !old.is_file() => Ok(new),
        Ok(_) => Ok(old),
        Err(e) => {
            migrate::notice(&format!(
                "playbook: the gate database move is deferred ({e})"
            ));
            Ok(old)
        }
    }
}

/// Open (creating if missing) the SQLite database at `path`, ensuring the
/// `gate_phases` schema and pragmas are in place.
pub fn open_db(path: &Path) -> Result<playbook_store::Connection, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create directory {}: {e}", parent.display()))?;
    }

    let conn = playbook_store::Connection::open(path)
        .map_err(|e| format!("failed to open database at {}: {e}", path.display()))?;

    // busy_timeout must be set BEFORE any statement that can contend for
    // the write lock (the schema creation below, and future callers
    // opening the same fresh file concurrently): a connection's default
    // busy timeout is 0, so an SQLITE_BUSY hit before this is set fails
    // immediately instead of retrying. `commands/plan.md` fires two
    // `gate record` processes in parallel (Phase 2 and Phase 3), so this
    // ordering is load-bearing, not cosmetic.
    conn.busy_timeout(Duration::from_millis(5000))
        .map_err(|e| format!("failed to set busy_timeout: {e}"))?;
    // The WAL switch skips the busy handler, so a parallel opener gets BUSY
    // at once; retry it for a bounded time.
    retry_busy(
        || conn.pragma_update(None, "journal_mode", "WAL"),
        OPEN_BUDGET,
    )
    .map_err(|e| format!("failed to set journal_mode=WAL: {e}"))?;
    retry_busy(|| conn.execute_batch(SCHEMA_SQL), OPEN_BUDGET)
        .map_err(|e| format!("failed to create gate_phases schema: {e}"))?;
    ensure_source_hash_column(&conn)?;
    ensure_stuck_verdict(&conn)?;
    retry_busy(|| conn.execute_batch(ROUNDS_SQL), OPEN_BUDGET)
        .map_err(|e| format!("failed to create gate_rounds schema: {e}"))?;

    Ok(conn)
}

/// Insert or replace the `(plan_slug, phase)` row with the given values.
#[allow(clippy::too_many_arguments)]
pub fn upsert_phase(
    conn: &playbook_store::Connection,
    plan_slug: &str,
    phase: &str,
    verdict: &str,
    evidence: &str,
    command: &str,
    recorded_at: &str,
    source_hash: &str,
) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO gate_phases \
         (plan_slug, phase, verdict, evidence, command, recorded_at, source_hash) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        playbook_store::params![
            plan_slug,
            phase,
            verdict,
            evidence,
            command,
            recorded_at,
            source_hash
        ],
    )
    .map_err(|e| format!("failed to upsert gate phase {plan_slug}/{phase}: {e}"))?;
    Ok(())
}

/// Append one round for `(plan_slug, phase)` and return its iteration number.
/// A series continues while the previous round ended FAIL or INCONCLUSIVE (a
/// revise and re-run); after PASS, WARN or STUCK the next record starts a new
/// series at 1 and clears the old rounds.
pub fn record_round(
    conn: &playbook_store::Connection,
    plan_slug: &str,
    phase: &str,
    verdict: &str,
    recorded_at: &str,
) -> Result<i64, String> {
    let ctx = |e: playbook_store::Error| format!("failed to record round {plan_slug}/{phase}: {e}");
    let tx = conn.unchecked_transaction().map_err(ctx)?;
    let last: Option<(i64, String)> = tx
        .query_row(
            "SELECT iteration, verdict FROM gate_rounds WHERE plan_slug = ?1 AND phase = ?2 \
             ORDER BY iteration DESC LIMIT 1",
            playbook_store::params![plan_slug, phase],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(ctx)?;
    let iteration = match last {
        Some((n, v)) if v == "FAIL" || v == "INCONCLUSIVE" => n + 1,
        Some(_) => {
            tx.execute(
                "DELETE FROM gate_rounds WHERE plan_slug = ?1 AND phase = ?2",
                playbook_store::params![plan_slug, phase],
            )
            .map_err(ctx)?;
            1
        }
        None => 1,
    };
    tx.execute(
        "INSERT INTO gate_rounds (plan_slug, phase, iteration, verdict, recorded_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        playbook_store::params![plan_slug, phase, iteration, verdict, recorded_at],
    )
    .map_err(ctx)?;
    tx.commit().map_err(ctx)?;
    Ok(iteration)
}

/// Look up the row for `(plan_slug, phase)`, or `None` if it has never been
/// recorded.
pub fn query_phase(
    conn: &playbook_store::Connection,
    plan_slug: &str,
    phase: &str,
) -> Result<Option<GatePhaseRow>, String> {
    conn.query_row(
        "SELECT verdict, evidence, command, recorded_at, source_hash FROM gate_phases \
         WHERE plan_slug = ?1 AND phase = ?2",
        playbook_store::params![plan_slug, phase],
        |row| {
            Ok(GatePhaseRow {
                verdict: row.get(0)?,
                evidence: row.get(1)?,
                command: row.get(2)?,
                recorded_at: row.get(3)?,
                source_hash: row.get(4)?,
            })
        },
    )
    .optional()
    .map_err(|e| format!("failed to query gate phase {plan_slug}/{phase}: {e}"))
}

/// Marks a completed [`migrate_legacy_repo_local`] run; absent means resume.
const MIGRATION_SENTINEL: &str = ".migration-complete";

/// The legacy items a pre-ADR-0012 checkout held under its own `.claude/`.
const LEGACY_ITEMS: &[&str] = &["state.db", "plans", "designs", "implement", "worktrees"];

/// Moves legacy `.claude/{state.db,plans,designs,implement,worktrees}` items
/// under `repo_root` to `dest_base`, once, locked against plan.md's two
/// parallel `gate record` calls.
pub fn migrate_legacy_repo_local(repo_root: &Path, dest_base: &Path) -> Result<(), String> {
    let legacy_root = repo_root.join(".claude");
    let sentinel = dest_base.join(MIGRATION_SENTINEL);
    if sentinel.is_file() {
        return Ok(());
    }
    // Nothing legacy present means `.claude/` may not even exist, so skip
    // locking rather than retry into a lock path with a missing parent.
    if !LEGACY_ITEMS
        .iter()
        .any(|name| legacy_root.join(name).exists())
    {
        return Ok(());
    }

    let lock_path = legacy_root.join(".migrate.lock");
    let (acquired, result) = with_dir_lock_aged(
        &lock_path,
        50,
        Duration::from_millis(100),
        LONG_LOCK_STALE_AGE,
        || migrate_legacy_repo_local_locked(&legacy_root, dest_base, &sentinel),
    );
    if acquired {
        let _ = fs::remove_dir(&lock_path);
        // Harmless no-op if anything else still lives under `.claude`.
        let _ = fs::remove_dir(&legacy_root);
    }
    if !acquired {
        if sentinel.is_file() {
            return Ok(());
        }
        return Err(format!(
            "legacy migration to {} is already in progress in another process; retry shortly",
            dest_base.display()
        ));
    }
    result
}

/// The actual migration, run once `migrate_legacy_repo_local` holds its lock.
fn migrate_legacy_repo_local_locked(
    legacy_root: &Path,
    dest_base: &Path,
    sentinel: &Path,
) -> Result<(), String> {
    if sentinel.is_file() {
        return Ok(());
    }

    let present: Vec<&str> = LEGACY_ITEMS
        .iter()
        .copied()
        .filter(|name| legacy_root.join(name).exists())
        .collect();
    if present.is_empty() {
        return Ok(());
    }

    fs::create_dir_all(dest_base)
        .map_err(|e| format!("failed to create {}: {e}", dest_base.display()))?;

    let mut files = Vec::new();
    for name in &present {
        let item = legacy_root.join(name);
        if item.is_dir() {
            collect_relative_files(&item, Path::new(name), &mut files);
        } else {
            files.push(PathBuf::from(name));
        }
    }

    copy_all(legacy_root, dest_base, &files).map_err(|e| {
        format!(
            "legacy migration copy to {} failed, the original is untouched: {e}",
            dest_base.display()
        )
    })?;

    if !all_copied_and_verified(legacy_root, dest_base, &files) {
        return Err(format!(
            "legacy migration verification failed after copying to {}, the original is untouched",
            dest_base.display()
        ));
    }

    write_migration_sentinel(sentinel).map_err(|e| {
        format!("legacy migration copy verified but could not write completion marker: {e}")
    })?;

    // Removed only now that the destination is verified complete. The
    // lock dir is still present, so the wrapper does the final rmdir.
    for name in &present {
        remove_legacy_item(&legacy_root.join(name));
    }
    Ok(())
}

/// Every regular file under `dir`, recursively, relative to `prefix`.
fn collect_relative_files(dir: &Path, prefix: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let rel = prefix.join(entry.file_name());
        if path.is_dir() {
            collect_relative_files(&path, &rel, out);
        } else {
            out.push(rel);
        }
    }
}

/// Copies each of `files` from `old_root` to `new_root`, stopping at the first failure so a partial copy is never marked done.
fn copy_all(old_root: &Path, new_root: &Path, files: &[PathBuf]) -> std::io::Result<()> {
    for rel in files {
        let dest = new_root.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(old_root.join(rel), &dest)?;
    }
    Ok(())
}

/// Every source file exists at the destination with a matching size.
fn all_copied_and_verified(old_root: &Path, new_root: &Path, files: &[PathBuf]) -> bool {
    files.iter().all(|rel| {
        let source_len = fs::metadata(old_root.join(rel)).ok().map(|m| m.len());
        let dest_len = fs::metadata(new_root.join(rel)).ok().map(|m| m.len());
        source_len.is_some() && source_len == dest_len
    })
}

fn write_migration_sentinel(sentinel: &Path) -> std::io::Result<()> {
    if let Some(parent) = sentinel.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(sentinel, "migrated\n")
}

fn remove_legacy_item(path: &Path) {
    if path.is_dir() {
        let _ = fs::remove_dir_all(path);
    } else {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;
    use crate::testing::write_file;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    /// Count rows for a `(plan_slug, phase)` pair via a fresh connection to
    /// `path`, proving `INSERT OR REPLACE` never leaves a duplicate behind.
    fn count_rows(path: &Path, plan_slug: &str, phase: &str) -> i64 {
        let conn = open_db(path).expect("open_db for count_rows");
        conn.query_row(
            "SELECT COUNT(*) FROM gate_phases WHERE plan_slug = ?1 AND phase = ?2",
            playbook_store::params![plan_slug, phase],
            |row| row.get(0),
        )
        .expect("count query")
    }

    #[test]
    fn open_db_sets_busy_timeout_before_returning() {
        // Arrange
        let dir = scratch_dir("busy-timeout");
        let path = dir.join("state.db");

        // Act
        let conn = open_db(&path).expect("open");
        let value: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .expect("busy_timeout query");

        // Assert: pinned at the value open_db sets, proving the call ran
        // (not the SQLite default of 0) and that it does not get shadowed
        // by a later statement in open_db's own body.
        assert_eq!(value, 5000);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_db_creates_schema_on_fresh_path() {
        // Arrange
        let dir = scratch_dir("open-fresh");
        let path = dir.join("state.db");

        // Act
        let result = open_db(&path);

        // Assert
        assert!(result.is_ok(), "expected Ok, got {:?}", result.err());
        assert!(path.is_file(), "database file should exist at {path:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_db_reopen_is_idempotent_and_keeps_data() {
        // Arrange
        let dir = scratch_dir("open-reopen");
        let path = dir.join("state.db");
        let conn = open_db(&path).expect("first open");
        upsert_phase(
            &conn,
            "plan-a",
            "spec",
            "PASS",
            "ev",
            "cmd",
            "2026-01-01T00:00:00Z",
            "test-hash",
        )
        .expect("upsert before reopen");
        drop(conn);

        // Act
        let reopened = open_db(&path);

        // Assert
        assert!(
            reopened.is_ok(),
            "reopen should not error: {:?}",
            reopened.err()
        );
        let row = query_phase(&reopened.unwrap(), "plan-a", "spec")
            .expect("query after reopen")
            .expect("row should survive reopen");
        assert_eq!(row.verdict, "PASS");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn upsert_phase_twice_keeps_a_single_row_with_latest_values() {
        // Arrange
        let dir = scratch_dir("upsert-twice");
        let path = dir.join("state.db");
        let conn = open_db(&path).expect("open");
        upsert_phase(
            &conn,
            "plan-a",
            "spec",
            "WARN",
            "first evidence",
            "cmd-1",
            "2026-01-01T00:00:00Z",
            "test-hash",
        )
        .expect("first upsert");

        // Act
        upsert_phase(
            &conn,
            "plan-a",
            "spec",
            "PASS",
            "second evidence",
            "cmd-2",
            "2026-01-02T00:00:00Z",
            "test-hash",
        )
        .expect("second upsert");

        // Assert
        let row = query_phase(&conn, "plan-a", "spec")
            .expect("query")
            .expect("row should exist");
        assert_eq!(row.verdict, "PASS");
        assert_eq!(row.evidence, "second evidence");
        assert_eq!(count_rows(&path, "plan-a", "spec"), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn upsert_phase_rejects_invalid_verdict() {
        // Arrange
        let dir = scratch_dir("bad-verdict");
        let path = dir.join("state.db");
        let conn = open_db(&path).expect("open");

        // Act
        let result = upsert_phase(
            &conn,
            "plan-a",
            "spec",
            "NOPE",
            "ev",
            "cmd",
            "2026-01-01T00:00:00Z",
            "test-hash",
        );

        // Assert
        assert!(
            result.is_err(),
            "an invalid verdict should be rejected, got {result:?}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn query_phase_never_bleeds_across_plan_slugs() {
        // Arrange
        let dir = scratch_dir("cross-plan");
        let path = dir.join("state.db");
        let conn = open_db(&path).expect("open");
        upsert_phase(
            &conn,
            "plan-a",
            "spec",
            "PASS",
            "a-evidence",
            "a-cmd",
            "2026-01-01T00:00:00Z",
            "test-hash",
        )
        .expect("upsert plan-a");
        upsert_phase(
            &conn,
            "plan-b",
            "spec",
            "FAIL",
            "b-evidence",
            "b-cmd",
            "2026-01-02T00:00:00Z",
            "test-hash",
        )
        .expect("upsert plan-b");

        // Act
        let row_a = query_phase(&conn, "plan-a", "spec")
            .expect("query plan-a")
            .expect("row a");
        let row_b = query_phase(&conn, "plan-b", "spec")
            .expect("query plan-b")
            .expect("row b");

        // Assert
        assert_eq!(row_a.verdict, "PASS");
        assert_eq!(row_b.verdict, "FAIL");
        assert_ne!(row_a.evidence, row_b.evidence);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn upsert_phase_stores_and_query_phase_returns_source_hash() {
        // Arrange
        let dir = scratch_dir("source-hash-roundtrip");
        let path = dir.join("state.db");
        let conn = open_db(&path).expect("open");

        // Act
        upsert_phase(
            &conn,
            "plan-a",
            "spec",
            "PASS",
            "ev",
            "cmd",
            "2026-01-01T00:00:00Z",
            "abc123hash",
        )
        .expect("upsert with source_hash");
        let row = query_phase(&conn, "plan-a", "spec")
            .expect("query")
            .expect("row should exist");

        // Assert
        assert_eq!(row.source_hash, Some("abc123hash".to_string()));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn query_phase_returns_none_source_hash_for_pre_migration_row() {
        // Arrange: a raw insert that omits source_hash, simulating a row
        // written before the column existed.
        let dir = scratch_dir("source-hash-legacy-row");
        let path = dir.join("state.db");
        let conn = open_db(&path).expect("open");
        conn.execute(
            "INSERT INTO gate_phases \
             (plan_slug, phase, verdict, evidence, command, recorded_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            playbook_store::params![
                "plan-a",
                "spec",
                "PASS",
                "ev",
                "cmd",
                "2026-01-01T00:00:00Z"
            ],
        )
        .expect("raw insert without source_hash");

        // Act
        let row = query_phase(&conn, "plan-a", "spec")
            .expect("query")
            .expect("row should exist");

        // Assert
        assert_eq!(row.source_hash, None);

        let _ = fs::remove_dir_all(&dir);
    }

    /// Two connections to the same legacy database both see the column
    /// absent; the second's own `ALTER TABLE` fails once the first commits it.
    #[test]
    fn ensure_source_hash_column_tolerates_concurrent_alter_race() {
        // Arrange: a legacy table (no source_hash column), plus conn_a
        // holding the write lock the way its own in-progress ALTER would.
        let dir = scratch_dir("source-hash-alter-race");
        fs::create_dir_all(&dir).expect("create scratch dir");
        let path = dir.join("state.db");
        let conn_a = playbook_store::Connection::open(&path).expect("open conn_a");
        conn_a
            .execute_batch(
                "CREATE TABLE gate_phases (
                    plan_slug TEXT NOT NULL,
                    phase TEXT NOT NULL,
                    verdict TEXT NOT NULL,
                    evidence TEXT NOT NULL,
                    command TEXT NOT NULL,
                    recorded_at TEXT NOT NULL,
                    PRIMARY KEY (plan_slug, phase)
                )",
            )
            .expect("create legacy schema");
        conn_a
            .execute_batch("BEGIN IMMEDIATE")
            .expect("begin immediate on conn_a");

        let conn_b = playbook_store::Connection::open(&path).expect("open conn_b");
        conn_b
            .busy_timeout(Duration::from_millis(2000))
            .expect("busy_timeout on conn_b");
        let handle = std::thread::spawn(move || ensure_source_hash_column(&conn_b));

        // Let conn_b's thread reach its own ALTER and start waiting on
        // conn_a's write lock before conn_a's ALTER commits underneath it.
        std::thread::sleep(Duration::from_millis(200));
        conn_a
            .execute_batch("ALTER TABLE gate_phases ADD COLUMN source_hash TEXT")
            .expect("conn_a adds the column first");
        conn_a.execute_batch("COMMIT").expect("commit conn_a");

        // Act
        let result = handle.join().expect("conn_b thread should not panic");

        // Assert: conn_b's own ALTER fails with duplicate column, and that
        // failure must be tolerated as success rather than propagated.
        assert!(result.is_ok(), "expected Ok, got {:?}", result.err());

        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_stale_migrate_lock_from_a_killed_process_is_cleared() {
        let base = scratch_dir("legacy-migrate-stale-lock");
        let repo_root = base.join("repo");
        let legacy = repo_root.join(".claude");
        write_file(&legacy.join("state.db"), "sqlite bytes");
        let lock = legacy.join(".migrate.lock");
        fs::create_dir_all(&lock).unwrap();
        let past = std::time::SystemTime::now() - LONG_LOCK_STALE_AGE * 3;
        fs::File::open(&lock)
            .and_then(|h| h.set_modified(past))
            .unwrap();
        let dest_base = base.join("dest");

        let result = migrate_legacy_repo_local(&repo_root, &dest_base);

        assert!(result.is_ok(), "{:?}", result.err());
        assert!(dest_base.join(MIGRATION_SENTINEL).is_file());
        assert!(!lock.exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn legacy_migration_moves_state_db_plans_designs_implement_worktrees() {
        // Arrange: every legacy item present, `plans`/`designs` holding real
        // nested content, matching the Done-When's content-preserving claim.
        let base = scratch_dir("legacy-migrate-full-set");
        let repo_root = base.join("repo");
        let legacy = repo_root.join(".claude");
        write_file(&legacy.join("state.db"), "sqlite bytes");
        write_file(&legacy.join("plans").join("topic.md"), "plan content");
        write_file(&legacy.join("designs").join("topic.md"), "design content");
        write_file(&legacy.join("implement").join("topic.progress.md"), "wip");
        write_file(&legacy.join("worktrees").join(".keep"), "");
        let dest_base = base.join("dest");

        // Act
        let result = migrate_legacy_repo_local(&repo_root, &dest_base);

        // Assert
        assert!(result.is_ok(), "expected Ok, got {:?}", result.err());
        assert_eq!(
            fs::read_to_string(dest_base.join("state.db")).unwrap(),
            "sqlite bytes"
        );
        assert_eq!(
            fs::read_to_string(dest_base.join("plans").join("topic.md")).unwrap(),
            "plan content"
        );
        assert_eq!(
            fs::read_to_string(dest_base.join("designs").join("topic.md")).unwrap(),
            "design content"
        );
        assert_eq!(
            fs::read_to_string(dest_base.join("implement").join("topic.progress.md")).unwrap(),
            "wip"
        );
        assert!(dest_base.join(MIGRATION_SENTINEL).is_file());
        assert!(
            !legacy.exists(),
            "the legacy .claude dir's migrated items should be gone"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn legacy_migration_is_idempotent_second_call_finds_nothing() {
        // Arrange: a sentinel already present, plus stray content on both
        // sides that a no-op second call must not touch.
        let base = scratch_dir("legacy-migrate-idempotent");
        let repo_root = base.join("repo");
        let legacy = repo_root.join(".claude");
        write_file(&legacy.join("state.db"), "leftover legacy bytes");
        let dest_base = base.join("dest");
        write_file(&dest_base.join("state.db"), "already migrated bytes");
        write_file(&dest_base.join(MIGRATION_SENTINEL), "migrated\n");

        // Act
        let result = migrate_legacy_repo_local(&repo_root, &dest_base);

        // Assert
        assert!(result.is_ok(), "expected Ok, got {:?}", result.err());
        assert_eq!(
            fs::read_to_string(legacy.join("state.db")).unwrap(),
            "leftover legacy bytes",
            "a no-op second call must not touch the untouched legacy leftovers"
        );
        assert_eq!(
            fs::read_to_string(dest_base.join("state.db")).unwrap(),
            "already migrated bytes"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn legacy_migration_leaves_source_untouched_on_simulated_failure() {
        // Arrange: a read-only destination so the very first copy write
        // fails, simulating a crash partway through.
        let base = scratch_dir("legacy-migrate-crash");
        let repo_root = base.join("repo");
        let legacy = repo_root.join(".claude");
        write_file(&legacy.join("state.db"), "must survive a crash");
        let dest_base = base.join("dest");
        fs::create_dir_all(&dest_base).unwrap();
        fs::set_permissions(&dest_base, fs::Permissions::from_mode(0o555)).unwrap();
        let probe = dest_base.join(".write-probe");
        let permissions_are_enforced = fs::write(&probe, "x").is_err();
        let _ = fs::set_permissions(&dest_base, fs::Permissions::from_mode(0o755));
        let _ = fs::remove_file(&probe);
        if !permissions_are_enforced {
            eprintln!(
                "skipping legacy_migration_leaves_source_untouched_on_simulated_failure: \
                 running as a user that bypasses directory permissions"
            );
            return;
        }
        fs::set_permissions(&dest_base, fs::Permissions::from_mode(0o555)).unwrap();

        // Act
        let result = migrate_legacy_repo_local(&repo_root, &dest_base);

        // Assert
        fs::set_permissions(&dest_base, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result.is_err(), "expected Err on a simulated write failure");
        assert_eq!(
            fs::read_to_string(legacy.join("state.db")).unwrap(),
            "must survive a crash"
        );
        assert!(legacy.join("state.db").exists());
        assert!(!dest_base.join(MIGRATION_SENTINEL).exists());

        let _ = fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn legacy_migration_resumes_correctly_after_simulated_kill_mid_copy() {
        // Arrange: `plans/` partially copied already (one of two files), no
        // sentinel, simulating a process killed mid-copy on a prior attempt.
        let base = scratch_dir("legacy-migrate-resume");
        let repo_root = base.join("repo");
        let legacy = repo_root.join(".claude");
        write_file(&legacy.join("plans").join("a.md"), "plan a content");
        write_file(&legacy.join("plans").join("b.md"), "plan b content");
        let dest_base = base.join("dest");
        write_file(&dest_base.join("plans").join("a.md"), "plan a content");

        // Act
        let result = migrate_legacy_repo_local(&repo_root, &dest_base);

        // Assert
        assert!(result.is_ok(), "expected Ok, got {:?}", result.err());
        assert_eq!(
            fs::read_to_string(dest_base.join("plans").join("a.md")).unwrap(),
            "plan a content"
        );
        assert_eq!(
            fs::read_to_string(dest_base.join("plans").join("b.md")).unwrap(),
            "plan b content"
        );
        assert!(dest_base.join(MIGRATION_SENTINEL).is_file());
        assert!(
            !legacy.exists(),
            "a completed resume should remove the original"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn legacy_migration_is_a_noop_when_nothing_legacy_exists() {
        // Arrange: a fresh checkout with no `.claude` at all.
        let base = scratch_dir("legacy-migrate-fresh");
        let repo_root = base.join("repo");
        fs::create_dir_all(&repo_root).unwrap();
        let dest_base = base.join("dest");

        // Act
        let result = migrate_legacy_repo_local(&repo_root, &dest_base);

        // Assert
        assert!(result.is_ok(), "expected Ok, got {:?}", result.err());
        assert!(!dest_base.exists());

        let _ = fs::remove_dir_all(&base);
    }
}
