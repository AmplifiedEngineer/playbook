// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `sessions.db` store: one row per agent session, with an append only
//! event log and an audit table for lifecycle actions. Every writer is one
//! statement (an upsert) or a `BEGIN IMMEDIATE` transaction: a deferred
//! transaction that upgrades from read to write fails at once with
//! SQLITE_BUSY in WAL mode, and `busy_timeout` does not retry it.

use crate::common::atomic::make_db_file_private;
use crate::common::sqlite::{retry_busy, OPEN_BUDGET};
use playbook_store::{params, schema, Connection};
use std::path::Path;
use std::time::Duration;

/// The `sessions.db` layout this build writes.
pub(crate) const SCHEMA_VERSION: i64 = 1;

/// How long a hook waits for the write lock before giving up.
const HOOK_BUSY_TIMEOUT_MS: u64 = 2000;

/// The name `sessions.db` carries in version errors.
const DB_NAME: &str = "sessions.db";

/// Times are Unix seconds. `meta` is created by `schema::check_and_stamp`.
const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS sessions (
    session_id TEXT PRIMARY KEY,
    agent TEXT,
    cwd TEXT,
    repo TEXT,
    branch TEXT,
    worktree TEXT,
    convention TEXT,
    pr_number INTEGER,
    pr_kind TEXT,
    pr_head_oid TEXT,
    pr_base TEXT,
    pr_checked_at INTEGER,
    started_at INTEGER,
    last_prompt_at INTEGER,
    last_stop_at INTEGER,
    waiting_since INTEGER,
    waiting_after_prompt INTEGER NOT NULL DEFAULT 0,
    ended_at INTEGER,
    end_reason TEXT,
    crashed_at INTEGER,
    closed_at INTEGER,
    closed_reason TEXT,
    close_blocked_at INTEGER,
    close_blocked_reason TEXT
);
CREATE TABLE IF NOT EXISTS session_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    at INTEGER NOT NULL,
    kind TEXT NOT NULL,
    detail TEXT
);
CREATE TABLE IF NOT EXISTS lifecycle_actions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    at INTEGER NOT NULL,
    session_id TEXT NOT NULL,
    action TEXT NOT NULL,
    target TEXT,
    result TEXT,
    tip_sha TEXT,
    undo_hint TEXT
);
";

/// What a SessionStart hook knows about the session it starts.
pub(crate) struct StartInfo {
    pub id: String,
    pub cwd: Option<String>,
    pub repo: Option<String>,
    pub worktree: Option<String>,
    pub branch: Option<String>,
    pub convention: Option<String>,
}

/// Open (creating if missing) the database at `path` with its schema in place.
/// A database stamped by a newer build is refused before any table is touched.
pub(crate) fn open_at(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create directory {}: {e}", parent.display()))?;
    }
    // Owner only before SQLite creates it: the rows hold local paths.
    make_db_file_private(path)?;
    let conn = Connection::open(path).map_err(|e| format!("failed to open {DB_NAME}: {e}"))?;
    // Before any statement that can contend for the write lock: the default
    // busy timeout is 0.
    conn.busy_timeout(Duration::from_millis(HOOK_BUSY_TIMEOUT_MS))
        .map_err(|e| format!("failed to set busy_timeout on {DB_NAME}: {e}"))?;
    // The WAL switch skips the busy handler, so a parallel opener gets BUSY
    // at once; retry it for a bounded time.
    retry_busy(
        || conn.pragma_update(None, "journal_mode", "WAL"),
        OPEN_BUDGET,
    )
    .map_err(|e| format!("failed to set journal_mode=WAL on {DB_NAME}: {e}"))?;
    // In WAL mode NORMAL cannot corrupt the file, and it skips an fsync per
    // commit, which keeps a hook write fast.
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| format!("failed to set synchronous on {DB_NAME}: {e}"))?;
    schema::check_and_stamp(&conn, DB_NAME, SCHEMA_VERSION).map_err(|e| e.to_string())?;
    retry_busy(|| conn.execute_batch(SCHEMA_SQL), OPEN_BUDGET)
        .map_err(|e| format!("failed to create the {DB_NAME} schema: {e}"))?;
    Ok(conn)
}

/// Open the shared database in the data folder.
pub(crate) fn open() -> Result<Connection, String> {
    open_at(&playbook_data::data_dir().join(DB_NAME))
}

