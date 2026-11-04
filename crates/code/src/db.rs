// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The code index database. It is a cache: a schema version mismatch or a
//! corrupt file is deleted and rebuilt by `index`, and query commands never
//! delete anything. One store file serves every worktree of a repo, so each
//! open holds a shared lock on `store-v<N>.lock` and the delete step takes the
//! exclusive one, which keeps it from pulling the file out from under a
//! sibling. Connection setup follows the usage store's ordering.

use super::host;
use playbook_common::sqlite::{retry_busy, OPEN_BUDGET};
use rusqlite::{Connection, ErrorCode, OptionalExtension, Transaction, TransactionBehavior};
use std::fs::{File, OpenOptions, TryLockError};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Bumped when the table layout changes. The store file name carries it, so a
/// build never opens a file with another layout.
pub const SCHEMA_VERSION: i64 = 4;

const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

/// How often a query asks for the shared store lock while a rebuild holds the
/// exclusive one, and the wait between asks.
const LOCK_POLL_TRIES: u32 = 5;
const LOCK_POLL_MS: u64 = 10;

/// Largest size the WAL file keeps on disk after a checkpoint, in bytes.
pub const WAL_SIZE_LIMIT: i64 = 64 * 1024 * 1024;

/// Test hooks for [`write_tx`]. Production passes [`TxHooks::default`], which
/// holds none. A `fail_*` hook returns the error to inject.
#[derive(Default)]
pub struct TxHooks {
    /// Runs each time a write transaction is about to begin.
    pub on_write_tx: Option<Box<dyn Fn() + Send + Sync>>,
    /// Makes the transaction fail at `BEGIN` when it returns an error.
    pub fail_begin: Option<Box<dyn Fn() -> rusqlite::Result<()> + Send + Sync>>,
    /// Makes [`commit_tx`] fail, and roll back, when it returns an error.
    pub fail_commit: Option<Box<dyn Fn() -> rusqlite::Result<()> + Send + Sync>>,
}

/// Begins a write transaction in `IMMEDIATE` mode. A deferred transaction that
/// reads and then writes gets `SQLITE_BUSY_SNAPSHOT` when a sibling connection
/// commits in between, and the busy timeout does not retry that error. Every
/// write path begins its transaction here and nowhere else.
pub fn write_tx<'c>(
    conn: &'c mut Connection,
    hooks: &TxHooks,
) -> rusqlite::Result<Transaction<'c>> {
    if let Some(hook) = &hooks.on_write_tx {
        hook();
    }
    if let Some(fail) = &hooks.fail_begin {
        fail()?;
    }
    conn.transaction_with_behavior(TransactionBehavior::Immediate)
}

/// Commits a transaction from [`write_tx`]. When `fail_commit` injects an
/// error the transaction is dropped, which rolls it back.
pub fn commit_tx(tx: Transaction<'_>, hooks: &TxHooks) -> rusqlite::Result<()> {
    if let Some(fail) = &hooks.fail_commit {
        fail()?;
    }
    tx.commit()
}

/// How a caller opens the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    /// Used only by `index` after it holds the run lock: creates the file and
    /// deletes and recreates it once when it is corrupt or from another
    /// version, if no sibling worktree is using it.
    Index,
    /// Used by every other command: never creates or deletes anything.
    Query,
}

/// Why the index could not be opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbError {
    /// No index file exists yet (query mode only).
    Missing,
    /// The file is corrupt or from another schema version and query mode does
    /// not delete it. The message names `--rebuild`.
    NeedsRebuild(String),
    /// Any other failure.
    Failed(String),
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbError::Missing => write!(f, "no index yet; run `{}` first", host::command("index")),
            DbError::NeedsRebuild(msg) | DbError::Failed(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for DbError {}

const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS worktrees (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    key TEXT NOT NULL UNIQUE,
    root TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    tool_version TEXT NOT NULL,
    last_run_started INTEGER,
    last_run_completed INTEGER
) STRICT;
CREATE TABLE IF NOT EXISTS content (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    sha256 TEXT NOT NULL,
    grammar TEXT NOT NULL,
    parser TEXT NOT NULL,
    partial INTEGER NOT NULL CHECK (partial IN (0, 1)),
    UNIQUE (sha256, grammar, parser)
) STRICT;
CREATE TABLE IF NOT EXISTS manifest (
    worktree INTEGER NOT NULL REFERENCES worktrees(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    content INTEGER REFERENCES content(id) ON DELETE RESTRICT,
    lang TEXT,
    sha256 TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('ok', 'partial', 'failed', 'skipped')),
    error TEXT,
    PRIMARY KEY (worktree, path)
) STRICT;
CREATE INDEX IF NOT EXISTS manifest_content ON manifest(content, worktree);
CREATE TABLE IF NOT EXISTS symbols (
    id INTEGER PRIMARY KEY,
    content INTEGER NOT NULL REFERENCES content(id) ON DELETE CASCADE,
    parent_id INTEGER,
    name TEXT NOT NULL,
    qualified_name TEXT NOT NULL,
    kind TEXT NOT NULL,
    signature TEXT,
    signature_cut INTEGER NOT NULL DEFAULT 0,
    exported INTEGER NOT NULL DEFAULT 0,
    start_byte INTEGER NOT NULL,
    end_byte INTEGER NOT NULL,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS symbols_name ON symbols(name);
CREATE INDEX IF NOT EXISTS symbols_name_lower ON symbols(lower(name));
CREATE INDEX IF NOT EXISTS symbols_qualified ON symbols(qualified_name);
CREATE INDEX IF NOT EXISTS symbols_content ON symbols(content);
CREATE TABLE IF NOT EXISTS imports (
    content INTEGER NOT NULL REFERENCES content(id) ON DELETE CASCADE,
    module TEXT NOT NULL,
    names TEXT,
    line INTEGER NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS imports_content ON imports(content);
CREATE INDEX IF NOT EXISTS imports_module ON imports(module);
CREATE TABLE IF NOT EXISTS chunks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    content INTEGER NOT NULL REFERENCES content(id) ON DELETE CASCADE,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS chunks_content ON chunks(content);
CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
    body,
    content='',
    contentless_delete=1,
    tokenize=\"unicode61 tokenchars '_'\"
);
";

/// Whether this build of SQLite was compiled with FTS5.
pub fn fts5_available(conn: &Connection) -> bool {
    let Ok(mut stmt) = conn.prepare("PRAGMA compile_options") else {
        return false;
    };
    let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) else {
        return false;
    };
    let options: Vec<String> = rows.flatten().collect();
    options.iter().any(|opt| opt == "ENABLE_FTS5")
}

/// The db file and the two files SQLite keeps beside it in WAL mode, with the
/// main file last so a failed delete leaves it for the next try.
fn sibling_files(path: &Path) -> Vec<PathBuf> {
    let base = path.as_os_str().to_os_string();
    ["-wal", "-shm", ""]
        .iter()
        .map(|suffix| {
            let mut name = base.clone();
            name.push(suffix);
            PathBuf::from(name)
        })
        .collect()
}

/// Deletes the db, its WAL and its shm file. A missing file is fine.
pub fn delete_index_files(path: &Path) -> Result<(), String> {
    for file in sibling_files(path) {
        match std::fs::remove_file(&file) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("failed to delete {}: {e}", file.display())),
        }
    }
    Ok(())
}

