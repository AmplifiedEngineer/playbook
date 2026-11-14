// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `sessions.db` store: one row per agent session, with an append only
//! event log and an audit table for lifecycle actions. Every writer is one
//! statement (an upsert) or a `BEGIN IMMEDIATE` transaction: a deferred
//! transaction that upgrades from read to write fails at once with
//! SQLITE_BUSY in WAL mode, and `busy_timeout` does not retry it.

use super::ACTIVE_SECONDS;
use crate::common::atomic::make_db_file_private;
use crate::common::sqlite::{retry_busy, OPEN_BUDGET};
use playbook_store::{params, schema, Connection};
use std::path::Path;
use std::time::Duration;

/// The `agent` of every row a hook writes. It equals the name the usage events
/// of the Claude Code source carry, so the two tables join on it.
const AGENT: &str = "claude-code";

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

/// Run `f` in a `BEGIN IMMEDIATE` transaction: a deferred one that upgrades
/// from read to write fails at once with SQLITE_BUSY, and `busy_timeout` does
/// not retry it.
fn immediate<T>(
    conn: &Connection,
    f: impl FnOnce(&Connection) -> playbook_store::Result<T>,
) -> playbook_store::Result<T> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    match f(conn).and_then(|v| conn.execute_batch("COMMIT").map(|()| v)) {
        Ok(v) => Ok(v),
        Err(e) => {
            // The original error is the one worth reporting.
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Append a `reopened` event when the row of `id` is closed, so the reopening
/// is in the log before the caller clears the close marks.
fn log_reopen(conn: &Connection, id: &str, now: i64) -> playbook_store::Result<usize> {
    conn.execute(
        "INSERT INTO session_events (session_id, at, kind)
         SELECT session_id, ?2, 'reopened' FROM sessions
         WHERE session_id = ?1 AND closed_at IS NOT NULL",
        params![id, now],
    )
}

/// Record a session start, returning the number of rows changed. A resumed
/// session keeps `started_at`, counts the resume as a fresh event (so a newer
/// session of the directory does not mark it crashed), takes the new place, leaves the ended, crashed
/// and closed states (a closed row gets a `reopened` event), and drops its pull
/// request link when the branch changed, so it never keeps the link of an
/// earlier directory.
pub(crate) fn start(conn: &Connection, info: &StartInfo, now: i64) -> Result<usize, String> {
    immediate(conn, |conn| {
        log_reopen(conn, &info.id, now)?;
        conn.execute(
            "INSERT INTO sessions (session_id, cwd, repo, worktree, branch, convention, started_at, agent)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(session_id) DO UPDATE SET
                 cwd = excluded.cwd,
                 repo = excluded.repo,
                 worktree = excluded.worktree,
                 convention = excluded.convention,
                 ended_at = NULL,
                 end_reason = NULL,
                 crashed_at = NULL,
                 closed_at = NULL,
                 closed_reason = NULL,
                 last_stop_at = MAX(COALESCE(sessions.last_stop_at, 0), excluded.started_at),
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
                now,
                AGENT
            ],
        )
    })
    .map_err(|e| format!("failed to record the start of session {}: {e}", info.id))
}

/// Run one hook update on the row of `id` in a single transaction. An id with
/// no row first gets a minimal one (a hook can fire for a session that started
/// before the database existed), and a closed row is reopened when `reopen` is set.
fn touch(
    conn: &Connection,
    id: &str,
    now: i64,
    reopen: bool,
    update: impl FnOnce(&Connection) -> playbook_store::Result<usize>,
) -> Result<usize, String> {
    immediate(conn, |conn| {
        conn.execute(
            "INSERT OR IGNORE INTO sessions (session_id, agent, started_at) VALUES (?1, ?2, ?3)",
            params![id, AGENT, now],
        )?;
        if reopen {
            log_reopen(conn, id, now)?;
        }
        update(conn)
    })
    .map_err(|e| format!("failed to update session {id}: {e}"))
}

/// A prompt starts a turn: the session is no longer waiting, crashed, ended or
/// closed (a closed row gets a `reopened` event).
pub(crate) fn prompt(conn: &Connection, id: &str, now: i64) -> Result<usize, String> {
    touch(conn, id, now, true, |conn| {
        conn.execute(
            "UPDATE sessions SET last_prompt_at = ?2, waiting_after_prompt = 0,
                 crashed_at = NULL, ended_at = NULL, end_reason = NULL,
                 closed_at = NULL, closed_reason = NULL
             WHERE session_id = ?1",
            params![id, now],
        )
    })
}

/// A stop ends the turn. It clears the waiting flag, because an approved
/// permission ends in a stop with no new prompt, and a crash mark.
pub(crate) fn stop(conn: &Connection, id: &str, now: i64) -> Result<usize, String> {
    touch(conn, id, now, false, |conn| {
        conn.execute(
            "UPDATE sessions SET last_stop_at = ?2, waiting_after_prompt = 0, crashed_at = NULL
             WHERE session_id = ?1",
            params![id, now],
        )
    })
}

/// A notice that the session waits for a person.
pub(crate) fn waiting(conn: &Connection, id: &str, now: i64) -> Result<usize, String> {
    touch(conn, id, now, false, |conn| {
        conn.execute(
            "UPDATE sessions SET waiting_since = ?2, waiting_after_prompt = 1, crashed_at = NULL
             WHERE session_id = ?1",
            params![id, now],
        )
    })
}

/// The session ended with `reason`.
pub(crate) fn end(conn: &Connection, id: &str, reason: &str, now: i64) -> Result<usize, String> {
    touch(conn, id, now, false, |conn| {
        conn.execute(
            "UPDATE sessions SET ended_at = ?2, end_reason = ?3 WHERE session_id = ?1",
            params![id, now, reason],
        )
    })
}

/// The filter for the rows of `cwd` other than the caller's that are still open and whose
/// last event is older than `ACTIVE_SECONDS`. `?1` is `cwd`, `?2` the caller's
/// id, `?3` `now` and `?4` `ACTIVE_SECONDS`.
const CRASHED_CANDIDATES: &str = "WHERE cwd = ?1 AND session_id != ?2
      AND ended_at IS NULL AND crashed_at IS NULL AND closed_at IS NULL
      AND ?3 - MAX(COALESCE(started_at, 0), COALESCE(last_prompt_at, 0),
                   COALESCE(last_stop_at, 0)) > ?4";

/// Mark every other open row of `cwd` that has been silent for more than
/// `ACTIVE_SECONDS` as crashed, with one `crashed` event each. Returns the
/// number of rows marked.
pub(crate) fn mark_crashed(
    conn: &Connection,
    cwd: &str,
    caller_id: &str,
    now: i64,
) -> Result<usize, String> {
    immediate(conn, |conn| {
        let args = params![cwd, caller_id, now, ACTIVE_SECONDS];
        conn.execute(
            &format!(
                "INSERT INTO session_events (session_id, at, kind)
                 SELECT session_id, ?3, 'crashed' FROM sessions {CRASHED_CANDIDATES}"
            ),
            args,
        )?;
        conn.execute(
            &format!("UPDATE sessions SET crashed_at = ?3 {CRASHED_CANDIDATES}"),
            args,
        )
    })
    .map_err(|e| format!("failed to mark crashed sessions of {cwd}: {e}"))
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

    const ACTIVE: i64 = crate::sessions::ACTIVE_SECONDS;

    fn open_db(tag: &str) -> Connection {
        open_at(&scratch_dir(tag).join("sessions.db")).expect("open")
    }

    fn crashed_at(conn: &Connection, id: &str) -> Option<i64> {
        conn.query_row(
            "SELECT crashed_at FROM sessions WHERE session_id = ?1",
            [id],
            |r| r.get(0),
        )
        .expect("row")
    }

    fn events_of(conn: &Connection, kind: &str) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM session_events WHERE kind = ?1",
            [kind],
            |r| r.get(0),
        )
        .expect("count")
    }

    #[test]
    fn mark_crashed_sets_the_row_when_its_last_event_is_over_the_active_window() {
        // Arrange
        let conn = open_db("sessions-crash-901");
        start(&conn, &info("s0", "a", "/proj"), 1000).expect("start");

        // Act
        mark_crashed(&conn, "/proj", "s1", 1000 + ACTIVE + 1).expect("mark");

        // Assert
        assert_eq!(crashed_at(&conn, "s0"), Some(1000 + ACTIVE + 1));
        assert_eq!(events_of(&conn, "crashed"), 1);
    }

    #[test]
    fn mark_crashed_leaves_a_row_at_or_inside_the_active_window() {
        for now in [1000 + ACTIVE, 1000 + ACTIVE - 1] {
            // Arrange
            let conn = open_db("sessions-crash-edge");
            start(&conn, &info("s0", "a", "/proj"), 1000).expect("start");

            // Act
            mark_crashed(&conn, "/proj", "s1", now).expect("mark");

            // Assert
            assert_eq!(crashed_at(&conn, "s0"), None, "now = {now}");
        }
    }

    #[test]
    fn a_resumed_session_is_not_marked_crashed_by_a_newer_session_of_the_directory() {
        // Arrange
        let conn = open_db("sessions-crash-resume");
        start(&conn, &info("s0", "a", "/proj"), 1000).expect("start");
        start(&conn, &info("s0", "a", "/proj"), 5000).expect("resume");

        // Act
        mark_crashed(&conn, "/proj", "other", 5000 + 100).expect("mark");

        // Assert
        assert_eq!(crashed_at(&conn, "s0"), None);
        assert_eq!(col(&conn, "s0", "started_at"), Some(1000));
    }

    #[test]
    fn mark_crashed_uses_the_newest_of_the_three_event_times() {
        // Arrange
        let conn = open_db("sessions-crash-newest");
        start(&conn, &info("s0", "a", "/proj"), 1000).expect("start");
        conn.execute(
            "UPDATE sessions SET last_stop_at = 5000 WHERE session_id = 's0'",
            [],
        )
        .expect("stop");

        // Act
        mark_crashed(&conn, "/proj", "s1", 5000 + ACTIVE).expect("mark");

        // Assert
        assert_eq!(crashed_at(&conn, "s0"), None);
    }

    #[test]
    fn mark_crashed_skips_rows_that_are_ended_closed_crashed_elsewhere_or_the_caller() {
        // Arrange
        let conn = open_db("sessions-crash-skip");
        for id in ["ended", "closed", "crashed", "other", "s1"] {
            let cwd = if id == "other" { "/elsewhere" } else { "/proj" };
            start(&conn, &info(id, "a", cwd), 1000).expect("start");
        }
        conn.execute(
            "UPDATE sessions SET ended_at = 1 WHERE session_id = 'ended'",
            [],
        )
        .expect("ended");
        conn.execute(
            "UPDATE sessions SET closed_at = 1 WHERE session_id = 'closed'",
            [],
        )
        .expect("closed");
        conn.execute(
            "UPDATE sessions SET crashed_at = 7 WHERE session_id = 'crashed'",
            [],
        )
        .expect("crashed");

        // Act
        mark_crashed(&conn, "/proj", "s1", 1000 + ACTIVE + 1).expect("mark");

        // Assert
        let changed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE crashed_at IS NOT NULL AND crashed_at != 7",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!((changed, events_of(&conn, "crashed")), (0, 0));
    }

    #[test]
    fn start_on_a_crashed_or_ended_row_clears_both_marks() {
        // Arrange
        let conn = open_db("sessions-resume");
        start(&conn, &info("s0", "a", "/proj"), 1000).expect("start");
        conn.execute(
            "UPDATE sessions SET crashed_at = 5, ended_at = 6, end_reason = 'logout'",
            [],
        )
        .expect("mark");

        // Act
        start(&conn, &info("s0", "a", "/proj"), 2000).expect("resume");

        // Assert
        let (crashed, ended): (Option<i64>, Option<i64>) = conn
            .query_row("SELECT crashed_at, ended_at FROM sessions", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .expect("row");
        assert_eq!((crashed, ended), (None, None));
    }

    #[test]
    fn start_on_a_closed_row_reopens_it_and_logs_one_event() {
        // Arrange
        let conn = open_db("sessions-reopen");
        start(&conn, &info("s0", "a", "/proj"), 1000).expect("start");
        conn.execute(
            "UPDATE sessions SET closed_at = 5, closed_reason = 'landed'",
            [],
        )
        .expect("close");

        // Act
        start(&conn, &info("s0", "a", "/proj"), 2000).expect("resume");

        // Assert
        let (closed, reason): (Option<i64>, Option<String>) = conn
            .query_row("SELECT closed_at, closed_reason FROM sessions", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .expect("row");
        assert_eq!((closed, reason), (None, None));
        assert_eq!(events_of(&conn, "reopened"), 1);
    }

    fn col(conn: &Connection, id: &str, column: &str) -> Option<i64> {
        conn.query_row(
            &format!("SELECT {column} FROM sessions WHERE session_id = ?1"),
            [id],
            |r| r.get(0),
        )
        .expect("row")
    }

    fn seeded(tag: &str) -> Connection {
        let conn = open_db(tag);
        start(&conn, &info("s1", "a", "/r"), 900).expect("start");
        conn
    }

    #[test]
    fn waiting_sets_the_time_and_the_flag() {
        // Arrange
        let conn = seeded("sessions-waiting");
        conn.execute("UPDATE sessions SET last_prompt_at = 1000", [])
            .expect("seed");

        // Act
        waiting(&conn, "s1", 1005).expect("waiting");

        // Assert
        assert_eq!(col(&conn, "s1", "waiting_since"), Some(1005));
        assert_eq!(col(&conn, "s1", "waiting_after_prompt"), Some(1));
    }

    #[test]
    fn prompt_clears_the_flag_and_sets_the_time() {
        // Arrange
        let conn = seeded("sessions-prompt");
        waiting(&conn, "s1", 1005).expect("waiting");

        // Act
        prompt(&conn, "s1", 1010).expect("prompt");

        // Assert
        assert_eq!(col(&conn, "s1", "waiting_after_prompt"), Some(0));
        assert_eq!(col(&conn, "s1", "last_prompt_at"), Some(1010));
    }

    #[test]
    fn stop_sets_the_time_clears_the_flag_and_keeps_waiting_since() {
        // Arrange
        let conn = seeded("sessions-stop");
        waiting(&conn, "s1", 1005).expect("waiting");

        // Act
        stop(&conn, "s1", 1020).expect("stop");

        // Assert
        assert_eq!(col(&conn, "s1", "last_stop_at"), Some(1020));
        assert_eq!(col(&conn, "s1", "waiting_after_prompt"), Some(0));
        assert_eq!(col(&conn, "s1", "waiting_since"), Some(1005));
    }

    #[test]
    fn a_notice_after_a_stop_sets_the_flag_again() {
        // Arrange
        let conn = seeded("sessions-stop-notice");
        waiting(&conn, "s1", 1005).expect("waiting");
        stop(&conn, "s1", 1020).expect("stop");

        // Act
        waiting(&conn, "s1", 1025).expect("waiting");

        // Assert
        assert_eq!(col(&conn, "s1", "waiting_after_prompt"), Some(1));
    }

    #[test]
    fn end_sets_the_time_and_the_reason() {
        // Arrange
        let conn = seeded("sessions-end");

        // Act
        end(&conn, "s1", "logout", 1030).expect("end");

        // Assert
        assert_eq!(col(&conn, "s1", "ended_at"), Some(1030));
        let reason: String = conn
            .query_row("SELECT end_reason FROM sessions", [], |r| r.get(0))
            .expect("reason");
        assert_eq!(reason, "logout");
    }

    #[test]
    fn prompt_clears_the_crashed_and_ended_marks() {
        // Arrange
        let conn = seeded("sessions-prompt-marks");
        conn.execute("UPDATE sessions SET crashed_at = 950, ended_at = 960", [])
            .expect("seed");

        // Act
        prompt(&conn, "s1", 1010).expect("prompt");

        // Assert
        assert_eq!(col(&conn, "s1", "crashed_at"), None);
        assert_eq!(col(&conn, "s1", "ended_at"), None);
    }

    #[test]
    fn prompt_on_a_closed_row_reopens_it_with_one_event() {
        // Arrange
        let conn = seeded("sessions-prompt-reopen");
        conn.execute(
            "UPDATE sessions SET closed_at = 950, closed_reason = 'merged'",
            [],
        )
        .expect("seed");

        // Act
        prompt(&conn, "s1", 1010).expect("prompt");

        // Assert
        assert_eq!(col(&conn, "s1", "closed_at"), None);
        assert_eq!(col(&conn, "s1", "closed_reason"), None);
        assert_eq!(events_of(&conn, "reopened"), 1);
    }

    #[test]
    fn prompt_stop_and_waiting_clear_a_crashed_mark() {
        type Writer = fn(&Connection, &str, i64) -> Result<usize, String>;
        let writers: [(&str, Writer); 3] =
            [("prompt", prompt), ("stop", stop), ("waiting", waiting)];
        for (name, write) in writers {
            // Arrange
            let conn = seeded(&format!("sessions-crash-{name}"));
            conn.execute("UPDATE sessions SET crashed_at = 950", [])
                .expect("seed");

            // Act
            write(&conn, "s1", 1010).expect(name);

            // Assert
            assert_eq!(col(&conn, "s1", "crashed_at"), None, "{name}");
        }
    }

    #[test]
    fn a_writer_for_an_id_without_a_row_inserts_a_minimal_row_then_applies_the_update() {
        type Call = fn(&Connection) -> Result<usize, String>;
        let calls: [(&str, Call, &str); 4] = [
            ("prompt", |c| prompt(c, "new", 1010), "last_prompt_at"),
            ("stop", |c| stop(c, "new", 1010), "last_stop_at"),
            ("waiting", |c| waiting(c, "new", 1010), "waiting_since"),
            ("end", |c| end(c, "new", "logout", 1010), "ended_at"),
        ];
        for (name, call, column) in calls {
            // Arrange
            let conn = open_db(&format!("sessions-norow-{name}"));

            // Act
            call(&conn).expect(name);

            // Assert
            assert_eq!(col(&conn, "new", column), Some(1010), "{name}");
            assert_eq!(col(&conn, "new", "started_at"), Some(1010), "{name}");
            let (agent, cwd): (String, Option<String>) = conn
                .query_row("SELECT agent, cwd FROM sessions", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .expect("row");
            assert_eq!((agent.as_str(), cwd), (AGENT, None), "{name}");
        }
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
