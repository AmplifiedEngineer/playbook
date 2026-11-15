// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! One shot import of the per session runtime files that older builds wrote
//! (`start-ts`, `last-clean-ts`, `clean-exit`, `activity.json`) into
//! `sessions.db`. The original files stay on disk.

use super::activity::{self, Turn};
use super::store::{self, ImportRow};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::time::UNIX_EPOCH;

const START_FILE: &str = "start-ts";
const LAST_CLEAN_FILE: &str = "last-clean-ts";
const CLEAN_EXIT_FILE: &str = "clean-exit";

/// What one import run did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Summary {
    /// Session folders written to the database.
    pub imported: usize,
    /// Session folders left out because a file in them could not be read.
    pub skipped: Vec<String>,
}

/// Import the session folders under `runtime_root` into the database at
/// `db_path`. A folder with an unreadable file is skipped and named, so one bad
/// folder never fails the run. The database is only created when a folder
/// qualifies, and a finished import returns at once. `now` is the import clock. All file reads happen
/// before the one short write, so a hook never waits on them.
pub(crate) fn run(runtime_root: &Path, db_path: &Path, now: i64) -> Result<Summary, String> {
    if db_path.exists() && store::runtime_import_done(&store::open_at(db_path)?)? {
        return Ok(Summary::default());
    }
    let mut summary = Summary::default();
    let mut rows = Vec::new();
    for (id, dir) in session_folders(runtime_root)? {
        match read_folder(&id, &dir, now) {
            Ok(Some(row)) => rows.push(row),
            Ok(None) => {}
            Err(_) => summary.skipped.push(id),
        }
    }
    if rows.is_empty() {
        return Ok(summary);
    }
    let conn = store::open_at(db_path)?;
    store::import_rows(&conn, &rows, now)?;
    summary.imported = rows.len();
    Ok(summary)
}

/// The `(id, path)` of every folder under `runtime_root`, sorted by id. A
/// missing root has none.
fn session_folders(runtime_root: &Path) -> Result<Vec<(String, std::path::PathBuf)>, String> {
    let entries = match fs::read_dir(runtime_root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("failed to read {}: {e}", runtime_root.display())),
    };
    let mut folders: Vec<_> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| Some((e.file_name().into_string().ok()?, e.path())))
        .collect();
    folders.sort();
    Ok(folders)
}

