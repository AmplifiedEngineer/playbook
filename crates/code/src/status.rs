// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Index freshness. `quick_check` compares size and mtime only and runs
//! before every query. `status` is the full check that hashes every file.

use super::index::load_existing;
use super::output::IndexState;
use super::{db, host, walker};
use playbook_common::hash::sha256_hex;
use playbook_store::Connection;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

/// Paths that differ between the index and the disk.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Drift {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
}

impl Drift {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// Compares the index with the disk by size and mtime only, so it can run
/// before every query. An edit that keeps both is missed on purpose; `status`
/// and result re-hashing cover that case.
pub fn quick_check(conn: &Connection, root: &Path) -> Result<Drift, String> {
    let existing = load_existing(conn)?;
    let (listed, _) = walker::list_paths(root)?;
    let mut drift = Drift::default();
    let mut seen: HashSet<&str> = HashSet::new();
    for path in &listed {
        let meta = match std::fs::symlink_metadata(root.join(path)) {
            Ok(meta) => meta,
            // Listed by git but gone from disk: removed if we hold rows.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                seen.insert(path);
                continue;
            }
        };
        seen.insert(path);
        match existing.get(path) {
            None => drift.added.push(path.clone()),
            Some(_) if meta.is_dir() => {}
            Some(row) => {
                let same_size = i64::try_from(meta.len()).ok() == Some(row.size);
                if !same_size || walker::mtime_ns(&meta) != row.mtime_ns {
                    drift.changed.push(path.clone());
                }
            }
        }
    }
    drift.removed = existing
        .keys()
        .filter(|path| !seen.contains(path.as_str()))
        .cloned()
        .collect();
    sort_all(&mut drift);
    Ok(drift)
}

/// Compares the index with the disk by hashing every listed file.
pub fn full_check(conn: &Connection, root: &Path) -> Result<Drift, String> {
    let existing = load_existing(conn)?;
    let found = walker::discover(root, &walker::Limits::default())?;
    let mut drift = Drift::default();
    let mut seen: HashSet<&str> = HashSet::new();
    for path in &found.files {
        seen.insert(path);
        let Some(row) = existing.get(path) else {
            drift.added.push(path.clone());
            continue;
        };
        let same = (row.status == "ok" || row.status == "partial")
            && std::fs::read(root.join(path)).is_ok_and(|bytes| sha256_hex(&bytes) == row.sha);
        if !same {
            drift.changed.push(path.clone());
        }
    }
    for skipped in &found.skipped {
        seen.insert(&skipped.path);
        match existing.get(&skipped.path) {
            None => drift.added.push(skipped.path.clone()),
            Some(row) => {
                let same = row.status == "skipped"
                    && row.error.as_deref() == Some(skipped.reason.as_str())
                    && i64::try_from(skipped.size).ok() == Some(row.size)
                    && row.mtime_ns == skipped.mtime_ns;
                if !same {
                    drift.changed.push(skipped.path.clone());
                }
            }
        }
    }
    drift.removed = existing
        .keys()
        .filter(|path| !seen.contains(path.as_str()))
        .cloned()
        .collect();
    sort_all(&mut drift);
    Ok(drift)
}

fn sort_all(drift: &mut Drift) {
    drift.added.sort();
    drift.removed.sort();
    drift.changed.sort();
}

/// `fresh` when nothing differs, `stale` otherwise.
pub fn state_of(drift: &Drift) -> IndexState {
    if drift.is_empty() {
        IndexState::Fresh
    } else {
        IndexState::Stale
    }
}

/// The warning a query prints when the index is behind the disk.
pub fn stale_warning(drift: &Drift) -> String {
    format!(
        "the index is stale ({} added, {} changed, {} removed); results can miss new code, \
         run `{}`",
        drift.added.len(),
        drift.changed.len(),
        drift.removed.len(),
        host::command("index")
    )
}

