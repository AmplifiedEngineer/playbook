// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook code`: a local, disposable index of the repository's source that
//! answers with exact paths, line ranges and short excerpts.
//!
//! The public functions here are the library surface. The command line layer
//! in `cli` calls them and only renders. This module reaches the host program
//! only through `host`.

pub mod cleanup;
pub mod cli;
pub mod context;
pub mod db;
pub mod host;
pub mod index;
pub mod lang;
#[cfg(test)]
mod language_list_tests;
pub mod location;
pub mod output;
#[cfg(test)]
mod parse_guard_tests;
pub mod ranked;
pub mod refs;
pub mod search;
pub mod source;
pub mod status;
#[cfg(test)]
pub(crate) mod testkit;
pub mod walker;

pub use cli::{run, CodeCommand, Env};

use std::path::{Path, PathBuf};

/// The result of an indexing run.
pub struct IndexOutcome {
    pub index_path: PathBuf,
    pub report: index::Report,
}

/// Builds or refreshes the index for the repository containing `cwd`.
pub fn index(cwd: &Path, rebuild: bool) -> Result<IndexOutcome, String> {
    index_with(cwd, rebuild, &index::Options::default())
}

/// [`index`] with the run's options passed in.
pub fn index_with(
    cwd: &Path,
    rebuild: bool,
    opts: &index::Options,
) -> Result<IndexOutcome, String> {
    let root = location::repo_root(cwd)?;
    let index_path = location::index_path()?;
    let report = index::run(&root, &index_path, &host::worktree_id(), rebuild, opts)?;
    Ok(IndexOutcome { index_path, report })
}

/// What `status` found.
pub enum StatusOutcome {
    /// No index file exists for this worktree.
    Missing { index_path: PathBuf },
    Found {
        index_path: PathBuf,
        status: status::Status,
    },
}

/// Drops the rows of a removed worktree from the repo's store and deletes its
/// run lock file. The store is not created when it is missing. A busy store or
/// a lock that cannot be taken is not an error: the dead-root rule of the next
/// index run drops the row later.
pub fn forget_worktree(data: &Path, slug: &str, worktree: &str) -> Result<(), String> {
    let Some(dir) = playbook_data::code_dir_in(data, slug, worktree) else {
        return Ok(());
    };
    let store = dir.join(format!("store-v{}.sqlite3", db::SCHEMA_VERSION));
    // A held run lock means that worktree is indexing now, so nothing changes.
    let lock_path = dir.join(format!("{worktree}.lock"));
    let held = match std::fs::OpenOptions::new().write(true).open(&lock_path) {
        Ok(file) => match file.try_lock() {
            Ok(()) => Some(file),
            Err(_) => return Ok(()),
        },
        Err(_) => None,
    };
    match db::open_maintenance(&store) {
        Ok(mut conn) => {
            let hooks = db::TxHooks::default();
            let deleted = db::write_tx(&mut conn, &hooks).and_then(|tx| {
                tx.execute("DELETE FROM worktrees WHERE key = ?1", [worktree])?;
                db::commit_tx(tx, &hooks)
            });
            match deleted {
                Ok(()) => {}
                Err(e) if is_busy(&e) => return Ok(()),
                Err(e) => return Err(db::map_sql_error("failed to forget the worktree", &e)),
            }
        }
        Err(db::DbError::Missing) => {}
        Err(db::DbError::Failed(_)) => return Ok(()),
        Err(e) => return Err(e.to_string()),
    }
    drop(held);
    let _ = std::fs::remove_file(&lock_path);
    Ok(())
}

fn is_busy(err: &playbook_store::Error) -> bool {
    err.sqlite_error_code() == Some(playbook_store::ErrorCode::DatabaseBusy)
}

/// The full freshness report for the repository containing `cwd`.
pub fn status(cwd: &Path) -> Result<StatusOutcome, String> {
    let root = location::repo_root(cwd)?;
    let index_path = location::index_path()?;
    let worktree = host::worktree_id();
    match db::open_query(&index_path, &worktree) {
        Err(db::DbError::Missing) => Ok(StatusOutcome::Missing { index_path }),
        Err(e) => Err(e.to_string()),
        Ok(conn) => {
            let status = status::status(&conn, &index_path, &root, &worktree)?;
            Ok(StatusOutcome::Found { index_path, status })
        }
    }
}

/// A search answer plus the freshness of the index it came from.
pub struct SearchOutcome {
    pub index_path: PathBuf,
    pub drift: status::Drift,
    pub result: search::SearchResult,
}

/// Searches indexed text for the repository containing `cwd`. Runs the quick
/// freshness check first and reports its drift, so a caller can warn.
pub fn search_text(cwd: &Path, opts: &search::SearchOptions) -> Result<SearchOutcome, String> {
    lookup(cwd, opts, search::text_search)
}