/// Record a session start, returning the number of rows changed. A resumed
/// session keeps `started_at`, takes the new place, and drops its pull request
/// link when the branch changed, so it never keeps the link of an earlier
/// directory. One upsert statement, so there is no read to write upgrade.
pub(crate) fn start(conn: &Connection, info: &StartInfo, now: i64) -> Result<usize, String> {
    conn.execute(
        "INSERT INTO sessions (session_id, cwd, repo, worktree, branch, convention, started_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(session_id) DO UPDATE SET
             cwd = excluded.cwd,
             repo = excluded.repo,
             worktree = excluded.worktree,
             convention = excluded.convention,
             pr_number = CASE WHEN sessions.branch IS excluded.branch THEN sessions.pr_number END,
             pr_kind = CASE WHEN sessions.branch IS excluded.branch THEN sessions.pr_kind END,
             pr_checked_at = CASE WHEN sessions.branch IS excluded.branch THEN sessions.pr_checked_at END,
             pr_head_oid = CASE WHEN sessions.branch IS excluded.branch THEN sessions.pr_head_oid END,
             pr_base = CASE WHEN sessions.branch IS excluded.branch THEN sessions.pr_base END,
             branch = excluded.branch",
        params![
            info.id,
            info.cwd,
            info.repo,
            info.worktree,
            info.branch,
            info.convention,
            now
        ],
    )
    .map_err(|e| format!("failed to record the start of session {}: {e}", info.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;
    use playbook_store::params;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc::{sync_channel, RecvTimeoutError};
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};

    /// The 95th percentile of one hook write (open, `start`, close) must stay
    /// under this many milliseconds.
    const HOOK_WRITE_P95_TARGET_MS: u128 = 100;
    const ITERATIONS: usize = 200;

    fn info(id: &str, branch: &str, cwd: &str) -> StartInfo {
        StartInfo {
            id: id.to_string(),
            cwd: Some(cwd.to_string()),
            repo: Some("/r".to_string()),
            worktree: Some("/r/wt".to_string()),
            branch: Some(branch.to_string()),
            convention: Some("agent-tool".to_string()),
        }
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).expect("count")
    }

    fn tables(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .expect("prepare");
        stmt.query_map([], |r| r.get(0))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect()
    }

    fn meta_rows(conn: &Connection) -> Vec<(String, String)> {
        let mut stmt = conn
            .prepare("SELECT key, value FROM meta ORDER BY key")
            .expect("prepare");
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect()
    }

    #[test]
    fn open_at_creates_the_four_tables_in_an_empty_folder() {
        // Arrange
        let dir = scratch_dir("sessions-open");
        let path = dir.join("sessions.db");

        // Act
        let conn = open_at(&path).expect("open");

        // Assert
        assert_eq!(
            tables(&conn),
            ["lifecycle_actions", "meta", "session_events", "sessions"]
        );
    }

    #[test]
    fn open_at_refuses_a_newer_database_and_changes_nothing() {
        // Arrange
        let dir = scratch_dir("sessions-newer");
        let path = dir.join("sessions.db");
        {
            let conn = open_at(&path).expect("open");
            conn.execute(
                "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
                params![(SCHEMA_VERSION + 1).to_string()],
            )
            .expect("stamp newer");
            start(&conn, &info("s1", "a", "/r"), 1000).expect("start");
        }

        // Act
        let err = open_at(&path).expect_err("refused");

        // Assert
        assert!(err.contains("sessions.db"), "{err}");
        let raw = Connection::open(&path).expect("raw open");
        assert_eq!(count(&raw, "SELECT COUNT(*) FROM sessions"), 1);
        assert_eq!(count(&raw, "SELECT COUNT(*) FROM session_events"), 0);
        assert_eq!(count(&raw, "SELECT COUNT(*) FROM lifecycle_actions"), 0);
        assert_eq!(
            meta_rows(&raw),
            [(
                "schema_version".to_string(),
                (SCHEMA_VERSION + 1).to_string()
            )]
        );
    }

    #[test]
    fn open_at_twice_leaves_meta_unchanged() {
        // Arrange
        let dir = scratch_dir("sessions-twice");
        let path = dir.join("sessions.db");
        let first = meta_rows(&open_at(&path).expect("first open"));

        // Act
        let second = meta_rows(&open_at(&path).expect("second open"));

        // Assert
        assert_eq!(first, second);
        assert_eq!(
            first,
            [("schema_version".to_string(), SCHEMA_VERSION.to_string())]
        );
    }

    #[test]
    fn start_again_keeps_one_row_and_its_started_at() {
        // Arrange
        let dir = scratch_dir("sessions-restart");
        let conn = open_at(&dir.join("sessions.db")).expect("open");
        start(&conn, &info("s1", "a", "/r"), 1000).expect("first start");

        // Act
        start(&conn, &info("s1", "a", "/r"), 2000).expect("second start");

        // Assert
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM sessions"), 1);
        assert_eq!(count(&conn, "SELECT started_at FROM sessions"), 1000);
    }

    fn seed_pr(conn: &Connection) {
        conn.execute(
            "UPDATE sessions SET pr_number = 7, pr_kind = 'merged', pr_checked_at = 5, \
             pr_head_oid = 'abc', pr_base = 'main' WHERE session_id = 's1'",
            [],
        )
        .expect("seed pr");
    }

    fn pr_columns_set(conn: &Connection) -> i64 {
        count(
            conn,
            "SELECT COUNT(pr_number) + COUNT(pr_kind) + COUNT(pr_checked_at) \
             + COUNT(pr_head_oid) + COUNT(pr_base) FROM sessions",
        )
    }

    #[test]
    fn start_on_a_new_branch_takes_the_new_place_and_clears_the_pr() {
        // Arrange
        let dir = scratch_dir("sessions-branch");
        let conn = open_at(&dir.join("sessions.db")).expect("open");
        start(&conn, &info("s1", "a", "/r"), 1000).expect("first start");
        seed_pr(&conn);

        // Act
        start(&conn, &info("s1", "b", "/other"), 2000).expect("second start");

        // Assert
        let (cwd, branch): (String, String) = conn
            .query_row("SELECT cwd, branch FROM sessions", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .expect("row");
        assert_eq!((cwd.as_str(), branch.as_str()), ("/other", "b"));
        assert_eq!(pr_columns_set(&conn), 0);
    }

    #[test]
    fn start_on_the_same_branch_keeps_the_pr() {
        // Arrange
        let dir = scratch_dir("sessions-same-branch");
        let conn = open_at(&dir.join("sessions.db")).expect("open");
        start(&conn, &info("s1", "a", "/r"), 1000).expect("first start");
        seed_pr(&conn);

        // Act
        start(&conn, &info("s1", "a", "/elsewhere"), 2000).expect("second start");

        // Assert
        assert_eq!(pr_columns_set(&conn), 5);
    }

    #[test]
    fn start_changes_exactly_one_row_per_call() {
        // Arrange
        let dir = scratch_dir("sessions-changes");
        let conn = open_at(&dir.join("sessions.db")).expect("open");

        // Act
        let first = start(&conn, &info("s1", "a", "/r"), 1000).expect("first start");
        let second = start(&conn, &info("s1", "a", "/r"), 2000).expect("second start");

        // Assert
        assert_eq!((first, second), (1, 1));
    }

    /// Runs `ITERATIONS` hook writes (open, `start`, close, each timed) while a
    /// second thread upserts a row every 5 ms. Returns the durations, the
    /// number of successful `start` calls and the writer commits seen before
    /// and after the loop.
    fn run_under_busy_writer(dir: &Path) -> (Vec<Duration>, usize, usize, usize) {
        let path = dir.join("sessions.db");
        open_at(&path).expect("create database");
        let stop = Arc::new(AtomicBool::new(false));
        let committed = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));
        let (tx, rx) = sync_channel::<()>(1);

        let writer = {
            let (path, stop, committed, barrier) = (
                path.clone(),
                Arc::clone(&stop),
                Arc::clone(&committed),
                Arc::clone(&barrier),
            );
            std::thread::spawn(move || {
                let conn = open_at(&path).expect("writer open");
                barrier.wait();
                let mut n = 0;
                while !stop.load(Ordering::SeqCst) {
                    conn.execute(
                        "INSERT OR REPLACE INTO sessions (session_id, started_at) VALUES (?1, ?2)",
                        params![format!("w-{n}"), n],
                    )
                    .expect("writer commit");
                    n += 1;
                    committed.fetch_add(1, Ordering::SeqCst);
                    let _ = tx.try_send(());
                    std::thread::sleep(Duration::from_millis(5));
                }
            })
        };

        barrier.wait();
        let mut durations = Vec::with_capacity(ITERATIONS);
        let mut started = 0;
        let mut first_seen = 0;
        for i in 0..ITERATIONS {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(()) => {}
                Err(RecvTimeoutError::Timeout) => panic!("writer stopped committing"),
                Err(RecvTimeoutError::Disconnected) => panic!("writer thread died"),
            }
            let t = Instant::now();
            let conn = open_at(&path).expect("hook open");
            start(&conn, &info(&format!("i-{i}"), "a", "/r"), 1000).expect("hook start");
            drop(conn);
            durations.push(t.elapsed());
            started += 1;
            if i == 0 {
                first_seen = committed.load(Ordering::SeqCst);
            }
        }
        let last_seen = committed.load(Ordering::SeqCst);
        stop.store(true, Ordering::SeqCst);
        writer.join().expect("writer thread");
        (durations, started, first_seen, last_seen)
    }

    #[test]
    fn a_hook_write_never_errors_under_a_busy_writer() {
        // Arrange
        let dir = scratch_dir("sessions-busy");

        // Act
        let (_, started, first_seen, last_seen) = run_under_busy_writer(&dir);

        // Assert
        let conn = open_at(&dir.join("sessions.db")).expect("open");
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM sessions WHERE session_id LIKE 'i-%'"
            ),
            started as i64
        );
        assert!(last_seen > first_seen, "writer never overlapped the loop");
    }

    #[test]
    #[ignore = "manual latency gate: run with --ignored --nocapture"]
    fn hook_write_p95_is_within_the_target() {
        // Arrange
        let dir = scratch_dir("sessions-p95");

        // Act
        let (mut durations, ..) = run_under_busy_writer(&dir);
        durations.sort();

        // Assert
        let at = |p: usize| durations[(durations.len() * p / 100).min(durations.len() - 1)];
        let median = durations[durations.len() / 2];
        let max = durations[durations.len() - 1];
        println!(
            "hook write ms: median={:.2} p95={:.2} p99={:.2} max={:.2}",
            median.as_secs_f64() * 1000.0,
            at(95).as_secs_f64() * 1000.0,
            at(99).as_secs_f64() * 1000.0,
            max.as_secs_f64() * 1000.0
        );
        assert!(at(95).as_millis() <= HOOK_WRITE_P95_TARGET_MS);
    }
}
