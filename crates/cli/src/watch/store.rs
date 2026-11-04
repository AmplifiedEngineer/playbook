// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `watch.db`: the polling cursors, the events already seen, and the queue of
//! work. A cursor move and the work it produced are written in one
//! transaction, so a kill at any instruction leaves either both or neither.

use std::fmt;
use std::fs::OpenOptions;
use std::io;
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Transaction};

/// The schema this code writes and understands.
const SCHEMA_VERSION: i64 = 1;
/// Items waiting for an action. A full queue sends new items to the dead list.
pub const QUEUE_LIMIT: i64 = 500;

#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    Sql(rusqlite::Error),
    /// The database was written by a newer playbook.
    TooNew {
        found: i64,
    },
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "watch database file: {e}"),
            Self::Sql(e) => write!(f, "watch database: {e}"),
            Self::TooNew { found } => write!(
                f,
                "watch.db has schema version {found}, newer than this playbook understands ({SCHEMA_VERSION}); update playbook"
            ),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sql(e)
    }
}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Where one source stopped. The ETag belongs to the exact URL it came from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cursor {
    pub since: i64,
    pub etag_url: Option<String>,
    pub etag: Option<String>,
    pub next_poll_at: i64,
}

/// One event that a rule matched and that should become work.
#[derive(Debug, Clone)]
pub struct NewWork<'a> {
    /// `<source>:<object id>:<kind>[:<sha>]`, unique per event and rule.
    pub key: &'a str,
    pub rule: &'a str,
    /// `owner/name` and the issue or pull request number the action touches.
    pub repo: &'a str,
    pub object: u64,
    pub body_hash: [u8; 32],
}

/// What happened to a `NewWork`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Insert {
    Queued(i64),
    /// The queue was full, so the item went straight to the dead list.
    Dead(i64),
    /// The event key was seen before.
    Duplicate,
}

/// A stored work row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkRow {
    pub key: String,
    pub rule: String,
    pub repo: String,
    pub object: u64,
    pub body_hash: Vec<u8>,
    pub state: String,
    pub reason: Option<String>,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens or creates `path`, owner-only, and brings the schema up to date.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            crate::common::atomic::ensure_private_dir(parent)?;
        }
        create_private(path)?;
        let conn = Connection::open(path)?;
        let found: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if found > SCHEMA_VERSION {
            return Err(StoreError::TooNew { found });
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS cursor (
                 source TEXT PRIMARY KEY,
                 since INTEGER NOT NULL,
                 etag_url TEXT,
                 etag TEXT,
                 next_poll_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS seen (
                 key TEXT PRIMARY KEY,
                 first_seen INTEGER NOT NULL,
                 body_hash BLOB NOT NULL
             );
             CREATE TABLE IF NOT EXISTS work (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 key TEXT NOT NULL UNIQUE,
                 rule TEXT NOT NULL,
                 repo TEXT NOT NULL,
                 object INTEGER NOT NULL,
                 body_hash BLOB NOT NULL,
                 state TEXT NOT NULL,
                 reason TEXT,
                 created_at INTEGER NOT NULL
             );",
        )?;
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?;
        Ok(Self { conn })
    }

    pub fn cursor(&self, source: &str) -> Result<Option<Cursor>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT since, etag_url, etag, next_poll_at FROM cursor WHERE source = ?1",
                [source],
                |r| {
                    Ok(Cursor {
                        since: r.get(0)?,
                        etag_url: r.get(1)?,
                        etag: r.get(2)?,
                        next_poll_at: r.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    /// The ids of work rows in `state`, oldest first.
    pub fn ids_in_state(&self, state: &str) -> Result<Vec<i64>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM work WHERE state = ?1 ORDER BY id")?;
        let ids = stmt.query_map([state], |r| r.get(0))?;
        Ok(ids.collect::<Result<_, _>>()?)
    }

    pub fn work(&self, id: i64) -> Result<Option<WorkRow>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT key, rule, repo, object, body_hash, state, reason FROM work WHERE id = ?1",
                [id],
                |r| {
                    Ok(WorkRow {
                        key: r.get(0)?,
                        rule: r.get(1)?,
                        repo: r.get(2)?,
                        object: r.get::<_, i64>(3)?.try_into().unwrap_or(0),
                        body_hash: r.get(4)?,
                        state: r.get(5)?,
                        reason: r.get(6)?,
                    })
                },
            )
            .optional()?)
    }

    /// Runs `body` in one transaction. An `Err` rolls everything back.
    pub fn transaction<T>(
        &mut self,
        body: impl FnOnce(&Tx<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let tx = self.conn.transaction()?;
        let wrapped = Tx { tx };
        let out = body(&wrapped)?;
        wrapped.tx.commit()?;
        Ok(out)
    }
}

/// The writes allowed inside [`Store::transaction`].
pub struct Tx<'a> {
    tx: Transaction<'a>,
}