fn lookup(
    cwd: &Path,
    opts: &search::SearchOptions,
    run: fn(
        &playbook_store::Connection,
        &Path,
        &search::SearchOptions,
    ) -> Result<search::SearchResult, String>,
) -> Result<SearchOutcome, String> {
    let root = location::repo_root(cwd)?;
    let index_path = location::index_path()?;
    let conn = db::open_query(&index_path, &host::worktree_id()).map_err(|e| e.to_string())?;
    let drift = status::quick_check(&conn, &root)?;
    let result = run(&conn, &root, opts)?;
    Ok(SearchOutcome {
        index_path,
        drift,
        result,
    })
}

/// Ranked search over symbols, imports and text.
pub fn search_ranked(cwd: &Path, opts: &search::SearchOptions) -> Result<SearchOutcome, String> {
    lookup(cwd, opts, ranked::ranked_search)
}

/// Symbols whose name matches, by the name tiers only.
pub fn symbol_lookup(cwd: &Path, opts: &search::SearchOptions) -> Result<SearchOutcome, String> {
    lookup(cwd, opts, |conn, root, opts| {
        let (hits, total) = ranked::symbol_hits(conn, root, opts, opts.limit)?;
        let omitted = total.saturating_sub(hits.len());
        Ok(search::SearchResult {
            hits,
            truncated: omitted > 0,
            omitted,
            ..search::SearchResult::default()
        })
    })
}

/// Reference candidates plus the freshness of the index they came from.
pub struct RefsOutcome {
    pub index_path: PathBuf,
    pub drift: status::Drift,
    pub result: refs::RefsResult,
}

/// Identifier uses of a name for the repository containing `cwd`.
pub fn find_refs(cwd: &Path, opts: &refs::RefsOptions) -> Result<RefsOutcome, String> {
    let root = location::repo_root(cwd)?;
    let index_path = location::index_path()?;
    let conn = db::open_query(&index_path, &host::worktree_id()).map_err(|e| e.to_string())?;
    let drift = status::quick_check(&conn, &root)?;
    let result = refs::find_refs(&conn, &root, opts)?;
    Ok(RefsOutcome {
        index_path,
        drift,
        result,
    })
}

/// A context answer plus the freshness of the index it came from.
pub struct ContextOutcome {
    pub index_path: PathBuf,
    pub drift: status::Drift,
    pub result: context::ContextResult,
}