fn rebuild_hint(path: &Path, why: &str) -> String {
    format!(
        "the code index at {} {why}; run `{}`",
        path.display(),
        host::command("index --rebuild")
    )
}

/// Test seams of [`open_index`]. Production passes [`DbHooks::default`].
#[derive(Default)]
pub struct DbHooks<'a> {
    /// Called before a new store's schema transaction begins.
    pub before_schema: Option<&'a dyn Fn()>,
    /// Called once the store is open and the shared lock is held.
    pub after_open: Option<&'a dyn Fn()>,
    /// Called after recovery released the exclusive lock, before the shared
    /// lock is taken again.
    pub after_recovery: Option<&'a dyn Fn()>,
    /// Called each time the shared lock was not available.
    pub on_poll: Option<&'a dyn Fn()>,
}

/// The lock file of a store: shared by every open, exclusive while a damaged
/// store is deleted and recreated.
pub fn store_lock_path(store: &Path) -> PathBuf {
    store.with_extension("lock")
}

/// A lock on [`store_lock_path`], shared unless recovery holds it exclusive.
/// Dropping it unlocks explicitly, so a copy of the descriptor that a
/// concurrent fork holds cannot keep it alive.
#[derive(Debug)]
pub struct StoreLock(File);

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl StoreLock {
    fn open(store: &Path) -> Result<StoreLock, DbError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(store_lock_path(store))
            .map_err(|e| {
                DbError::Failed(format!(
                    "failed to open the lock of the code index at {}: {e}",
                    store.display()
                ))
            })?;
        Ok(StoreLock(file))
    }

    /// Takes the shared lock, asking [`LOCK_POLL_TRIES`] times while a rebuild
    /// holds the exclusive one.
    fn share(&self, store: &Path, hooks: &DbHooks) -> Result<(), DbError> {
        for attempt in 0..LOCK_POLL_TRIES {
            match self.0.try_lock_shared() {
                Ok(()) => return Ok(()),
                Err(TryLockError::WouldBlock) => {
                    if let Some(poll) = hooks.on_poll {
                        poll();
                    }
                    if attempt + 1 < LOCK_POLL_TRIES {
                        std::thread::sleep(Duration::from_millis(LOCK_POLL_MS));
                    }
                }
                Err(TryLockError::Error(e)) => {
                    return Err(DbError::Failed(format!(
                        "failed to lock the code index at {}: {e}",
                        store.display()
                    )))
                }
            }
        }
        Err(DbError::Failed(
            "the index is being rebuilt; try again".to_string(),
        ))
    }

    /// Swaps the shared lock for the exclusive one without waiting. Returns
    /// false, with the shared lock held again, when a sibling holds a shared
    /// lock.
    fn try_exclusive(&self, store: &Path, hooks: &DbHooks) -> Result<bool, DbError> {
        let failed = |e: std::io::Error| {
            DbError::Failed(format!(
                "failed to lock the code index at {}: {e}",
                store.display()
            ))
        };
        self.0.unlock().map_err(failed)?;
        match self.0.try_lock() {
            Ok(()) => Ok(true),
            Err(TryLockError::WouldBlock) => self.share(store, hooks).map(|()| false),
            Err(TryLockError::Error(e)) => Err(failed(e)),
        }
    }
}

/// An open index store: the connection and the shared store lock it holds.
/// The connection is closed before the lock is released.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
    _lock: StoreLock,
    recovered: bool,
}

impl Store {
    /// Whether this open deleted a damaged store and created a new one.
    pub fn recovered(&self) -> bool {
        self.recovered
    }
}

impl Deref for Store {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.conn
    }
}

impl DerefMut for Store {
    fn deref_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }
}

/// Opens the index at `path` for `mode`. See [`OpenMode`].
pub fn open_index(path: &Path, mode: OpenMode) -> Result<Store, DbError> {
    open_index_with(path, mode, &delete_index_files, &DbHooks::default())
}

/// [`open_index`] with the file deleter and the test seams injected, so a
/// test can make the recreate step fail without relying on directory
/// permissions.
fn open_index_with(
    path: &Path,
    mode: OpenMode,
    delete: &dyn Fn(&Path) -> Result<(), String>,
    hooks: &DbHooks,
) -> Result<Store, DbError> {
    if mode == OpenMode::Query && !path.exists() {
        return Err(DbError::Missing);
    }
    ensure_parent(path, mode)?;
    let lock = StoreLock::open(path)?;
    lock.share(path, hooks)?;
    match try_open(path, mode, hooks) {
        Ok(conn) => {
            if let Some(opened) = hooks.after_open {
                opened();
            }
            Ok(Store {
                conn,
                _lock: lock,
                recovered: false,
            })
        }
        Err(DbError::NeedsRebuild(first)) if mode == OpenMode::Index => {
            recover(path, &first, lock, delete, hooks)
        }
        Err(e) => Err(e),
    }
}