/// What `status` reports.
#[derive(Debug, Clone)]
pub struct Status {
    pub state: IndexState,
    pub drift: Drift,
    pub schema_version: i64,
    pub by_status: BTreeMap<String, usize>,
    pub by_lang: BTreeMap<String, usize>,
    pub db_bytes: u64,
    pub last_run_started: Option<i64>,
    pub last_run_completed: Option<i64>,
    pub interrupted: bool,
}

/// When this worktree's last run started and completed, from its row.
fn run_times(conn: &Connection, worktree: &str) -> (Option<i64>, Option<i64>) {
    conn.query_row(
        "SELECT last_run_started, last_run_completed FROM worktrees WHERE key = ?1",
        [worktree],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap_or((None, None))
}

fn group_counts(conn: &Connection, column: &str) -> Result<BTreeMap<String, usize>, String> {
    let sql = format!("SELECT COALESCE({column}, 'unknown'), COUNT(*) FROM files GROUP BY 1");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| super::db::map_sql_error("status failed", &e))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .map_err(|e| super::db::map_sql_error("status failed", &e))?;
    let mut out = BTreeMap::new();
    for row in rows {
        let (key, n) = row.map_err(|e| super::db::map_sql_error("status failed", &e))?;
        out.insert(key, usize::try_from(n).unwrap_or(0));
    }
    Ok(out)
}

/// The full status report: a hashing freshness check plus index facts.
pub fn status(
    conn: &Connection,
    index_path: &Path,
    root: &Path,
    worktree: &str,
) -> Result<Status, String> {
    let drift = full_check(conn, root)?;
    let (started, completed) = run_times(conn, worktree);
    let interrupted = match (started, completed) {
        (Some(_), None) => true,
        (Some(s), Some(c)) => s > c,
        _ => false,
    };
    Ok(Status {
        state: state_of(&drift),
        drift,
        schema_version: db::SCHEMA_VERSION,
        by_status: group_counts(conn, "status")?,
        by_lang: group_counts(conn, "lang")?,
        db_bytes: std::fs::metadata(index_path).map_or(0, |m| m.len()),
        last_run_started: started,
        last_run_completed: completed,
        interrupted,
    })
}

/// Changed, added and removed paths shown in a report, at most this many each.
pub const MAX_REPORTED_PATHS: usize = 50;

fn capped(paths: &[String]) -> serde_json::Value {
    let shown: Vec<&String> = paths.iter().take(MAX_REPORTED_PATHS).collect();
    serde_json::json!({"count": paths.len(), "paths": shown})
}

