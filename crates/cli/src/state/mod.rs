// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Small key-value bookkeeping in the `state` table of `config.db`: the
//! migration record and the worktree sweep rate-limit markers used to be loose
//! files under the playbook root.
//!
//! Keys are slash separated: `migrations/applied/<id>`, `migrations/shipped/<key>`
//! and `worktree-sweep/<slug>`. Writes go through transactions. The first open
//! imports the old files in one transaction and renames each to `.migrated`;
//! nothing is deleted.

use crate::common::slug::slugify;
use crate::config::store;
use playbook_store::{params, Connection, OptionalExtension};
use std::fs;
use std::path::{Path, PathBuf};

pub const MIGRATIONS_APPLIED: &str = "migrations/applied/";
pub const MIGRATIONS_SHIPPED: &str = "migrations/shipped/";
pub const SWEEP_PREFIX: &str = "worktree-sweep/";

const IMPORTED_FLAG: &str = "state_imported";
const LEGACY_MIGRATIONS_FILE: &str = "migrations.state";
const LEGACY_SWEEP_PREFIX: &str = "worktree-sweep-marker-";

#[derive(Debug)]
pub struct StateError(pub String);

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "state store: {}", self.0)
    }
}

impl std::error::Error for StateError {}

fn err(e: impl std::fmt::Display) -> StateError {
    StateError(e.to_string())
}

/// Open the database under `root` and run the one-time import of the old
/// state files.
pub fn open(root: &Path) -> Result<Connection, StateError> {
    let conn = store::open(root).map_err(err)?;
    import_legacy_once(&conn, root)?;
    Ok(conn)
}

/// A transaction handle: every read and write inside it is atomic.
pub struct Tx<'a> {
    conn: &'a Connection,
}