/// Deletes and recreates a damaged store, holding the exclusive store lock
/// only for that step. A sibling that holds the shared lock stops it, and
/// nothing is deleted. Opening the store again under the lock covers a sibling
/// that repaired it first.
fn recover(
    path: &Path,
    first: &str,
    lock: StoreLock,
    delete: &dyn Fn(&Path) -> Result<(), String>,
    hooks: &DbHooks,
) -> Result<Store, DbError> {
    if !lock.try_exclusive(path, hooks)? {
        return Err(DbError::Failed(format!(
            "another worktree is using the code index at {}; try again when its command ends, \
             then run `{}`",
            path.display(),
            host::command("index --rebuild")
        )));
    }
    let recreated = recreate(path, first, delete, hooks);
    let _ = lock.0.unlock();
    if let Some(recovered) = hooks.after_recovery {
        recovered();
    }
    lock.share(path, hooks)?;
    recreated.map(|(conn, deleted)| Store {
        conn,
        _lock: lock,
        recovered: deleted,
    })
}

/// The recovery step under the exclusive lock. Returns the connection and
/// whether it deleted the store.
fn recreate(
    path: &Path,
    first: &str,
    delete: &dyn Fn(&Path) -> Result<(), String>,
    hooks: &DbHooks,
) -> Result<(Connection, bool), DbError> {
    match try_open(path, OpenMode::Index, hooks) {
        Ok(conn) => return Ok((conn, false)),
        Err(DbError::NeedsRebuild(_)) => {}
        Err(e) => return Err(e),
    }
    delete(path)
        .map_err(|e| DbError::Failed(format!("{e}; run `{}`", host::command("index --rebuild"))))?;
    try_open(path, OpenMode::Index, hooks)
        .map(|conn| (conn, true))
        .map_err(|second| {
            DbError::Failed(rebuild_hint(
                path,
                &format!("could not be recreated ({first}; then {second})"),
            ))
        })
}

/// Deletes the store of `path` and creates an empty one, for a `--rebuild`
/// that found the store damaged. It stops, and deletes nothing, while another
/// worktree uses the store.
pub fn rebuild_store(path: &Path) -> Result<Store, DbError> {
    ensure_parent(path, OpenMode::Index)?;
    let lock = StoreLock::open(path)?;
    let hooks = DbHooks::default();
    lock.share(path, &hooks)?;
    recover(
        path,
        "failed an integrity check",
        lock,
        &delete_index_files,
        &hooks,
    )
}

fn ensure_parent(path: &Path, mode: OpenMode) -> Result<(), DbError> {
    if mode != OpenMode::Index {
        return Ok(());
    }
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    host::ensure_private_dir(parent).map_err(|e| {
        DbError::Failed(format!(
            "failed to create directory {}: {e}",
            parent.display()
        ))
    })
}

fn is_corrupt(err: &rusqlite::Error) -> bool {
    matches!(
        err.sqlite_error_code(),
        Some(ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt)
    )
}

/// The text of a query or write error. A corrupt or foreign database (the
/// primary codes `SQLITE_CORRUPT` and `SQLITE_NOTADB`, `CORRUPT_VTAB` included)
/// names `--rebuild`; any other error keeps `prefix` and its own text.
pub fn map_sql_error(prefix: &str, err: &rusqlite::Error) -> String {
    if is_corrupt(err) {
        format!(
            "the code index is not a valid database; run `{}`",
            host::command("index --rebuild")
        )
    } else {
        format!("{prefix}: {err}")
    }
}

fn step(path: &Path, what: &str, err: &rusqlite::Error) -> DbError {
    if is_corrupt(err) {
        DbError::NeedsRebuild(rebuild_hint(path, "is not a valid index database"))
    } else {
        DbError::Failed(format!(
            "failed to {what} for the code index at {}: {err}",
            path.display()
        ))
    }
}

fn try_open(path: &Path, mode: OpenMode, hooks: &DbHooks) -> Result<Connection, DbError> {
    if mode == OpenMode::Index {
        host::make_file_private(path).map_err(DbError::Failed)?;
    }
    if mode == OpenMode::Query && std::fs::metadata(path).is_ok_and(|m| m.len() == 0) {
        return Err(DbError::Missing);
    }
    let mut conn = Connection::open(path).map_err(|e| step(path, "open the database", &e))?;
    conn.busy_timeout(BUSY_TIMEOUT)
        .map_err(|e| step(path, "set busy_timeout", &e))?;
    // Read the version before any pragma that writes the header, so a query on
    // an empty or foreign file leaves its bytes alone.
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|e| step(path, "read user_version", &e))?;
    if version != 0 && version != SCHEMA_VERSION {
        return Err(DbError::NeedsRebuild(rebuild_hint(
            path,
            &format!("has schema version {version}, this build uses {SCHEMA_VERSION}"),
        )));
    }
    if version == 0 && mode == OpenMode::Query {
        return Err(DbError::Missing);
    }
    if version == 0 {
        // Only a new file takes this; it must come before the WAL header.
        conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")
            .map_err(|e| step(path, "set auto_vacuum", &e))?;
    }
    retry_busy(
        || conn.pragma_update(None, "journal_mode", "WAL"),
        OPEN_BUDGET,
    )
    .map_err(|e| step(path, "set journal_mode=WAL", &e))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| step(path, "set foreign_keys", &e))?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| step(path, "set synchronous", &e))?;
    conn.pragma_update(None, "journal_size_limit", WAL_SIZE_LIMIT)
        .map_err(|e| step(path, "set journal_size_limit", &e))?;
    if !fts5_available(&conn) {
        return Err(DbError::Failed(
            "this build has no SQLite FTS5; rebuild with FTS5 enabled".to_string(),
        ));
    }
    if version == SCHEMA_VERSION {
        return Ok(conn);
    }
    if create_schema(&mut conn, path, hooks)? {
        let vacuum: i64 = conn
            .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
            .map_err(|e| step(path, "read auto_vacuum", &e))?;
        if vacuum != 2 {
            // The pragma had no effect on this file; the store is still empty,
            // so a VACUUM applies it cheaply.
            retry_busy(|| conn.execute_batch("VACUUM"), OPEN_BUDGET)
                .map_err(|e| step(path, "apply auto_vacuum", &e))?;
        }
    }
    Ok(conn)
}