impl Tx<'_> {
    pub fn set_cursor(&self, source: &str, cursor: &Cursor) -> Result<(), StoreError> {
        self.tx.execute(
            "INSERT INTO cursor (source, since, etag_url, etag, next_poll_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(source) DO UPDATE SET since = ?2, etag_url = ?3, etag = ?4, next_poll_at = ?5",
            params![source, cursor.since, cursor.etag_url, cursor.etag, cursor.next_poll_at],
        )?;
        Ok(())
    }

    /// Moves a work row to `state`, with the reason or outcome that explains it.
    pub fn set_state(&self, id: i64, state: &str, reason: Option<&str>) -> Result<(), StoreError> {
        self.tx.execute(
            "UPDATE work SET state = ?2, reason = ?3 WHERE id = ?1",
            params![id, state, reason],
        )?;
        Ok(())
    }

    /// Records the event as seen and queues its work, unless the key is known.
    pub fn insert_work(&self, item: &NewWork<'_>, now: i64) -> Result<Insert, StoreError> {
        let fresh = self.tx.execute(
            "INSERT OR IGNORE INTO seen (key, first_seen, body_hash) VALUES (?1, ?2, ?3)",
            params![item.key, now, item.body_hash.as_slice()],
        )?;
        if fresh == 0 {
            return Ok(Insert::Duplicate);
        }
        let pending: i64 = self.tx.query_row(
            "SELECT COUNT(*) FROM work WHERE state IN ('queued', 'proposed', 'approved', 'running')",
            [],
            |r| r.get(0),
        )?;
        let (state, reason) = if pending >= QUEUE_LIMIT {
            ("dead", Some("queue_full"))
        } else {
            ("queued", None)
        };
        self.tx.execute(
            "INSERT INTO work (key, rule, repo, object, body_hash, state, reason, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                item.key,
                item.rule,
                item.repo,
                i64::try_from(item.object).unwrap_or(i64::MAX),
                item.body_hash.as_slice(),
                state,
                reason,
                now
            ],
        )?;
        let id = self.tx.last_insert_rowid();
        Ok(if state == "dead" {
            Insert::Dead(id)
        } else {
            Insert::Queued(id)
        })
    }
}

/// Creates `path` owner-only if it is missing. SQLite would create it with
/// the umask, which can be world readable.
fn create_private(path: &Path) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(tag: &str) -> std::path::PathBuf {
        crate::common::test_support::scratch_dir(tag).join("data/watch.db")
    }

    fn item(key: &str) -> NewWork<'_> {
        NewWork {
            key,
            rule: "label-new-issues",
            repo: "o/r",
            object: 1,
            body_hash: [7; 32],
        }
    }

    fn cursor(since: i64) -> Cursor {
        Cursor {
            since,
            etag_url: Some("https://example.invalid/x".into()),
            etag: Some("W/\"abc\"".into()),
            next_poll_at: since + 60,
        }
    }

    #[test]
    fn an_aborted_transaction_leaves_neither_the_cursor_nor_the_work() {
        // Arrange
        let path = db("watch-store-abort");
        let mut store = Store::open(&path).unwrap();

        // Act
        let result: Result<(), StoreError> = store.transaction(|tx| {
            tx.set_cursor("issues", &cursor(100))?;
            tx.insert_work(&item("issue:1:opened"), 100)?;
            Err(StoreError::TooNew { found: 99 })
        });
        drop(store);
        let reopened = Store::open(&path).unwrap();

        // Assert
        assert!(result.is_err());
        assert_eq!(reopened.cursor("issues").unwrap(), None);
        assert_eq!(reopened.work(1).unwrap(), None);
    }

    #[test]
    fn a_committed_transaction_keeps_the_cursor_and_the_work() {
        let path = db("watch-store-commit");
        let mut store = Store::open(&path).unwrap();
        let inserted = store
            .transaction(|tx| {
                tx.set_cursor("issues", &cursor(100))?;
                tx.insert_work(&item("issue:1:opened"), 100)
            })
            .unwrap();
        let Insert::Queued(id) = inserted else {
            panic!("expected a queued item");
        };
        assert_eq!(store.cursor("issues").unwrap(), Some(cursor(100)));
        assert_eq!(store.work(id).unwrap().unwrap().state, "queued");
    }

    #[test]
    fn the_same_event_key_twice_is_a_duplicate() {
        let mut store = Store::open(&db("watch-store-dup")).unwrap();
        let second = store
            .transaction(|tx| {
                tx.insert_work(&item("comment:9:created"), 1)?;
                tx.insert_work(&item("comment:9:created"), 2)
            })
            .unwrap();
        assert_eq!(second, Insert::Duplicate);
    }

    #[test]
    fn a_store_from_a_newer_playbook_is_refused() {
        let path = db("watch-store-newer");
        drop(Store::open(&path).unwrap());
        Connection::open(&path)
            .unwrap()
            .execute_batch("PRAGMA user_version = 99")
            .unwrap();
        let err = Store::open(&path).err().expect("must refuse");
        assert!(matches!(err, StoreError::TooNew { found: 99 }));
        assert!(err.to_string().contains("update playbook"));
    }

    #[cfg(unix)]
    #[test]
    fn the_database_is_owner_only_and_its_folder_private() {
        use std::os::unix::fs::PermissionsExt;
        let path = db("watch-store-mode");
        drop(Store::open(&path).unwrap());
        let file = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let dir = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!((file, dir), (0o600, 0o700));
    }

    #[test]
    fn the_501st_pending_item_goes_to_the_dead_list() {
        let mut store = Store::open(&db("watch-store-full")).unwrap();
        let keys: Vec<String> = (0..=QUEUE_LIMIT)
            .map(|n| format!("issue:{n}:opened"))
            .collect();
        let last = store
            .transaction(|tx| {
                let mut last = Insert::Duplicate;
                for key in &keys {
                    last = tx.insert_work(&item(key), 1)?;
                }
                Ok(last)
            })
            .unwrap();
        let Insert::Dead(id) = last else {
            panic!("expected the dead list, got {last:?}");
        };
        let row = store.work(id).unwrap().unwrap();
        assert_eq!(
            (row.state.as_str(), row.reason.as_deref()),
            ("dead", Some("queue_full"))
        );
    }
}