impl Status {
    /// The `results` object for the JSON envelope.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema_version": self.schema_version,
            "changed": capped(&self.drift.changed),
            "added": capped(&self.drift.added),
            "removed": capped(&self.drift.removed),
            "files_by_status": self.by_status,
            "files_by_language": self.by_lang,
            "db_bytes": self.db_bytes,
            "last_run_started": self.last_run_started,
            "last_run_completed": self.last_run_completed,
            "interrupted": self.interrupted,
        })
    }

    /// A short text report.
    pub fn to_text(&self) -> String {
        let mut out = format!(
            "index: {}\nfiles: {}\n",
            self.state.as_str(),
            self.by_status
                .iter()
                .map(|(k, v)| format!("{v} {k}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        for (label, paths) in [
            ("changed", &self.drift.changed),
            ("added", &self.drift.added),
            ("removed", &self.drift.removed),
        ] {
            if paths.is_empty() {
                continue;
            }
            out.push_str(&format!("{label} ({}):\n", paths.len()));
            for path in paths.iter().take(MAX_REPORTED_PATHS) {
                out.push_str(&format!("  {path}\n"));
            }
        }
        if self.interrupted {
            out.push_str("the last index run did not finish\n");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{fx, pinned};

    #[test]
    fn an_unchanged_repo_is_fresh() {
        let _g = pinned();
        // Arrange
        let f = fx("st-fresh");
        f.write("a.rs", "fn a() {}\n");
        f.run();

        // Act
        let drift = quick_check(&f.conn(), &f.root).expect("quick");

        // Assert
        assert!(drift.is_empty(), "{drift:?}");
        assert_eq!(state_of(&drift), IndexState::Fresh);
    }

    #[test]
    fn an_edit_that_keeps_size_and_mtime_is_stale_for_status_and_fresh_for_quick_check() {
        let _g = pinned();
        // Arrange
        let f = fx("st-samesize");
        f.write("a.rs", "fn aaa() {}\n");
        f.set_mtime("a.rs", 3_000_000);
        f.run();
        f.write("a.rs", "fn bbb() {}\n");
        f.set_mtime("a.rs", 3_000_000);

        // Act
        let quick = quick_check(&f.conn(), &f.root).expect("quick");
        let full = full_check(&f.conn(), &f.root).expect("full");

        // Assert
        assert!(quick.is_empty());
        assert_eq!(full.changed, vec!["a.rs"]);
    }

    #[test]
    fn an_added_file_is_stale_with_its_path() {
        let _g = pinned();
        // Arrange
        let f = fx("st-added");
        f.write("a.rs", "fn a() {}\n");
        f.run();
        f.write("new.rs", "fn n() {}\n");

        // Act
        let drift = quick_check(&f.conn(), &f.root).expect("quick");

        // Assert
        assert_eq!(drift.added, vec!["new.rs"]);
        assert!(drift.changed.is_empty() && drift.removed.is_empty());
        assert_eq!(state_of(&drift), IndexState::Stale);
    }

    #[test]
    fn a_size_change_and_an_mtime_only_change_are_both_changed() {
        let _g = pinned();
        // Arrange
        let f = fx("st-changed");
        f.write("size.rs", "fn a() {}\n");
        f.write("time.rs", "fn b() {}\n");
        f.set_mtime("time.rs", 4_000_000);
        f.run();
        f.write("size.rs", "fn a() {}\nfn more() {}\n");
        f.set_mtime("time.rs", 4_000_100);

        // Act
        let drift = quick_check(&f.conn(), &f.root).expect("quick");

        // Assert
        assert_eq!(drift.changed, vec!["size.rs", "time.rs"]);
    }

    #[test]
    fn a_deleted_file_is_removed() {
        let _g = pinned();
        // Arrange
        let f = fx("st-removed");
        f.write("gone.rs", "fn g() {}\n");
        f.write("stay.rs", "fn s() {}\n");
        f.git_add_all();
        f.run();
        std::fs::remove_file(f.root.join("gone.rs")).expect("rm");

        // Act
        let drift = quick_check(&f.conn(), &f.root).expect("quick");

        // Assert
        assert_eq!(drift.removed, vec!["gone.rs"]);
    }

    #[test]
    fn a_binary_and_an_over_cap_file_stay_fresh_after_indexing() {
        let _g = pinned();
        // Arrange
        let f = fx("st-skipped");
        f.write("ok.rs", "fn ok() {}\n");
        std::fs::write(f.root.join("bin.dat"), b"a\0b").expect("bin");
        std::fs::write(f.root.join("big.txt"), vec![b'a'; 1024 * 1024 + 1]).expect("big");
        let report = f.run();

        // Act
        let drift = quick_check(&f.conn(), &f.root).expect("quick");

        // Assert
        assert_eq!(report.skipped, 2);
        assert!(drift.is_empty(), "{drift:?}");
    }

    #[test]
    fn status_reports_an_interrupted_run_and_clears_it_after_a_full_run() {
        let _g = pinned();
        // Arrange
        let f = fx("st-interrupted");
        f.write("a.rs", "fn a() {}\n");
        let stop = crate::index::Options {
            hooks: crate::index::Hooks {
                after_batch_commit: Some(Box::new(|_| Err("stop".to_string()))),
                before_file_insert: None,
                ..Default::default()
            },
            ..crate::index::Options::default()
        };
        let _ = crate::index::run(&f.root, &f.index, &f.worktree, false, &stop);

        // Act
        let stopped = status(&f.conn(), &f.index, &f.root, &f.worktree).expect("status");
        f.run();
        let done = status(&f.conn(), &f.index, &f.root, &f.worktree).expect("status");

        // Assert
        assert!(stopped.interrupted);
        assert!(!done.interrupted);
        assert_eq!(done.state, IndexState::Fresh);
    }

    #[test]
    fn fifty_and_fifty_one_changed_files_list_fifty_paths_with_the_true_count() {
        let _g = pinned();
        // Arrange
        let f = fx("st-cap");
        for i in 0..51 {
            f.write(&format!("f{i:02}.rs"), "fn a() {}\n");
        }
        f.run();
        for i in 0..50 {
            f.write(&format!("f{i:02}.rs"), "fn b() {}\nfn c() {}\n");
        }
        let fifty = status(&f.conn(), &f.index, &f.root, &f.worktree)
            .expect("status")
            .to_json();
        f.write("f50.rs", "fn b() {}\nfn c() {}\n");

        // Act
        let fifty_one = status(&f.conn(), &f.index, &f.root, &f.worktree)
            .expect("status")
            .to_json();

        // Assert
        assert_eq!(fifty["changed"]["count"], 50);
        assert_eq!(fifty["changed"]["paths"].as_array().map(Vec::len), Some(50));
        assert_eq!(fifty_one["changed"]["count"], 51);
        assert_eq!(
            fifty_one["changed"]["paths"].as_array().map(Vec::len),
            Some(50)
        );
    }

    #[test]
    fn two_status_reports_on_the_same_state_are_byte_identical() {
        let _g = pinned();
        // Arrange
        let f = fx("st-deterministic");
        f.write("a.rs", "fn a() {}\n");
        f.run();

        // Act
        let one = status(&f.conn(), &f.index, &f.root, &f.worktree)
            .expect("status")
            .to_json()
            .to_string();
        let two = status(&f.conn(), &f.index, &f.root, &f.worktree)
            .expect("status")
            .to_json()
            .to_string();

        // Assert
        assert!(!one.is_empty());
        assert_eq!(one, two);
    }

    #[test]
    fn a_damaged_manifest_names_rebuild() {
        let _g = pinned();
        // Arrange
        let f = fx("status-damaged");
        f.write("a.rs", "fn needle() {}\n");
        f.run();
        let conn = f.conn();
        crate::testkit::damage_pages(&f);

        // Act
        let got = status(&conn, &f.index, &f.root, &f.worktree);

        // Assert
        let Err(message) = got else {
            panic!("the query must fail")
        };
        assert!(message.contains("--rebuild"), "{message}");
    }

    #[test]
    fn an_interrupted_run_is_reported_only_for_its_own_worktree() {
        let _g = pinned();
        // Arrange
        let f = fx("st-interrupted-own");
        f.write("a.rs", "fn a() {}\n");
        f.run();
        let elsewhere = crate::walker::test_repo::init("st-interrupted-other");
        crate::walker::test_repo::write(&elsewhere, "b.rs", b"fn b() {}\n");
        let stop = crate::index::Options {
            hooks: crate::index::Hooks {
                after_batch_commit: Some(Box::new(|_| Err("stop".to_string()))),
                before_file_insert: None,
                ..crate::index::Hooks::default()
            },
            ..crate::index::Options::default()
        };
        let _ = crate::index::run(&elsewhere, &f.index, "other", false, &stop);

        // Act
        let own = status(&f.conn(), &f.index, &f.root, &f.worktree).expect("status");

        // Assert
        assert!(!own.interrupted);
        assert_eq!(own.state, IndexState::Fresh);
    }
}