/// Creates the schema in one write transaction. A second cold start that got
/// the write lock after the first finds the version set and writes nothing.
/// Returns whether this call created it.
fn create_schema(conn: &mut Connection, path: &Path, hooks: &DbHooks) -> Result<bool, DbError> {
    if let Some(before) = hooks.before_schema {
        before();
    }
    let tx =
        write_tx(conn, &TxHooks::default()).map_err(|e| step(path, "create the schema", &e))?;
    let version: i64 = tx
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|e| step(path, "read user_version", &e))?;
    if version == SCHEMA_VERSION {
        return Ok(false);
    }
    tx.execute_batch(SCHEMA_SQL)
        .map_err(|e| step(path, "create the schema", &e))?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|e| step(path, "set user_version", &e))?;
    tx.commit()
        .map_err(|e| step(path, "create the schema", &e))?;
    Ok(true)
}

/// Whether the store passes SQLite's quick check and the FTS5 integrity check.
pub fn is_healthy(conn: &Connection) -> bool {
    let quick: rusqlite::Result<String> =
        conn.query_row("PRAGMA quick_check(1)", [], |row| row.get(0));
    quick.is_ok_and(|result| result == "ok")
        && conn
            .execute(
                "INSERT INTO chunks_fts(chunks_fts) VALUES ('integrity-check')",
                [],
            )
            .is_ok()
}

/// Which connection the scoped views are for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    /// An index run: only `files` is a view. The run reads and writes the real
    /// `symbols`, `imports` and `chunks` tables.
    Writer,
    /// A query: `files`, `symbols`, `imports` and `chunks` are all views that
    /// show one worktree.
    Reader,
}

/// Creates the TEMP views that show one worktree under today's table names.
/// A view cannot bind a parameter, so the integer row id of the worktree is
/// written into the text. Call it after that row is final: `--rebuild` deletes
/// and inserts the row and gets a new id. Bodies name `main.` tables, because
/// an unqualified name inside a temp view resolves to the view itself.
///
/// `files` has the columns of the old table, minus `indexed_at`, plus
/// `content` and `parser`. Its `id` is the content id, and NULL for a path with
/// no content, which no `file_id` matches. A plain column keeps the view flat,
/// so `f.id = ?` is an index search on `manifest_content`; a `COALESCE` or a
/// `UNION ALL` view is built as a temporary table on every query and costs
/// more. In `symbols`, `imports`
/// and `chunks` the `file_id` column is the content id, restricted to the
/// content that `files` lists. `imports` also lists its `rowid`.
pub fn create_scope_views(
    conn: &Connection,
    worktree_row_id: i64,
    kind: ScopeKind,
) -> rusqlite::Result<()> {
    let wt = worktree_row_id;
    // The first write to the temp schema opens a temp file; keep it in memory.
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    let mut sql = format!(
        "CREATE TEMP VIEW IF NOT EXISTS files AS
         SELECT m.content AS id, m.path AS path, m.lang AS lang, m.sha256 AS sha256,
                m.size AS size, m.mtime_ns AS mtime_ns, m.status AS status, m.error AS error,
                m.content AS content, c.parser AS parser
         FROM main.manifest m LEFT JOIN main.content c ON c.id = m.content
         WHERE m.worktree = {wt};"
    );
    if kind == ScopeKind::Reader {
        // A correlated EXISTS on `manifest_content` costs one index probe per
        // row. An `IN (SELECT ...)` list is rebuilt on every statement.
        let listed = |table: &str| {
            format!(
                "EXISTS (SELECT 1 FROM main.manifest m2
                         WHERE m2.content = main.{table}.content AND m2.worktree = {wt})"
            )
        };
        sql.push_str(&format!(
            "CREATE TEMP VIEW IF NOT EXISTS symbols AS
             SELECT id, content AS file_id, parent_id, name, qualified_name, kind, signature,
                    signature_cut, exported, start_byte, end_byte, start_line, end_line
             FROM main.symbols WHERE {};
             CREATE TEMP VIEW IF NOT EXISTS imports AS
             SELECT rowid AS rowid, content AS file_id, module, names, line
             FROM main.imports WHERE {};
             CREATE TEMP VIEW IF NOT EXISTS chunks AS
             SELECT id, content AS file_id, start_line, end_line
             FROM main.chunks WHERE {};",
            listed("symbols"),
            listed("imports"),
            listed("chunks"),
        ));
    }
    conn.execute_batch(&sql)
}