/// Gathers code for a task described in words, for the repository containing `cwd`.
pub fn build_context(cwd: &Path, opts: &context::ContextOptions) -> Result<ContextOutcome, String> {
    let root = location::repo_root(cwd)?;
    let index_path = location::index_path()?;
    let conn = db::open_query(&index_path, &host::worktree_id()).map_err(|e| e.to_string())?;
    let drift = status::quick_check(&conn, &root)?;
    let result = context::build(&conn, &root, opts)?;
    Ok(ContextOutcome {
        index_path,
        drift,
        result,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{self, Options};
    use crate::testkit::{fx, pinned};
    use playbook_store::Connection;

    fn store_in(data: &Path) -> PathBuf {
        data.join("code/o/r")
            .join(format!("store-v{}.sqlite3", db::SCHEMA_VERSION))
    }

    /// A data folder whose store holds the worktrees `a` and `b`.
    fn two_worktrees(tag: &str) -> PathBuf {
        let a = fx(tag);
        a.write("a.rs", "fn a() {}\n");
        let data = a.index.parent().expect("dir").join("data");
        let store = store_in(&data);
        index::run(&a.root, &store, "a", false, &Options::default()).expect("a");
        index::run(&a.root, &store, "b", false, &Options::default()).expect("b");
        data
    }

    fn rows(store: &Path, sql: &str) -> i64 {
        Connection::open(store)
            .expect("open")
            .query_row(sql, [], |r| r.get(0))
            .expect("count")
    }

    #[test]
    fn forgetting_a_worktree_drops_its_rows_and_its_lock_and_keeps_the_sibling() {
        let _g = pinned();
        // Arrange
        let data = two_worktrees("forget-rows");
        let store = store_in(&data);
        let lock = data.join("code/o/r/a.lock");
        assert!(lock.exists());

        // Act
        forget_worktree(&data, "o/r", "a").expect("forget");

        // Assert
        assert_eq!(rows(&store, "SELECT COUNT(*) FROM worktrees"), 1);
        assert_eq!(
            rows(&store, "SELECT COUNT(*) FROM worktrees WHERE key = 'b'"),
            1
        );
        assert!(!lock.exists());
        assert!(data.join("code/o/r/b.lock").exists());
    }

    #[test]
    fn the_manifest_goes_with_the_worktree_through_the_cascade() {
        let _g = pinned();
        // Arrange
        let data = two_worktrees("forget-cascade");
        let store = store_in(&data);
        assert_eq!(rows(&store, "SELECT COUNT(*) FROM manifest"), 2);

        // Act
        forget_worktree(&data, "o/r", "a").expect("forget");

        // Assert
        assert_eq!(rows(&store, "SELECT COUNT(*) FROM manifest"), 1);
    }

    #[test]
    fn a_missing_store_is_not_created_and_its_lock_file_goes() {
        // Arrange
        let data = playbook_common::test_support::scratch_dir("forget-no-store");
        let dir = data.join("code/o/r");
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("a.lock"), b"").expect("lock");

        // Act
        forget_worktree(&data, "o/r", "a").expect("forget");

        // Assert
        assert!(!dir.join("a.lock").exists());
        assert!(!store_in(&data).exists());
    }

    #[test]
    fn an_unsafe_name_changes_nothing() {
        let _g = pinned();
        // Arrange
        let data = two_worktrees("forget-unsafe");
        let store = store_in(&data);
        let keep = data.join("code/o/r/a.lock");

        // Act
        for (slug, worktree) in [
            ("o/r", ".."),
            ("o/r", "a/b"),
            ("o/r", "a\\b"),
            ("o/..", "a"),
            ("/o/r", "a"),
            ("o", "a"),
        ] {
            forget_worktree(&data, slug, worktree).expect("ignored");

            // Assert
            assert!(
                playbook_data::code_dir_in(&data, slug, worktree).is_none(),
                "{slug} {worktree}"
            );
        }
        assert_eq!(rows(&store, "SELECT COUNT(*) FROM worktrees"), 2);
        assert!(keep.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_owner_folder_changes_nothing() {
        let _g = pinned();
        // Arrange
        let data = two_worktrees("forget-symlink");
        let store = store_in(&data);
        let link = data.join("code/linked");
        std::os::unix::fs::symlink(data.join("code/o"), &link).expect("symlink");

        // Act
        forget_worktree(&data, "linked/r", "a").expect("ignored");

        // Assert
        assert_eq!(rows(&store, "SELECT COUNT(*) FROM worktrees"), 2);
        assert!(data.join("code/o/r/a.lock").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_repo_folder_changes_nothing() {
        let _g = pinned();
        // Arrange
        let data = two_worktrees("forget-symlink-repo");
        let store = store_in(&data);
        std::os::unix::fs::symlink(data.join("code/o/r"), data.join("code/o/r2")).expect("link");

        // Act
        forget_worktree(&data, "o/r2", "a").expect("ignored");

        // Assert
        assert_eq!(rows(&store, "SELECT COUNT(*) FROM worktrees"), 2);
        assert!(data.join("code/o/r/a.lock").exists());
    }

    #[test]
    fn a_held_run_lock_keeps_the_row_and_the_lock() {
        let _g = pinned();
        // Arrange
        let data = two_worktrees("forget-running");
        let store = store_in(&data);
        let lock = data.join("code/o/r/a.lock");
        let running = std::fs::OpenOptions::new()
            .write(true)
            .open(&lock)
            .expect("open");
        running.try_lock().expect("take");

        // Act
        forget_worktree(&data, "o/r", "a").expect("forget");

        // Assert
        assert_eq!(rows(&store, "SELECT COUNT(*) FROM worktrees"), 2);
        assert!(lock.exists());
    }

    #[test]
    fn a_store_being_rebuilt_keeps_the_row_and_the_lock() {
        let _g = pinned();
        // Arrange
        let data = two_worktrees("forget-rebuilding");
        let store = store_in(&data);
        let rebuilding = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(store.with_extension("lock"))
            .expect("store lock");
        rebuilding.lock().expect("exclusive");

        // Act
        let got = forget_worktree(&data, "o/r", "a");

        // Assert
        assert!(got.is_ok());
        assert_eq!(rows(&store, "SELECT COUNT(*) FROM worktrees"), 2);
        assert!(data.join("code/o/r/a.lock").exists());
    }

    #[test]
    fn a_damaged_store_reports_an_error() {
        let _g = pinned();
        // Arrange
        let data = two_worktrees("forget-damaged");
        let store = store_in(&data);
        Connection::open(&store)
            .expect("open")
            .execute_batch("DROP TABLE manifest; DROP TABLE worktrees;")
            .expect("damage");

        // Act
        let got = forget_worktree(&data, "o/r", "a");

        // Assert
        assert!(got.is_err());
    }

    #[test]
    fn a_store_held_busy_keeps_the_row_and_returns_ok() {
        let _g = pinned();
        // Arrange
        let data = two_worktrees("forget-busy");
        let store = store_in(&data);
        let holder = Connection::open(&store).expect("holder");
        holder.execute_batch("BEGIN IMMEDIATE").expect("hold");

        // Act
        let got = forget_worktree(&data, "o/r", "a");

        // Assert
        assert!(got.is_ok());
        holder.execute_batch("ROLLBACK").expect("release");
        assert_eq!(rows(&store, "SELECT COUNT(*) FROM worktrees"), 2);
    }
}