/// The text of `dir/name`, `None` when the file does not exist.
fn read_optional(dir: &Path, name: &str) -> std::io::Result<Option<String>> {
    match fs::read_to_string(dir.join(name)) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn seconds(text: Option<String>) -> Option<i64> {
    text?.trim().parse().ok()
}

/// When a session ended: the marker's modification time, else its last clean
/// stop, else the import time. A zero would read as the epoch and hide a
/// session that really ended, so the import clock is the last resort.
fn end_time(marker_modified: Option<i64>, last_stop_at: Option<i64>, now: i64) -> i64 {
    marker_modified.or(last_stop_at).unwrap_or(now)
}

/// The row of one folder, `None` when it holds none of the four files. A
/// read error other than a missing file is returned. `activity.json` problems
/// never are, because `activity::read` turns them into `None`.
fn read_folder(id: &str, dir: &Path, now: i64) -> std::io::Result<Option<ImportRow>> {
    let started = read_optional(dir, START_FILE)?;
    let last_clean = read_optional(dir, LAST_CLEAN_FILE)?;
    let clean_exit = read_optional(dir, CLEAN_EXIT_FILE)?;
    let activity = activity::read(dir);
    if started.is_none() && last_clean.is_none() && clean_exit.is_none() && activity.is_none() {
        return Ok(None);
    }
    let mut row = ImportRow {
        id: id.to_string(),
        started_at: seconds(started),
        last_stop_at: seconds(last_clean),
        ..ImportRow::default()
    };
    if let Some(reason) = clean_exit {
        // The marker has no time of its own: its modification time is when the
        // session ended.
        let ended = fs::metadata(dir.join(CLEAN_EXIT_FILE))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        row.ended_at = Some(end_time(ended, row.last_stop_at, now));
        row.end_reason = Some(reason.trim().to_string());
    }
    if let Some(found) = activity {
        let ts = found.ts as i64;
        match found.turn {
            Turn::Working => row.last_prompt_at = Some(ts),
            Turn::Idle => row.last_stop_at = row.last_stop_at.or(Some(ts)),
        }
        if found.notice.is_some() {
            row.waiting_since = Some(ts);
        }
    }
    Ok(Some(row))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;
    use crate::sessions::store;
    use playbook_store::{params, Connection};
    use std::fs;
    use std::path::PathBuf;

    struct Home {
        runtime: PathBuf,
        db: PathBuf,
    }

    fn home(tag: &str) -> Home {
        let dir = scratch_dir(tag);
        Home {
            runtime: dir.join("runtime"),
            db: dir.join("data").join("sessions.db"),
        }
    }

    impl Home {
        fn folder(&self, id: &str, files: &[(&str, &str)]) -> PathBuf {
            let dir = self.runtime.join(id);
            fs::create_dir_all(&dir).unwrap();
            for (name, body) in files {
                fs::write(dir.join(name), body).unwrap();
            }
            dir
        }

        fn conn(&self) -> Connection {
            store::open_at(&self.db).expect("open")
        }

        /// One column of the row of `id` as an integer, `None` when null.
        fn int(&self, id: &str, column: &str) -> Option<i64> {
            self.conn()
                .query_row(
                    &format!("SELECT {column} FROM sessions WHERE session_id = ?1"),
                    [id],
                    |r| r.get(0),
                )
                .expect("row")
        }

        fn rows(&self, id: &str) -> i64 {
            self.conn()
                .query_row(
                    "SELECT COUNT(*) FROM sessions WHERE session_id = ?1",
                    [id],
                    |r| r.get(0),
                )
                .unwrap()
        }
    }

    /// The import clock of every test.
    const NOW: i64 = 5000;
    const WORKING_ACTIVITY: &str = r#"{"turn":"working","notice":null,"ts":700}"#;
    const IDLE_ACTIVITY: &str = r#"{"turn":"idle","notice":null,"ts":800}"#;
    const WAITING_ACTIVITY: &str = r#"{"turn":"working","notice":"permission_prompt","ts":900}"#;

    #[test]
    fn the_files_of_a_folder_map_to_the_row_columns() {
        // Arrange
        let h = home("import-map");
        h.folder("s1", &[("start-ts", "500"), ("last-clean-ts", "600\n")]);

        // Act
        let summary = run(&h.runtime, &h.db, NOW).expect("import");

        // Assert
        assert_eq!(summary.imported, 1);
        assert_eq!(h.int("s1", "started_at"), Some(500));
        assert_eq!(h.int("s1", "last_stop_at"), Some(600));
        assert_eq!(h.int("s1", "ended_at"), None);
    }

    #[test]
    fn the_activity_file_maps_to_the_turn_and_waiting_columns() {
        // Arrange: (activity.json, last_prompt_at, last_stop_at, waiting_since, waiting_after_prompt)
        let cases = [
            (WORKING_ACTIVITY, Some(700), None, None, 0),
            (IDLE_ACTIVITY, None, Some(800), None, 0),
            (WAITING_ACTIVITY, Some(900), None, Some(900), 1),
        ];
        for (body, prompt, stop, waiting, flag) in cases {
            let h = home("import-activity");
            h.folder("s1", &[("activity.json", body)]);

            // Act
            run(&h.runtime, &h.db, NOW).expect("import");

            // Assert
            assert_eq!(h.int("s1", "last_prompt_at"), prompt, "{body}");
            assert_eq!(h.int("s1", "last_stop_at"), stop, "{body}");
            assert_eq!(h.int("s1", "waiting_since"), waiting, "{body}");
            assert_eq!(h.int("s1", "waiting_after_prompt"), Some(flag), "{body}");
        }
    }

    #[test]
    fn last_clean_ts_wins_over_an_idle_activity_file() {
        // Arrange
        let h = home("import-clean-wins");
        h.folder(
            "s1",
            &[("last-clean-ts", "600"), ("activity.json", IDLE_ACTIVITY)],
        );

        // Act
        run(&h.runtime, &h.db, NOW).expect("import");

        // Assert
        assert_eq!(h.int("s1", "last_stop_at"), Some(600));
    }

    #[test]
    fn a_corrupt_activity_file_keeps_the_other_columns_and_does_not_skip() {
        // Arrange
        let h = home("import-bad-activity");
        h.folder("s1", &[("start-ts", "500"), ("activity.json", "not json")]);

        // Act
        let summary = run(&h.runtime, &h.db, NOW).expect("import");

        // Assert
        assert!(summary.skipped.is_empty());
        assert_eq!(h.int("s1", "started_at"), Some(500));
        assert_eq!(h.int("s1", "waiting_since"), None);
    }

    #[test]
    fn an_activity_file_that_is_a_directory_does_not_skip_the_folder() {
        // Arrange
        let h = home("import-dir-activity");
        let dir = h.folder("s1", &[("start-ts", "500")]);
        fs::create_dir(dir.join("activity.json")).unwrap();

        // Act
        let summary = run(&h.runtime, &h.db, NOW).expect("import");

        // Assert
        assert!(summary.skipped.is_empty());
        assert_eq!(h.int("s1", "started_at"), Some(500));
    }

    #[test]
    fn a_start_ts_that_cannot_be_read_skips_only_that_folder() {
        // Arrange
        let h = home("import-skip");
        h.folder("a1", &[("start-ts", "500")]);
        let bad = h.folder("b2", &[]);
        fs::create_dir(bad.join("start-ts")).unwrap();

        // Act
        let summary = run(&h.runtime, &h.db, NOW).expect("import");

        // Assert
        assert_eq!(summary.skipped, ["b2"]);
        assert_eq!(summary.imported, 1);
        assert_eq!(h.int("a1", "started_at"), Some(500));
        assert_eq!(h.rows("b2"), 0);
    }

    #[test]
    fn a_minimal_row_takes_the_started_at_of_the_file() {
        // Arrange: a hook made the row (no cwd) with started_at 900.
        let h = home("import-minimal");
        store::stop(&h.conn(), "s1", 900).expect("stop");
        h.folder("s1", &[("start-ts", "500")]);

        // Act
        run(&h.runtime, &h.db, NOW).expect("import");

        // Assert
        assert_eq!(h.int("s1", "started_at"), Some(500));
    }

    #[test]
    fn an_existing_row_is_filled_only_in_its_null_columns() {
        // Arrange: a row with a cwd, started_at null and last_stop_at 5.
        let h = home("import-fill");
        let conn = h.conn();
        conn.execute(
            "INSERT INTO sessions (session_id, cwd, last_stop_at) VALUES ('s1', '/r', 5)",
            params![],
        )
        .unwrap();
        h.folder("s1", &[("start-ts", "500"), ("last-clean-ts", "600")]);

        // Act
        run(&h.runtime, &h.db, NOW).expect("import");

        // Assert
        assert_eq!(h.int("s1", "started_at"), Some(500));
        assert_eq!(h.int("s1", "last_stop_at"), Some(5));
    }

    #[test]
    fn an_existing_row_keeps_its_waiting_flag() {
        // Arrange: a hook made the row, so the flag is 0 and waiting_since is null.
        let h = home("import-flag");
        store::stop(&h.conn(), "s1", 900).expect("stop");
        h.folder("s1", &[("activity.json", WAITING_ACTIVITY)]);

        // Act
        run(&h.runtime, &h.db, NOW).expect("import");

        // Assert
        assert_eq!(h.int("s1", "waiting_since"), Some(900));
        assert_eq!(h.int("s1", "waiting_after_prompt"), Some(0));
    }

    #[test]
    fn a_clean_exit_ends_only_a_row_the_import_inserts() {
        // Arrange: a hook made s1 with a cwd through `start`.
        let h = home("import-ended");
        let info = store::StartInfo {
            id: "s1".to_string(),
            cwd: Some("/r".to_string()),
            repo: None,
            worktree: None,
            branch: None,
            convention: None,
        };
        store::start(&h.conn(), &info, 900).expect("start");
        h.folder("s1", &[("clean-exit", "logout\n")]);
        h.folder("s2", &[("clean-exit", "logout\n")]);

        // Act
        run(&h.runtime, &h.db, NOW).expect("import");

        // Assert
        assert_eq!(h.int("s1", "ended_at"), None);
        assert!(h.int("s2", "ended_at").is_some());
        let reason: String = h
            .conn()
            .query_row(
                "SELECT end_reason FROM sessions WHERE session_id = 's2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(reason, "logout");
    }

    #[test]
    fn a_rerun_after_success_changes_nothing() {
        // Arrange
        let h = home("import-rerun");
        h.folder("s1", &[("start-ts", "500"), ("clean-exit", "logout")]);
        run(&h.runtime, &h.db, NOW).expect("first");
        store::prompt(&h.conn(), "s1", 1000).expect("resume");

        // Act
        let summary = run(&h.runtime, &h.db, NOW).expect("second");

        // Assert
        assert_eq!(summary, Summary::default());
        assert_eq!(h.int("s1", "ended_at"), None);
    }

    #[test]
    fn the_runtime_import_mark_holds_the_given_now() {
        // Arrange
        let h = home("import-mark");
        h.folder("s1", &[("start-ts", "500")]);

        // Act
        run(&h.runtime, &h.db, 1234).expect("import");

        // Assert
        let mark: String = h
            .conn()
            .query_row(
                "SELECT value FROM meta WHERE key = 'runtime_import'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(mark, "1234");
    }

    #[test]
    fn the_end_time_falls_back_from_the_marker_to_the_last_stop_to_now() {
        // Arrange: (marker modified, last_stop_at, expected)
        let cases = [
            (Some(700), Some(600), 700),
            (None, Some(600), 600),
            (None, None, NOW),
        ];
        for (modified, last_stop, expected) in cases {
            // Act
            let ended = end_time(modified, last_stop, NOW);

            // Assert
            assert_eq!(ended, expected, "{modified:?} {last_stop:?}");
        }
    }

    #[test]
    fn no_qualifying_folder_creates_no_database() {
        // Arrange
        let h = home("import-none");
        h.folder("s1", &[("search-count", "3")]);

        // Act
        let summary = run(&h.runtime, &h.db, NOW).expect("import");

        // Assert
        assert_eq!(summary, Summary::default());
        assert!(!h.db.exists());
    }

    #[test]
    fn the_original_files_stay_on_disk() {
        // Arrange
        let h = home("import-keep");
        let dir = h.folder(
            "s1",
            &[("start-ts", "500"), ("activity.json", IDLE_ACTIVITY)],
        );

        // Act
        run(&h.runtime, &h.db, NOW).expect("import");

        // Assert
        assert!(dir.join("start-ts").is_file());
        assert!(dir.join("activity.json").is_file());
    }

    #[test]
    fn a_corrupt_database_is_an_error() {
        // Arrange
        let h = home("import-corrupt");
        h.folder("s1", &[("start-ts", "500")]);
        fs::create_dir_all(h.db.parent().unwrap()).unwrap();
        fs::write(&h.db, "not a database").unwrap();

        // Act
        let result = run(&h.runtime, &h.db, NOW);

        // Assert
        assert!(result.is_err());
    }
}