/// Opens the index for a query: never creates or deletes anything, finds the
/// row of `worktree` and creates the scoped views. A store with no row for
/// this worktree reads as [`DbError::Missing`].
pub fn open_query(path: &Path, worktree: &str) -> Result<Store, DbError> {
    let store = open_index(path, OpenMode::Query)?;
    let row: Option<i64> = store
        .query_row("SELECT id FROM worktrees WHERE key = ?1", [worktree], |r| {
            r.get(0)
        })
        .optional()
        .map_err(|e| step(path, "read the worktree row", &e))?;
    let id = row.ok_or(DbError::Missing)?;
    create_scope_views(&store, id, ScopeKind::Reader)
        .map_err(|e| step(path, "create the scoped views", &e))?;
    Ok(store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use playbook_common::test_support::scratch_dir;

    fn count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .expect("count should run")
    }

    #[test]
    fn fts5_is_available_in_the_bundled_build() {
        // Arrange
        let conn = Connection::open_in_memory().expect("memory db");

        // Act
        let available = fts5_available(&conn);

        // Assert
        assert!(available, "bundled SQLite must have FTS5");
    }

    #[test]
    fn an_fts5_table_matches_the_row_it_holds() {
        // Arrange
        let conn = Connection::open_in_memory().expect("memory db");
        conn.execute_batch("CREATE VIRTUAL TABLE t USING fts5(body);")
            .expect("fts5 table");
        conn.execute("INSERT INTO t(rowid, body) VALUES (7, 'hello world')", [])
            .expect("insert");

        // Act
        let hit: i64 = conn
            .query_row("SELECT rowid FROM t WHERE t MATCH 'hello'", [], |r| {
                r.get(0)
            })
            .expect("match");

        // Assert
        assert_eq!(hit, 7);
    }

    #[cfg(unix)]
    #[test]
    fn open_creates_a_private_dir_and_file_at_the_current_schema_version() {
        use std::os::unix::fs::PermissionsExt;
        // Arrange
        let dir = scratch_dir("code-db-modes");
        let path = dir.join("code").join("index.sqlite3");

        // Act
        let conn = open_index(&path, OpenMode::Index).expect("open");

        // Assert
        let dir_mode = std::fs::metadata(path.parent().expect("parent"))
            .expect("dir meta")
            .permissions()
            .mode()
            & 0o777;
        let file_mode = std::fs::metadata(&path)
            .expect("file meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
        assert_eq!(file_mode, 0o600);
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("version");
        assert_eq!(version, SCHEMA_VERSION);
        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .expect("fk");
        assert_eq!(fk, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_version_mismatch_rebuilds_and_drops_stray_tables() {
        // Arrange
        let dir = scratch_dir("code-db-version");
        let path = dir.join("index.sqlite3");
        std::fs::create_dir_all(&dir).expect("dir");
        {
            let conn = Connection::open(&path).expect("raw db");
            conn.execute_batch("CREATE TABLE stray(x); PRAGMA user_version = 99;")
                .expect("seed");
        }

        // Act
        let conn = open_index(&path, OpenMode::Index).expect("open");

        // Assert
        let stray: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'stray'",
                [],
                |r| r.get(0),
            )
            .expect("master");
        assert_eq!(stray, 0);
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("version");
        assert_eq!(version, SCHEMA_VERSION);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn query_mode_on_a_missing_file_reports_missing_and_creates_nothing() {
        // Arrange
        let dir = scratch_dir("code-db-missing");
        let path = dir.join("nested").join("index.sqlite3");

        // Act
        let got = open_index(&path, OpenMode::Query);

        // Assert
        assert_eq!(got.err(), Some(DbError::Missing));
        assert!(!dir.exists(), "no directory may be created");
    }

    #[test]
    fn a_corrupt_file_rebuilds_in_index_mode_and_errors_in_query_mode() {
        // Arrange
        let dir = scratch_dir("code-db-corrupt");
        let path = dir.join("index.sqlite3");
        std::fs::create_dir_all(&dir).expect("dir");
        let garbage = vec![0x5Au8; 4096];
        std::fs::write(&path, &garbage).expect("garbage");

        // Act
        let query = open_index(&path, OpenMode::Query);

        // Assert: query mode names --rebuild and leaves the file alone.
        let message = query.expect_err("query must fail").to_string();
        assert!(message.contains("--rebuild"), "{message}");
        assert_eq!(std::fs::read(&path).expect("still there"), garbage);

        // Act
        let index = open_index(&path, OpenMode::Index).expect("index mode rebuilds");

        // Assert
        assert_eq!(count(&index, "manifest"), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_recreate_names_rebuild() {
        // Arrange: a corrupt db and a deleter that always fails.
        let dir = scratch_dir("code-db-undeletable");
        let path = dir.join("index.sqlite3");
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(&path, vec![0x5Au8; 4096]).expect("garbage");

        // Act
        let got = open_index_with(
            &path,
            OpenMode::Index,
            &|_| Err("denied".to_string()),
            &DbHooks::default(),
        );

        // Assert
        let message = got.expect_err("must fail").to_string();
        assert!(message.contains("denied"), "{message}");
        assert!(message.contains("--rebuild"), "{message}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn deleting_a_content_row_cascades_to_symbols_imports_and_chunks() {
        // Arrange
        let dir = scratch_dir("code-db-cascade");
        let path = dir.join("index.sqlite3");
        let conn = open_index(&path, OpenMode::Index).expect("open");
        conn.execute_batch(
            "INSERT INTO content(id, sha256, grammar, parser, partial) VALUES (1, 'h', 'rust', 'p', 0);
             INSERT INTO symbols(content, name, qualified_name, kind, start_byte, end_byte, start_line, end_line)
                 VALUES (1, 'f', 'f', 'function', 0, 1, 1, 1);
             INSERT INTO imports(content, module, names, line) VALUES (1, 'm', 'x', 1);
             INSERT INTO chunks(content, start_line, end_line) VALUES (1, 1, 1);",
        )
        .expect("seed");
        for table in ["symbols", "imports", "chunks"] {
            assert_eq!(count(&conn, table), 1, "{table} before");
        }

        // Act
        conn.execute("DELETE FROM content WHERE id = 1", [])
            .expect("delete");

        // Assert
        for table in ["symbols", "imports", "chunks"] {
            assert_eq!(count(&conn, table), 0, "{table} after");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_contentless_fts_row_is_deleted_by_rowid() {
        // Arrange
        let dir = scratch_dir("code-db-fts-delete");
        let conn = open_index(&dir.join("index.sqlite3"), OpenMode::Index).expect("open");
        conn.execute(
            "INSERT INTO chunks_fts(rowid, body) VALUES (5, 'create_payment')",
            [],
        )
        .expect("insert");
        let hit = |q: &str| -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM chunks_fts WHERE chunks_fts MATCH ?1",
                [q],
                |r| r.get(0),
            )
            .expect("match")
        };
        assert_eq!(hit("\"create_payment\""), 1);

        // Act
        conn.execute("DELETE FROM chunks_fts WHERE rowid = 5", [])
            .expect("delete");

        // Assert
        assert_eq!(hit("\"create_payment\""), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn open_in_query_mode_works_on_an_existing_index() {
        // Arrange
        let dir = scratch_dir("code-db-query");
        let path = dir.join("index.sqlite3");
        drop(open_index(&path, OpenMode::Index).expect("create"));

        // Act
        let got = open_index(&path, OpenMode::Query);

        // Assert
        assert!(got.is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn pragma(conn: &Connection, name: &str) -> i64 {
        conn.query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))
            .expect("pragma")
    }

    #[test]
    fn a_fresh_store_has_the_pragmas_and_strict_tables_the_blueprint_names() {
        // Arrange
        let dir = scratch_dir("code-db-pragmas");
        let conn = open_index(&dir.join("index.sqlite3"), OpenMode::Index).expect("open");

        // Act and assert, one row per pragma
        let version: String = conn
            .query_row("SELECT sqlite_version()", [], |r| r.get(0))
            .expect("version");
        let parts: Vec<u32> = version
            .split('.')
            .map(|p| p.parse().expect("num"))
            .collect();
        assert!((parts[0], parts[1]) >= (3, 43), "sqlite {version}");
        assert_eq!(pragma(&conn, "foreign_keys"), 1);
        assert_eq!(pragma(&conn, "auto_vacuum"), 2);
        assert_eq!(pragma(&conn, "journal_size_limit"), WAL_SIZE_LIMIT);
        for table in [
            "worktrees",
            "content",
            "manifest",
            "symbols",
            "imports",
            "chunks",
        ] {
            let strict: i64 = conn
                .query_row(
                    "SELECT strict FROM pragma_table_list WHERE name = ?1",
                    [table],
                    |r| r.get(0),
                )
                .expect("table_list");
            assert_eq!(strict, 1, "{table}");
        }
        let wrong_type = conn.execute(
            "INSERT INTO content(sha256, grammar, parser, partial) VALUES ('h', 'rust', 'p', 'x')",
            [],
        );
        assert!(wrong_type.is_err(), "text in an integer column must fail");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_deleted_content_id_and_a_deleted_chunk_id_are_never_reused() {
        // Arrange
        let dir = scratch_dir("code-db-autoincrement");
        let conn = open_index(&dir.join("index.sqlite3"), OpenMode::Index).expect("open");
        let insert = |sha: &str| {
            conn.execute(
                "INSERT INTO content(sha256, grammar, parser, partial) VALUES (?1, 'rust', 'p', 0)",
                [sha],
            )
            .expect("insert");
            conn.last_insert_rowid()
        };
        let top = insert("a");
        conn.execute("DELETE FROM content WHERE id = ?1", [top])
            .expect("delete");

        // Act
        let next = insert("b");

        // Assert
        assert!(next > top);
        conn.execute(
            "INSERT INTO content(id, sha256, grammar, parser, partial) VALUES (99, 'h', 'rust', 'p', 0)",
            [],
        )
        .expect("content");
        conn.execute(
            "INSERT INTO chunks(content, start_line, end_line) VALUES (99, 1, 1)",
            [],
        )
        .expect("chunk");
        let chunk = conn.last_insert_rowid();
        conn.execute("DELETE FROM chunks WHERE id = ?1", [chunk])
            .expect("delete chunk");
        conn.execute(
            "INSERT INTO chunks(content, start_line, end_line) VALUES (99, 1, 1)",
            [],
        )
        .expect("chunk again");
        assert!(conn.last_insert_rowid() > chunk);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_version_2_file_is_rebuilt_in_index_mode_and_kept_in_query_mode() {
        // Arrange
        let dir = scratch_dir("code-db-v2");
        let path = dir.join("index.sqlite3");
        std::fs::create_dir_all(&dir).expect("dir");
        {
            let conn = Connection::open(&path).expect("raw db");
            conn.execute_batch("CREATE TABLE files(x); PRAGMA user_version = 2;")
                .expect("seed");
        }

        // Act
        let query = open_index(&path, OpenMode::Query);

        // Assert
        let message = query.expect_err("query must not open v2").to_string();
        assert!(message.contains("--rebuild"), "{message}");
        assert!(path.exists(), "query mode must not delete the file");

        // Act
        let conn = open_index(&path, OpenMode::Index).expect("rebuild");

        // Assert
        assert_eq!(pragma(&conn, "user_version"), SCHEMA_VERSION);
        assert_eq!(count(&conn, "worktrees"), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_query_on_an_empty_file_or_version_0_file_is_missing_and_writes_nothing() {
        // Arrange
        let dir = scratch_dir("code-db-empty");
        std::fs::create_dir_all(&dir).expect("dir");
        let empty = dir.join("empty.sqlite3");
        std::fs::write(&empty, b"").expect("empty");
        let zero = dir.join("zero.sqlite3");
        {
            let conn = Connection::open(&zero).expect("raw db");
            conn.execute_batch("CREATE TABLE t(x);").expect("seed");
        }
        let before = std::fs::read(&zero).expect("bytes");

        // Act
        let empty_got = open_index(&empty, OpenMode::Query);
        let zero_got = open_index(&zero, OpenMode::Query);

        // Assert
        assert_eq!(empty_got.err(), Some(DbError::Missing));
        assert_eq!(zero_got.err(), Some(DbError::Missing));
        assert_eq!(std::fs::read(&empty).expect("empty again"), b"");
        assert_eq!(std::fs::read(&zero).expect("bytes again"), before);
        let created = open_index(&empty, OpenMode::Index).expect("index mode creates");
        assert_eq!(pragma(&created, "auto_vacuum"), 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_immediate_write_tx_makes_a_sibling_writer_wait_instead_of_failing_the_reader() {
        // Arrange
        let dir = scratch_dir("code-db-write-tx");
        let path = dir.join("index.sqlite3");
        let mut first = open_index(&path, OpenMode::Index).expect("open");
        let second = open_index(&path, OpenMode::Index).expect("second");
        second.busy_timeout(Duration::ZERO).expect("no wait");
        let hooks = TxHooks::default();

        // Act
        let tx = write_tx(&mut first, &hooks).expect("begin");
        let _: i64 = tx
            .query_row("SELECT COUNT(*) FROM worktrees", [], |r| r.get(0))
            .expect("read");
        let sibling = second.execute(
            "INSERT INTO worktrees(key, root, created_at, tool_version) VALUES ('k', 'r', 1, 'v')",
            [],
        );

        // Assert
        let err = sibling.expect_err("the sibling write must be refused");
        assert_eq!(err.sqlite_error_code(), Some(ErrorCode::DatabaseBusy));
        commit_tx(tx, &hooks).expect("the first transaction commits");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_tx_hooks_run_and_can_fail_the_begin_and_the_commit() {
        // Arrange
        let dir = scratch_dir("code-db-tx-hooks");
        let mut conn = open_index(&dir.join("index.sqlite3"), OpenMode::Index).expect("open");
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = seen.clone();
        let busy = || {
            Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                None,
            ))
        };
        let observe = TxHooks {
            on_write_tx: Some(Box::new(move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })),
            ..TxHooks::default()
        };
        let begin = TxHooks {
            fail_begin: Some(Box::new(busy)),
            ..TxHooks::default()
        };
        let commit = TxHooks {
            fail_commit: Some(Box::new(busy)),
            ..TxHooks::default()
        };

        // Act and assert
        drop(write_tx(&mut conn, &observe).expect("begin"));
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(write_tx(&mut conn, &begin).is_err());
        let tx = write_tx(&mut conn, &commit).expect("begin");
        tx.execute(
            "INSERT INTO worktrees(key, root, created_at, tool_version) VALUES ('k', 'r', 1, 'v')",
            [],
        )
        .expect("insert");
        assert!(commit_tx(tx, &commit).is_err());
        assert_eq!(count(&conn, "worktrees"), 0, "a failed commit rolls back");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_content_row_that_a_manifest_row_uses_cannot_be_deleted() {
        // Arrange
        let dir = scratch_dir("code-db-restrict");
        let conn = open_index(&dir.join("index.sqlite3"), OpenMode::Index).expect("open");
        conn.execute_batch(
            "INSERT INTO worktrees(id, key, root, created_at, tool_version) VALUES (1, 'k', 'r', 1, 'v');
             INSERT INTO content(id, sha256, grammar, parser, partial) VALUES (1, 'h', 'rust', 'p', 0);
             INSERT INTO manifest(worktree, path, content, sha256, size, mtime_ns, status)
                 VALUES (1, 'a.rs', 1, 'h', 1, 1, 'ok');",
        )
        .expect("seed");

        // Act
        let blocked = conn.execute("DELETE FROM content WHERE id = 1", []);
        conn.execute("DELETE FROM worktrees WHERE id = 1", [])
            .expect("the cascade removes the manifest");

        // Assert
        assert!(blocked.is_err(), "RESTRICT must refuse the delete");
        assert_eq!(count(&conn, "manifest"), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_repoint_check_uses_the_manifest_content_index() {
        // Arrange
        let dir = scratch_dir("code-db-plan");
        let conn = open_index(&dir.join("index.sqlite3"), OpenMode::Index).expect("open");

        // Act
        let plan: Vec<String> = conn
            .prepare("EXPLAIN QUERY PLAN SELECT 1 FROM manifest WHERE content = ?1")
            .expect("prepare")
            .query_map([1], |r| r.get::<_, String>(3))
            .expect("plan")
            .map(|r| r.expect("row"))
            .collect();

        // Assert
        let text = plan.join(" | ");
        assert!(
            text.contains("SEARCH") && text.contains("manifest_content"),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    fn damage(path: &Path) {
        std::fs::write(path, vec![0x5Au8; 4096]).expect("garbage");
    }

    fn try_lock_exclusive(store: &Path) -> bool {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(store_lock_path(store))
            .expect("lock file");
        let got = file.try_lock().is_ok();
        let _ = file.unlock();
        got
    }

    fn lock_file(store: &Path) -> File {
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(store_lock_path(store))
            .expect("lock file")
    }

    #[test]
    fn a_build_opens_only_its_own_store_version() {
        // Arrange
        let dir = scratch_dir("code-db-versions");
        std::fs::create_dir_all(&dir).expect("dir");
        let v3 = dir.join("store-v3.sqlite3");
        let v4 = dir.join("store-v4.sqlite3");
        {
            let conn = Connection::open(&v3).expect("v3");
            conn.execute_batch("CREATE TABLE marker(x); INSERT INTO marker VALUES (1);")
                .expect("seed");
        }
        let v3_bytes = std::fs::read(&v3).expect("v3 bytes");
        let held = lock_file(&v3);
        held.lock_shared().expect("v3 lock");
        damage(&v4);

        // Act
        let query = open_index(&v4, OpenMode::Query);

        // Assert
        assert!(matches!(query, Err(DbError::NeedsRebuild(_))));
        assert!(v4.exists());

        // Act
        let index = open_index(&v4, OpenMode::Index).expect("v4 recovers");

        // Assert
        assert_eq!(count(&index, "worktrees"), 0);
        assert_eq!(std::fs::read(&v3).expect("v3 after"), v3_bytes);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn two_cold_starts_create_the_store_once() {
        // Arrange
        let dir = scratch_dir("code-db-cold");
        let path = dir.join("code").join("store-v4.sqlite3");
        let inner_ok = std::cell::Cell::new(false);
        let nested = || {
            inner_ok.set(open_index(&path, OpenMode::Index).is_ok());
        };
        let hooks = DbHooks {
            before_schema: Some(&nested),
            ..DbHooks::default()
        };

        // Act
        let outer = open_index_with(&path, OpenMode::Index, &delete_index_files, &hooks);

        // Assert
        let outer = outer.expect("outer open");
        assert!(inner_ok.get(), "the nested open completes");
        assert_eq!(pragma(&outer, "user_version"), SCHEMA_VERSION);
        assert!(!outer.recovered());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_open_store_holds_the_shared_lock_in_both_modes() {
        // Arrange
        let dir = scratch_dir("code-db-shared");
        let path = dir.join("store-v4.sqlite3");
        drop(open_index(&path, OpenMode::Index).expect("create"));
        let exclusive_while_open = std::cell::Cell::new(true);
        let probe = || exclusive_while_open.set(try_lock_exclusive(&path));

        for mode in [OpenMode::Index, OpenMode::Query] {
            exclusive_while_open.set(true);
            let hooks = DbHooks {
                after_open: Some(&probe),
                ..DbHooks::default()
            };

            // Act
            let store = open_index_with(&path, mode, &delete_index_files, &hooks);

            // Assert
            assert!(store.is_ok() || mode == OpenMode::Query);
            assert!(!exclusive_while_open.get(), "{mode:?}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_query_waits_briefly_for_an_exclusive_lock() {
        // Arrange
        let dir = scratch_dir("code-db-poll");
        let path = dir.join("store-v4.sqlite3");
        drop(open_index(&path, OpenMode::Index).expect("create"));
        let holder = lock_file(&path);
        holder.lock().expect("exclusive");
        let polls = std::cell::Cell::new(0u32);
        let count_poll = || polls.set(polls.get() + 1);
        let hooks = DbHooks {
            on_poll: Some(&count_poll),
            ..DbHooks::default()
        };

        // Act
        let got = open_index_with(&path, OpenMode::Query, &delete_index_files, &hooks);

        // Assert
        let message = got.expect_err("must give up").to_string();
        assert_eq!(message, "the index is being rebuilt; try again");
        assert_eq!(polls.get(), LOCK_POLL_TRIES);
        let _ = holder.unlock();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn recovery_recreates_the_store_and_leaves_the_shared_lock() {
        // Arrange
        let dir = scratch_dir("code-db-recover");
        let path = dir.join("store-v4.sqlite3");
        std::fs::create_dir_all(&dir).expect("dir");
        damage(&path);
        let third_is_healthy = std::cell::Cell::new(false);
        let third = || {
            let again = open_index(&path, OpenMode::Index);
            third_is_healthy.set(again.is_ok_and(|s| !s.recovered()));
        };
        let hooks = DbHooks {
            after_recovery: Some(&third),
            ..DbHooks::default()
        };

        // Act
        let store =
            open_index_with(&path, OpenMode::Index, &delete_index_files, &hooks).expect("recovers");

        // Assert
        assert!(store.recovered());
        assert_eq!(pragma(&store, "auto_vacuum"), 2);
        assert!(!try_lock_exclusive(&path), "the shared lock is held");
        assert!(third_is_healthy.get(), "a third open finds a healthy store");
        drop(store);
        let before = std::fs::read(&path).expect("bytes");
        #[cfg(unix)]
        let inode = std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&path).expect("meta"));

        // Act
        let second = open_index(&path, OpenMode::Index).expect("second open");

        // Assert
        assert!(!second.recovered());
        drop(second);
        assert_eq!(std::fs::read(&path).expect("after"), before);
        #[cfg(unix)]
        assert_eq!(
            std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&path).expect("meta")),
            inode
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_damaged_store_a_sibling_uses_is_left_alone() {
        // Arrange
        let dir = scratch_dir("code-db-sibling");
        let path = dir.join("store-v4.sqlite3");
        std::fs::create_dir_all(&dir).expect("dir");
        damage(&path);
        let before = std::fs::read(&path).expect("bytes");
        let sibling = lock_file(&path);
        sibling.lock_shared().expect("shared");

        // Act
        let got = open_index(&path, OpenMode::Index);

        // Assert
        let message = got.expect_err("must refuse").to_string();
        assert!(
            message.contains("another worktree is using the code index"),
            "{message}"
        );
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(message.contains("--rebuild"), "{message}");
        assert_eq!(std::fs::read(&path).expect("after"), before);

        // Act
        sibling.unlock().expect("release");
        let rebuilt = open_index(&path, OpenMode::Index).expect("rebuilds");

        // Assert
        assert!(rebuilt.recovered());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_wal_and_shm_files_go_before_the_main_file() {
        // Arrange
        let path = Path::new("/d/store-v4.sqlite3");

        // Act
        let order: Vec<String> = sibling_files(path)
            .iter()
            .map(|p| p.display().to_string())
            .collect();

        // Assert
        assert_eq!(
            order,
            [
                "/d/store-v4.sqlite3-wal",
                "/d/store-v4.sqlite3-shm",
                "/d/store-v4.sqlite3"
            ]
        );
    }

    #[test]
    fn a_failed_delete_stops_the_open_without_a_retry() {
        // Arrange
        let dir = scratch_dir("code-db-nodelete");
        let path = dir.join("store-v4.sqlite3");
        std::fs::create_dir_all(&dir).expect("dir");
        damage(&path);
        let calls = std::cell::Cell::new(0u32);
        let refuse = |_: &Path| {
            calls.set(calls.get() + 1);
            Err("the file is open".to_string())
        };

        // Act
        let got = open_index_with(&path, OpenMode::Index, &refuse, &DbHooks::default());

        // Assert
        let message = got.expect_err("must fail").to_string();
        assert!(message.contains("the file is open"), "{message}");
        assert_eq!(calls.get(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_fresh_store_passes_the_health_check() {
        // Arrange
        let dir = scratch_dir("code-db-health");
        let path = dir.join("store-v4.sqlite3");
        let store = open_index(&path, OpenMode::Index).expect("open");

        // Act
        let healthy = is_healthy(&store);

        // Assert
        assert!(healthy);
        let _ = std::fs::remove_dir_all(dir);
    }
}