impl Tx<'_> {
    pub fn get(&self, key: &str) -> Result<Option<String>, StateError> {
        self.conn
            .query_row(
                "SELECT value FROM state WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
            .map_err(err)
    }

    pub fn set(&self, key: &str, value: &str) -> Result<(), StateError> {
        self.conn
            .execute(
                "INSERT INTO state (key, value, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                params![key, value, crate::common::time::now_secs()],
            )
            .map(|_| ())
            .map_err(err)
    }

    pub fn delete(&self, key: &str) -> Result<bool, StateError> {
        self.conn
            .execute("DELETE FROM state WHERE key = ?1", params![key])
            .map(|n| n > 0)
            .map_err(err)
    }

    /// Every `(key, value)` whose key starts with `prefix`, sorted by key.
    pub fn list(&self, prefix: &str) -> Result<Vec<(String, String)>, StateError> {
        let mut stmt = self
            .conn
            .prepare("SELECT key, value FROM state WHERE substr(key, 1, ?2) = ?1 ORDER BY key")
            .map_err(err)?;
        let rows = stmt
            .query_map(params![prefix, prefix.chars().count() as i64], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(err)
    }

    pub fn delete_prefix(&self, prefix: &str) -> Result<usize, StateError> {
        self.conn
            .execute(
                "DELETE FROM state WHERE substr(key, 1, ?2) = ?1",
                params![prefix, prefix.chars().count() as i64],
            )
            .map_err(err)
    }
}

/// Run `f` in one write transaction (`BEGIN IMMEDIATE`, so concurrent writers
/// queue on the busy timeout). A returned error rolls everything back.
pub fn transaction<T>(
    root: &Path,
    f: impl FnOnce(&Tx<'_>) -> Result<T, StateError>,
) -> Result<T, StateError> {
    let conn = open(root)?;
    conn.execute_batch("BEGIN IMMEDIATE").map_err(err)?;
    match f(&Tx { conn: &conn }) {
        Ok(value) => {
            conn.execute_batch("COMMIT").map_err(err)?;
            Ok(value)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

pub fn get(root: &Path, key: &str) -> Result<Option<String>, StateError> {
    let conn = open(root)?;
    Tx { conn: &conn }.get(key)
}

pub fn set(root: &Path, key: &str, value: &str) -> Result<(), StateError> {
    transaction(root, |tx| tx.set(key, value))
}

pub fn delete(root: &Path, key: &str) -> Result<bool, StateError> {
    transaction(root, |tx| tx.delete(key))
}

pub fn list(root: &Path, prefix: &str) -> Result<Vec<(String, String)>, StateError> {
    let conn = open(root)?;
    Tx { conn: &conn }.list(prefix)
}

/// The sweep marker key for a repo: one per repo, so one repo's sweep never
/// claims another's rate-limit slot.
pub fn sweep_key(repo_root: &Path) -> String {
    format!("{SWEEP_PREFIX}{}", slugify(&repo_root.to_string_lossy()))
}

/// The legacy files the import reads: `migrations.state` and every
/// `worktree-sweep-marker-*`.
fn legacy_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let file = root.join(LEGACY_MIGRATIONS_FILE);
    if file.is_file() {
        out.push(file);
    }
    if let Ok(entries) = fs::read_dir(root) {
        let mut markers: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        // A marker already imported carries `.migrated`.
                        // Importing it again would rename it once more on
                        // every run and store a key with the suffix.
                        .is_some_and(|n| {
                            n.starts_with(LEGACY_SWEEP_PREFIX) && !n.ends_with(".migrated")
                        })
            })
            .collect();
        markers.sort();
        out.extend(markers);
    }
    out
}

fn file_epoch(path: &Path, body: &str) -> i64 {
    body.trim().parse::<i64>().unwrap_or_else(|_| {
        fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(crate::common::time::epoch_secs_of)
            .unwrap_or(0)
    })
}

fn flagged(conn: &Connection) -> Result<bool, StateError> {
    conn.query_row(
        "SELECT 1 FROM meta WHERE key = ?1",
        params![IMPORTED_FLAG],
        |_| Ok(()),
    )
    .optional()
    .map(|r| r.is_some())
    .map_err(err)
}

/// Import the old files once, in one transaction. A file that cannot be read
/// fails the import and nothing is renamed or flagged, so the next open retries.
fn import_legacy_once(conn: &Connection, root: &Path) -> Result<(), StateError> {
    if flagged(conn)? {
        return Ok(());
    }
    conn.execute_batch("BEGIN IMMEDIATE").map_err(err)?;
    let tx = Tx { conn };
    let outcome = (|| -> Result<Vec<PathBuf>, StateError> {
        if flagged(conn)? {
            return Ok(Vec::new());
        }
        let mut done = Vec::new();
        for path in legacy_files(root) {
            let body = fs::read_to_string(&path)
                .map_err(|e| StateError(format!("{}: {e}", path.display())))?;
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == LEGACY_MIGRATIONS_FILE {
                for line in body.lines() {
                    if let Some(id) = line.strip_prefix("applied ") {
                        tx.set(&format!("{MIGRATIONS_APPLIED}{id}"), "1")?;
                    } else if let Some(rest) = line.strip_prefix("shipped ") {
                        if let Some((key, hash)) = rest.rsplit_once(' ') {
                            tx.set(&format!("{MIGRATIONS_SHIPPED}{key}"), hash)?;
                        }
                    }
                }
            } else if let Some(slug) = name.strip_prefix(LEGACY_SWEEP_PREFIX) {
                tx.set(
                    &format!("{SWEEP_PREFIX}{slug}"),
                    &file_epoch(&path, &body).to_string(),
                )?;
            }
            done.push(path);
        }
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, '1')",
            params![IMPORTED_FLAG],
        )
        .map_err(err)?;
        Ok(done)
    })();
    match outcome {
        Ok(done) => {
            conn.execute_batch("COMMIT").map_err(err)?;
            for path in done {
                let mut renamed = path.clone().into_os_string();
                renamed.push(".migrated");
                let _ = fs::rename(&path, PathBuf::from(renamed));
            }
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// How many old state files are still waiting to be imported.
pub fn pending_legacy(root: &Path) -> usize {
    if flagged_on_disk(root) {
        return 0;
    }
    legacy_files(root).len()
}

/// Rows under `prefix` read through a read-only connection, which runs no
/// schema step and no legacy import, so reading changes nothing on disk.
/// Empty when the database or its `state` table does not exist yet.
pub fn list_read_only(root: &Path, prefix: &str) -> Result<Vec<(String, String)>, StateError> {
    let path = store::db_path(root);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let conn = open_read_only(&path).map_err(err)?;
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'state')",
            [],
            |r| r.get(0),
        )
        .map_err(err)?;
    if !exists {
        return Ok(Vec::new());
    }
    Tx { conn: &conn }.list(prefix)
}

/// A read-only connection that leaves no file behind. With no `-wal` file
/// every row is in the main file, so it opens as immutable and SQLite makes
/// no `-wal` or `-shm` file; with one, a plain read-only open reads it.
pub(crate) fn open_read_only(path: &Path) -> playbook_store::Result<playbook_store::Connection> {
    use playbook_store::OpenFlags;
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    if Path::new(&wal).exists() {
        return playbook_store::Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY);
    }
    let encoded: String = path
        .to_string_lossy()
        .chars()
        .map(|c| match c {
            '%' => "%25".to_string(),
            '?' => "%3f".to_string(),
            '#' => "%23".to_string(),
            c => c.to_string(),
        })
        .collect();
    playbook_store::Connection::open_with_flags(
        format!("file:{encoded}?immutable=1"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
}

fn flagged_on_disk(root: &Path) -> bool {
    store::db_path(root).exists()
        && open_read_only(&store::db_path(root))
            .ok()
            .is_some_and(|c| flagged(&c).unwrap_or(false))
}

/// Dump the whole table as sorted `key<TAB>value` lines, for inspection.
pub fn dump(root: &Path) -> Result<Vec<(String, String)>, StateError> {
    list(root, "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::scratch_root as root;

    #[test]
    fn set_get_delete_and_list_by_prefix() {
        let r = root("state-basic");
        assert_eq!(get(&r, "a/1").unwrap(), None);
        set(&r, "a/1", "x").unwrap();
        set(&r, "a/2", "y").unwrap();
        set(&r, "b/1", "z").unwrap();
        set(&r, "a/1", "x2").unwrap();
        assert_eq!(get(&r, "a/1").unwrap().as_deref(), Some("x2"));
        assert_eq!(
            list(&r, "a/").unwrap(),
            vec![("a/1".into(), "x2".into()), ("a/2".into(), "y".into())]
        );
        assert!(delete(&r, "a/1").unwrap());
        assert!(!delete(&r, "a/1").unwrap());
        assert_eq!(list(&r, "a/").unwrap().len(), 1);
    }

    #[test]
    fn a_prefix_with_like_wildcards_matches_literally() {
        let r = root("state-like");
        set(&r, "a%b/1", "1").unwrap();
        set(&r, "axb/1", "2").unwrap();
        assert_eq!(list(&r, "a%").unwrap().len(), 1);
        assert_eq!(list(&r, "a_").unwrap().len(), 0);
    }

    #[test]
    fn a_failed_transaction_changes_nothing() {
        let r = root("state-rollback");
        set(&r, "k", "1").unwrap();
        let res: Result<(), StateError> = transaction(&r, |tx| {
            tx.set("k", "2")?;
            tx.set("new", "x")?;
            Err(StateError("boom".into()))
        });
        assert!(res.is_err());
        assert_eq!(get(&r, "k").unwrap().as_deref(), Some("1"));
        assert_eq!(get(&r, "new").unwrap(), None);
    }

    #[test]
    fn an_imported_marker_is_not_imported_or_renamed_again() {
        let r = root("state-marker-again");
        let done = r.join("worktree-sweep-marker-_tmp_repo.migrated");
        fs::write(&done, "1700000000").unwrap();
        assert_eq!(get(&r, "worktree-sweep/_tmp_repo.migrated").unwrap(), None);
        assert!(done.is_file(), "the imported marker was renamed again");
        assert!(!r
            .join("worktree-sweep-marker-_tmp_repo.migrated.migrated")
            .exists());
    }

    #[test]
    fn the_old_files_are_imported_once_and_renamed_not_deleted() {
        let r = root("state-import");
        fs::write(
            r.join("migrations.state"),
            "applied 0008-x\nshipped system-prompt abcd\nshipped skill:1.0:demo ef01\n",
        )
        .unwrap();
        fs::write(r.join("worktree-sweep-marker-_tmp_repo"), "1700000000").unwrap();
        fs::write(r.join("worktree-sweep-marker-_tmp_other"), "not a number").unwrap();
        assert_eq!(
            get(&r, "migrations/applied/0008-x").unwrap().as_deref(),
            Some("1")
        );
        assert_eq!(
            get(&r, "migrations/shipped/skill:1.0:demo")
                .unwrap()
                .as_deref(),
            Some("ef01")
        );
        assert_eq!(
            get(&r, "worktree-sweep/_tmp_repo").unwrap().as_deref(),
            Some("1700000000")
        );
        let other = get(&r, "worktree-sweep/_tmp_other").unwrap().unwrap();
        assert!(other.parse::<i64>().unwrap() > 1_600_000_000, "{other}");
        assert!(!r.join("migrations.state").exists());
        assert!(r.join("migrations.state.migrated").is_file());
        assert!(r.join("worktree-sweep-marker-_tmp_repo.migrated").is_file());
        // A file written later is not imported a second time.
        fs::write(r.join("migrations.state"), "applied late\n").unwrap();
        assert_eq!(get(&r, "migrations/applied/late").unwrap(), None);
        assert!(r.join("migrations.state").is_file());
    }

    #[test]
    fn an_unreadable_file_fails_the_import_and_renames_nothing() {
        let r = root("state-import-fail");
        fs::write(r.join("migrations.state"), "applied a\n").unwrap();
        fs::write(r.join("worktree-sweep-marker-bad"), [0xff, 0xfe, 0xfd]).unwrap();
        assert!(get(&r, "x").is_err());
        assert!(r.join("migrations.state").is_file());
        assert!(r.join("worktree-sweep-marker-bad").is_file());
    }

    #[test]
    fn sweep_keys_match_the_old_marker_slug() {
        assert_eq!(
            sweep_key(Path::new("/Users/me/my repo")),
            "worktree-sweep/_Users_me_my_repo"
        );
    }

    #[test]
    fn a_read_only_listing_creates_and_imports_nothing() {
        let r = root("read-only");
        fs::create_dir_all(&r).unwrap();
        fs::write(r.join("migrations.state"), "applied 0001-x\n").unwrap();

        assert!(list_read_only(&r, "").unwrap().is_empty());
        assert!(!store::db_path(&r).exists());
        assert!(r.join("migrations.state").is_file());

        transaction(&r, |tx| tx.set("versions/binary", "0.27.0")).unwrap();
        assert_eq!(
            list_read_only(&r, "versions/").unwrap(),
            vec![("versions/binary".to_string(), "0.27.0".to_string())]
        );
    }
}
