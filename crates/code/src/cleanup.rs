// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Garbage collection of the shared store at the end of an index run. Content
//! that no manifest lists, worktrees whose checkout is gone, and store files of
//! older schema versions go in short transactions, so a sibling that waits for
//! the write lock waits for one chunk at most.

use super::db::{self, TxHooks};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Content rows, or manifest rows of a dead worktree, per transaction.
pub const CLEANUP_CHUNK: usize = 50;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// A worktree row whose root is gone is dropped after this long.
pub const DEAD_ROOT_GRACE_MS: i64 = 7 * DAY_MS;
/// A store file of an older schema version is deleted after this long.
pub const STALE_STORE_GRACE_MS: i64 = 30 * DAY_MS;
/// Pages the incremental vacuum returns to the file after a cleanup.
const VACUUM_PAGES: i64 = 1000;

/// Whether the cleanup of a run finished.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Cleanup {
    #[default]
    Done,
    /// A chunk failed, for example on a busy store. The run still succeeded
    /// and the next run tries again.
    Skipped,
}

impl Cleanup {
    pub fn as_str(self) -> &'static str {
        match self {
            Cleanup::Done => "done",
            Cleanup::Skipped => "skipped",
        }
    }
}

/// An injected file deleter.
pub type FileDeleter = Box<dyn Fn(&Path) -> std::io::Result<()> + Send + Sync>;

/// Test seams. Production passes [`Hooks::default`], which holds none.
#[derive(Default)]
pub struct Hooks {
    /// Makes each chunk fail with a busy error.
    pub fail_cleanup: bool,
    /// Called with the rows a non-empty chunk deleted.
    pub after_cleanup_chunk: Option<Box<dyn Fn(usize) + Send + Sync>>,
    /// Called once after the delete loop and before the incremental vacuum.
    pub before_vacuum: Option<Box<dyn Fn() + Send + Sync>>,
    /// Replaces `std::fs::remove_file` for stale store and lock files.
    pub delete_file: Option<FileDeleter>,
}

/// What a cleanup reads besides the store: the clock, the transaction hooks
/// and the test seams.
pub struct Env<'a> {
    pub now_ms: i64,
    pub tx: &'a TxHooks,
    pub hooks: &'a Hooks,
}

impl Env<'_> {
    fn delete(&self, path: &Path) -> std::io::Result<()> {
        match &self.hooks.delete_file {
            Some(delete) => delete(path),
            None => std::fs::remove_file(path),
        }
    }

    fn chunk_done(&self, rows: usize) {
        if let (Some(hook), true) = (&self.hooks.after_cleanup_chunk, rows > 0) {
            hook(rows);
        }
    }

    fn check(&self) -> rusqlite::Result<()> {
        if self.hooks.fail_cleanup {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                None,
            ));
        }
        Ok(())
    }
}

/// Cleans the store at `store`. Returns [`Cleanup::Skipped`] when a chunk
/// failed, after which the rest waits for the next run.
pub fn collect_garbage(conn: &mut Connection, store: &Path, env: &Env) -> Cleanup {
    match clean(conn, store, env) {
        Ok(()) => Cleanup::Done,
        Err(_) => Cleanup::Skipped,
    }
}

/// The FTS5 `automerge` value that stops merging when a transaction flushes.
const AUTOMERGE_OFF: i64 = 0;
/// The FTS5 default, restored after a cleanup.
const AUTOMERGE_DEFAULT: i64 = 4;

fn set_automerge(conn: &mut Connection, env: &Env, value: i64) -> rusqlite::Result<()> {
    let tx = db::write_tx(conn, env.tx)?;
    tx.execute(
        "INSERT INTO chunks_fts(chunks_fts, rank) VALUES ('automerge', ?1)",
        [value],
    )?;
    db::commit_tx(tx, env.tx)
}

/// Whether an earlier cleanup stopped before it restored the merge setting.
fn automerge_is_off(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT v FROM chunks_fts_config WHERE k = 'automerge'",
        [],
        |r| r.get::<_, i64>(0),
    )
    .is_ok_and(|v| v == AUTOMERGE_OFF)
}

/// Runs the delete loops with the FTS5 merge deferred. Each delete adds a
/// small segment, and with `automerge` on, a flush now and then merges many of
/// them in one transaction, which held the write lock for hundreds of
/// milliseconds on a 52 worktree store. The setting is restored afterwards, so
/// the next index run merges the segments in its own small steps.
fn clean(conn: &mut Connection, store: &Path, env: &Env) -> rusqlite::Result<()> {
    let dead = dead_worktrees(conn, env.now_ms)?;
    let work = !dead.is_empty() || any_unreferenced_content(conn)?;
    let left_off = automerge_is_off(conn);
    if work {
        set_automerge(conn, env, AUTOMERGE_OFF)?;
    }
    let result = delete_loops(conn, store, env, dead);
    if work || left_off {
        set_automerge(conn, env, AUTOMERGE_DEFAULT)?;
    }
    result?;
    if let Some(before) = &env.hooks.before_vacuum {
        before();
    }
    incremental_vacuum(conn)?;
    conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE)")?;
    delete_stale_stores(store, env);
    Ok(())
}

fn delete_loops(
    conn: &mut Connection,
    store: &Path,
    env: &Env,
    dead: Vec<(i64, String)>,
) -> rusqlite::Result<()> {
    for (id, key) in dead {
        // A held run lock means that worktree is indexing right now, so its
        // row stays whatever its root says.
        let lock = store.parent().map(|dir| dir.join(format!("{key}.lock")));
        let held = match &lock {
            Some(path) => match exclusive_lock(path) {
                Some(guard) => Some(guard),
                None => continue,
            },
            None => None,
        };
        drop_worktree(conn, id, env)?;
        drop(held);
        // A lock file that cannot be deleted stays for the next run to retry
        // and does not stop the cleanup.
        if let Some(path) = &lock {
            let _ = env.delete(path);
        }
    }
    drop_unreferenced_content(conn, env)
}

/// Returns up to [`VACUUM_PAGES`] free pages to the file. The pragma frees one
/// page per step, so every row it returns is stepped through.
fn incremental_vacuum(conn: &Connection) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA incremental_vacuum({VACUUM_PAGES})"))?;
    let mut rows = stmt.query([])?;
    while rows.next()?.is_some() {}
    Ok(())
}

/// Worktree rows whose root is gone and whose last activity is older than
/// the grace. A root that cannot be checked keeps its row.
fn dead_worktrees(conn: &Connection, now_ms: i64) -> rusqlite::Result<Vec<(i64, String)>> {
    let mut stmt = conn.prepare(
        "SELECT id, key, root,
                MAX(COALESCE(last_run_completed, 0), COALESCE(last_run_started, 0), created_at)
         FROM worktrees",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;
    let mut dead = Vec::new();
    for row in rows {
        let (id, key, root, last_seen) = row?;
        let old = now_ms.saturating_sub(last_seen) > DEAD_ROOT_GRACE_MS;
        if old && matches!(Path::new(&root).try_exists(), Ok(false)) {
            dead.push((id, key));
        }
    }
    Ok(dead)
}

/// Deletes a worktree's manifest rows in chunks, then its row.
fn drop_worktree(conn: &mut Connection, id: i64, env: &Env) -> rusqlite::Result<()> {
    loop {
        env.check()?;
        let tx = db::write_tx(conn, env.tx)?;
        let deleted = tx.execute(
            "DELETE FROM manifest WHERE rowid IN
             (SELECT rowid FROM manifest WHERE worktree = ?1 LIMIT ?2)",
            params![id, CLEANUP_CHUNK as i64],
        )?;
        db::commit_tx(tx, env.tx)?;
        env.chunk_done(deleted);
        if deleted < CLEANUP_CHUNK {
            break;
        }
    }
    let tx = db::write_tx(conn, env.tx)?;
    tx.execute("DELETE FROM worktrees WHERE id = ?1", [id])?;
    db::commit_tx(tx, env.tx)
}

fn any_unreferenced_content(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM content c
                        WHERE NOT EXISTS (SELECT 1 FROM manifest m WHERE m.content = c.id))",
        [],
        |r| r.get(0),
    )
}

/// Deletes content no manifest row lists, with its FTS rows first. The ids of
/// a chunk are selected once into a temporary table, so the FTS delete and
/// the content delete act on the same rows.
fn drop_unreferenced_content(conn: &mut Connection, env: &Env) -> rusqlite::Result<()> {
    while any_unreferenced_content(conn)? {
        env.check()?;
        let tx = db::write_tx(conn, env.tx)?;
        tx.execute_batch("DROP TABLE IF EXISTS temp.doomed; CREATE TEMP TABLE doomed(id INTEGER)")?;
        tx.execute(
            "INSERT INTO temp.doomed
             SELECT c.id FROM content c
             WHERE NOT EXISTS (SELECT 1 FROM manifest m WHERE m.content = c.id) LIMIT ?1",
            [CLEANUP_CHUNK as i64],
        )?;
        tx.execute(
            "DELETE FROM chunks_fts WHERE rowid IN
             (SELECT id FROM chunks WHERE content IN (SELECT id FROM temp.doomed))",
            [],
        )?;
        let deleted = tx.execute(
            "DELETE FROM content WHERE id IN (SELECT id FROM temp.doomed)",
            [],
        )?;
        tx.execute_batch("DROP TABLE temp.doomed")?;
        db::commit_tx(tx, env.tx)?;
        env.chunk_done(deleted);
        if deleted < CLEANUP_CHUNK {
            break;
        }
    }
    Ok(())
}

/// Deletes the files of `store-v<M>` for every `M` below the current version
/// that nothing modified for the grace and no process uses. Every failure
/// leaves that store for the next run.
fn delete_stale_stores(store: &Path, env: &Env) {
    let (Some(dir), Some(current)) = (store.parent(), version_of(store)) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(version) = version_of(&path) else {
            continue;
        };
        if version >= current || !is_stale(&path, env.now_ms) {
            continue;
        }
        let lock = path.with_extension("lock");
        let Some(_held) = exclusive_lock(&lock) else {
            continue;
        };
        for suffix in ["-wal", "-shm", ""] {
            let mut name = path.as_os_str().to_owned();
            name.push(suffix);
            let _ = env.delete(&PathBuf::from(name));
        }
        let _ = env.delete(&lock);
    }
}

/// The `M` of a `store-v<M>.sqlite3` path, compared as a number.
fn version_of(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_str()?;
    name.strip_prefix("store-v")?
        .strip_suffix(".sqlite3")?
        .parse()
        .ok()
}

/// Whether the store file and its sidecars all predate the grace.
fn is_stale(path: &Path, now_ms: i64) -> bool {
    ["", "-wal", "-shm"].iter().all(|suffix| {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        let Ok(meta) = std::fs::metadata(PathBuf::from(name)) else {
            return true;
        };
        let Ok(age) = meta.modified().map(|m| m.duration_since(UNIX_EPOCH)) else {
            return false;
        };
        let modified_ms = age.map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        now_ms.saturating_sub(modified_ms) > STALE_STORE_GRACE_MS
    })
}

/// The exclusive lock on an old store's lock file, or `None` while a process
/// holds it. The guard unlocks on drop.
fn exclusive_lock(lock: &Path) -> Option<ExclusiveLock> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock)
        .ok()?;
    file.try_lock().ok()?;
    Some(ExclusiveLock(file))
}

struct ExclusiveLock(std::fs::File);

impl Drop for ExclusiveLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::OpenMode;
    use crate::index::{self, Options};
    use crate::testkit::{fx, pinned, Fx};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    const NOW: i64 = 1_800_000_000_000;

    fn raw(f: &Fx) -> Connection {
        let conn = Connection::open(&f.index).expect("raw");
        conn.pragma_update(None, "foreign_keys", "ON").expect("fk");
        conn
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).expect("count")
    }

    /// Runs one cleanup against the fixture's store.
    fn clean_with(f: &Fx, hooks: &Hooks, tx: &TxHooks) -> Cleanup {
        let mut store = db::open_index(&f.index, OpenMode::Index).expect("open");
        let env = Env {
            now_ms: NOW,
            tx,
            hooks,
        };
        collect_garbage(&mut store, &f.index, &env)
    }

    fn clean(f: &Fx) -> Cleanup {
        clean_with(f, &Hooks::default(), &TxHooks::default())
    }

    fn add_unreferenced_content(f: &Fx, n: usize) {
        let conn = raw(f);
        for i in 0..n {
            conn.execute(
                "INSERT INTO content(sha256, grammar, parser, partial) VALUES (?1, 'text', 'p', 0)",
                [format!("{i:0>64}")],
            )
            .expect("content");
        }
    }

    fn chunk_log() -> (Arc<Mutex<Vec<usize>>>, Hooks) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let hooks = Hooks {
            after_cleanup_chunk: Some(Box::new(move |n| log.lock().expect("log").push(n))),
            ..Hooks::default()
        };
        (seen, hooks)
    }

    #[test]
    fn content_only_a_dropped_worktree_listed_goes_and_shared_content_stays() {
        let _g = pinned();
        // Arrange
        let f = fx("gc-forgotten");
        f.write("shared.rs", "fn shared() {}\n");
        f.write("only_a.rs", "fn only_a() {}\n");
        f.run();
        let other = crate::walker::test_repo::init("gc-forgotten-b");
        crate::walker::test_repo::write(&other, "shared.rs", b"fn shared() {}\n");
        index::run(&other, &f.index, "other", false, &Options::default()).expect("b");
        raw(&f)
            .execute("DELETE FROM worktrees WHERE key = ?1", [&f.worktree])
            .expect("forget a");

        // Act
        let report =
            index::run(&other, &f.index, "other", false, &Options::default()).expect("b again");

        // Assert
        assert_eq!(report.cleanup, Cleanup::Done);
        let conn = raw(&f);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM content"), 1);
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM symbols WHERE name = 'only_a'"),
            0
        );
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM chunks"), 1);
        let hits = |term: &str| {
            count(
                &conn,
                &format!("SELECT COUNT(*) FROM chunks_fts WHERE chunks_fts MATCH '{term}'"),
            )
        };
        assert_eq!(hits("only_a"), 0);
        assert_eq!(hits("shared"), 1);
    }

    #[test]
    fn unreferenced_content_goes_in_chunks_of_at_most_the_limit() {
        let _g = pinned();
        for (rows, calls) in [(0usize, 0usize), (50, 1), (51, 2), (450, 9)] {
            // Arrange
            let f = fx("gc-chunks");
            f.write("a.rs", "fn a() {}\n");
            f.run();
            let keep = count(&raw(&f), "SELECT COUNT(*) FROM content");
            add_unreferenced_content(&f, rows);
            let (seen, hooks) = chunk_log();

            // Act
            let outcome = clean_with(&f, &hooks, &TxHooks::default());

            // Assert
            let seen = seen.lock().expect("seen");
            assert_eq!(outcome, Cleanup::Done);
            assert_eq!(seen.len(), calls, "{rows} rows");
            assert_eq!(seen.iter().sum::<usize>(), rows);
            assert!(seen.iter().all(|n| *n <= CLEANUP_CHUNK));
            assert_eq!(count(&raw(&f), "SELECT COUNT(*) FROM content"), keep);
        }
    }

    #[test]
    fn a_store_with_nothing_to_clean_opens_no_write_transaction() {
        let _g = pinned();
        // Arrange
        let f = fx("gc-nothing");
        f.write("a.rs", "fn a() {}\n");
        f.run();
        let begins = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&begins);
        let tx = TxHooks {
            on_write_tx: Some(Box::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            })),
            ..TxHooks::default()
        };

        // Act
        let outcome = clean_with(&f, &Hooks::default(), &tx);

        // Assert
        assert_eq!(outcome, Cleanup::Done);
        assert_eq!(begins.load(Ordering::SeqCst), 0);
    }

    fn add_worktree_row(f: &Fx, key: &str, root: &str, created: i64, completed: Option<i64>) {
        raw(f)
            .execute(
                "INSERT INTO worktrees(key, root, created_at, tool_version, last_run_completed)
                 VALUES (?1, ?2, ?3, 'x', ?4)",
                rusqlite::params![key, root, created, completed],
            )
            .expect("worktree");
    }

    fn has_worktree(f: &Fx, key: &str) -> bool {
        count(
            &raw(f),
            &format!("SELECT COUNT(*) FROM worktrees WHERE key = '{key}'"),
        ) == 1
    }

    #[test]
    fn a_dead_worktree_loses_its_rows_in_chunks_and_its_lock_file() {
        let _g = pinned();
        // Arrange
        let f = fx("gc-dead");
        f.write("a.rs", "fn a() {}\n");
        f.run();
        add_worktree_row(&f, "dead", "/no/such/root", 0, Some(1));
        {
            let conn = raw(&f);
            let id: i64 = conn
                .query_row("SELECT id FROM worktrees WHERE key = 'dead'", [], |r| {
                    r.get(0)
                })
                .expect("id");
            for i in 0..120 {
                conn.execute(
                    "INSERT INTO manifest(worktree, path, sha256, size, mtime_ns, status)
                     VALUES (?1, ?2, '', 0, 0, 'skipped')",
                    rusqlite::params![id, format!("p{i}")],
                )
                .expect("manifest");
            }
        }
        let lock = f.index.with_file_name("dead.lock");
        std::fs::write(&lock, b"").expect("lock");
        let (seen, hooks) = chunk_log();

        // Act
        let outcome = clean_with(&f, &hooks, &TxHooks::default());

        // Assert
        assert_eq!(outcome, Cleanup::Done);
        assert_eq!(*seen.lock().expect("seen"), vec![50, 50, 20]);
        assert!(!has_worktree(&f, "dead"));
        assert!(!lock.exists());
        assert!(has_worktree(&f, &f.worktree));
    }

    #[test]
    fn a_worktree_row_is_dropped_only_when_its_root_is_gone_and_it_is_old() {
        let _g = pinned();
        let grace = DEAD_ROOT_GRACE_MS;
        // (name, root exists, age of created_at, age of last_run_completed, dropped)
        let cases: [(&str, bool, i64, Option<i64>, bool); 7] = [
            ("old-gone", false, 100 * grace, Some(grace + 1), true),
            ("exactly-grace", false, 100 * grace, Some(grace), false),
            ("just-inside", false, 100 * grace, Some(grace - 1), false),
            ("null-completed-old-created", false, grace + 1, None, true),
            ("null-completed-new-created", false, grace - 1, None, false),
            (
                "old-created-recent-completed",
                false,
                100 * grace,
                Some(1),
                false,
            ),
            ("root-exists", true, 100 * grace, Some(100 * grace), false),
        ];
        for (name, exists, created, completed, dropped) in cases {
            // Arrange
            let f = fx("gc-dead-table");
            f.write("a.rs", "fn a() {}\n");
            f.run();
            let root = if exists {
                f.root.display().to_string()
            } else {
                "/no/such/root".to_string()
            };
            add_worktree_row(&f, name, &root, NOW - created, completed.map(|c| NOW - c));

            // Act
            clean(&f);

            // Assert
            assert_eq!(has_worktree(&f, name), !dropped, "{name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_root_that_cannot_be_checked_keeps_its_row() {
        let _g = pinned();
        // Arrange
        let f = fx("gc-dead-error");
        f.write("a.rs", "fn a() {}\n");
        f.run();
        let under_a_file = format!("{}/child", f.root.join("a.rs").display());
        add_worktree_row(&f, "unknown", &under_a_file, 0, Some(1));

        // Act
        clean(&f);

        // Assert
        assert!(has_worktree(&f, "unknown"));
    }

    fn set_age(file: &Path, age_ms: i64) {
        let at = UNIX_EPOCH + Duration::from_millis(u64::try_from(NOW - age_ms).expect("time"));
        std::fs::File::options()
            .write(true)
            .open(file)
            .expect("open")
            .set_modified(at)
            .expect("mtime");
    }

    /// The four files of an old store, each `age_ms` old.
    fn old_store(f: &Fx, version: u32, age_ms: i64) -> Vec<PathBuf> {
        let dir = f.index.parent().expect("dir");
        let main = dir.join(format!("store-v{version}.sqlite3"));
        let files = vec![
            main.clone(),
            PathBuf::from(format!("{}-wal", main.display())),
            PathBuf::from(format!("{}-shm", main.display())),
            dir.join(format!("store-v{version}.lock")),
        ];
        for file in &files {
            std::fs::write(file, b"x").expect("write");
            set_age(file, age_ms);
        }
        files
    }

    #[test]
    fn stores_of_older_versions_go_after_the_grace_and_newer_ones_stay() {
        let _g = pinned();
        let grace = STALE_STORE_GRACE_MS;
        // (version, age, deleted)
        let cases = [
            (3u32, grace + 1000, true),
            (2, grace + 1000, true),
            (3, grace - 1000, false),
            (5, grace + 1000, false),
            (10, grace + 1000, false),
            (9, grace + 1000, false),
        ];
        for (version, age, deleted) in cases {
            // Arrange
            let f = fx("gc-stale-stores");
            f.write("a.rs", "fn a() {}\n");
            f.run();
            let files = old_store(&f, version, age);

            // Act
            clean(&f);

            // Assert
            for file in &files {
                assert_eq!(!file.exists(), deleted, "v{version} age {age}: {file:?}");
            }
        }
    }

    #[test]
    fn a_recent_sidecar_or_a_held_lock_keeps_an_old_store() {
        let _g = pinned();
        // Arrange
        let f = fx("gc-stale-held");
        f.write("a.rs", "fn a() {}\n");
        f.run();
        let grace = STALE_STORE_GRACE_MS;
        let files = old_store(&f, 3, grace + 1000);
        set_age(&files[1], 24 * 60 * 60 * 1000);

        // Act
        clean(&f);

        // Assert
        assert!(files.iter().all(|p| p.exists()), "a recent -wal keeps it");

        // Arrange
        set_age(&files[1], grace + 1000);
        let held = std::fs::File::options()
            .write(true)
            .open(&files[3])
            .expect("lock");
        held.lock_shared().expect("shared");

        // Act
        clean(&f);

        // Assert
        assert!(files.iter().all(|p| p.exists()), "a held lock keeps it");
        held.unlock().expect("release");

        // Act
        clean(&f);

        // Assert
        assert!(files.iter().all(|p| !p.exists()));
    }

    #[test]
    fn a_failed_file_delete_does_not_stop_the_others() {
        let _g = pinned();
        // Arrange
        let f = fx("gc-delete-fails");
        f.write("a.rs", "fn a() {}\n");
        f.run();
        let grace = STALE_STORE_GRACE_MS;
        let v2 = old_store(&f, 2, grace + 1000);
        let v3 = old_store(&f, 3, grace + 1000);
        let stuck = v2[0].clone();
        let hooks = Hooks {
            delete_file: Some(Box::new(move |p| {
                if p == stuck {
                    Err(std::io::Error::other("in use"))
                } else {
                    std::fs::remove_file(p)
                }
            })),
            ..Hooks::default()
        };

        // Act
        let outcome = clean_with(&f, &hooks, &TxHooks::default());

        // Assert
        assert_eq!(outcome, Cleanup::Done);
        assert!(v2[0].exists());
        assert!(v3.iter().all(|p| !p.exists()));
    }

    #[test]
    fn a_failing_chunk_skips_the_cleanup_and_leaves_the_content() {
        let _g = pinned();
        // Arrange
        let f = fx("gc-fail");
        f.write("a.rs", "fn a() {}\n");
        f.run();
        add_unreferenced_content(&f, 3);
        let before = count(&raw(&f), "SELECT COUNT(*) FROM content");
        let opts = Options {
            cleanup_hooks: Hooks {
                fail_cleanup: true,
                ..Hooks::default()
            },
            ..Options::default()
        };

        // Act
        let report = index::run(&f.root, &f.index, &f.worktree, false, &opts)
            .expect("the run still succeeds");

        // Assert
        assert_eq!(report.cleanup, Cleanup::Skipped);
        assert_eq!(count(&raw(&f), "SELECT COUNT(*) FROM content"), before);
    }

    #[test]
    fn freed_pages_return_to_the_file_after_the_delete_loop() {
        let _g = pinned();
        // Arrange
        let f = fx("gc-vacuum");
        f.write("a.rs", "fn a() {}\n");
        f.run();
        add_unreferenced_content(&f, 450);
        let index = f.index.clone();
        let during = Arc::new(Mutex::new(0i64));
        let log = Arc::clone(&during);
        let hooks = Hooks {
            before_vacuum: Some(Box::new(move || {
                let conn = Connection::open(&index).expect("probe");
                *log.lock().expect("log") = count(&conn, "PRAGMA freelist_count");
            })),
            ..Hooks::default()
        };

        // Act
        clean_with(&f, &hooks, &TxHooks::default());

        // Assert
        let during = *during.lock().expect("during");
        assert!(during > 0);
        assert!(count(&raw(&f), "PRAGMA freelist_count") < during);
    }

    #[test]
    fn the_vacuum_returns_every_page_it_is_allowed_to() {
        let _g = pinned();
        // Arrange
        let f = fx("gc-vacuum-many");
        f.write("a.rs", "fn a() {}\n");
        f.run();
        {
            let conn = raw(&f);
            conn.execute_batch("CREATE TABLE junk(x BLOB)")
                .expect("junk");
            for _ in 0..200 {
                conn.execute("INSERT INTO junk VALUES (zeroblob(4096))", [])
                    .expect("row");
            }
            conn.execute_batch("DROP TABLE junk").expect("drop");
        }
        let before = count(&raw(&f), "PRAGMA freelist_count");
        assert!(before > 100, "{before}");

        // Act
        clean(&f);

        // Assert
        assert!(count(&raw(&f), "PRAGMA freelist_count") < before - 100);
    }

    #[test]
    fn a_dead_worktree_whose_run_lock_is_held_keeps_its_row() {
        let _g = pinned();
        // Arrange
        let f = fx("gc-dead-running");
        f.write("a.rs", "fn a() {}\n");
        f.run();
        add_worktree_row(&f, "busy", "/no/such/root", 0, Some(1));
        let lock = f.index.with_file_name("busy.lock");
        let held = std::fs::File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock)
            .expect("lock");
        held.try_lock().expect("take");

        // Act
        clean(&f);

        // Assert
        assert!(has_worktree(&f, "busy"));
        assert!(lock.exists());
        held.unlock().expect("release");

        // Act
        clean(&f);

        // Assert
        assert!(!has_worktree(&f, "busy"));
        assert!(!lock.exists());
    }

    #[test]
    fn a_worktree_that_started_a_run_recently_is_not_dead() {
        let _g = pinned();
        // Arrange
        let f = fx("gc-dead-started");
        f.write("a.rs", "fn a() {}\n");
        f.run();
        add_worktree_row(
            &f,
            "started",
            "/no/such/root",
            NOW - 100 * DEAD_ROOT_GRACE_MS,
            Some(NOW - 100 * DEAD_ROOT_GRACE_MS),
        );
        raw(&f)
            .execute(
                "UPDATE worktrees SET last_run_started = ?1 WHERE key = 'started'",
                [NOW - 1000],
            )
            .expect("started");

        // Act
        clean(&f);

        // Assert
        assert!(has_worktree(&f, "started"));
    }

    #[test]
    fn the_content_of_a_dead_worktree_goes_in_the_same_cleanup() {
        let _g = pinned();
        // Arrange
        let f = fx("gc-dead-content");
        f.write("a.rs", "fn a() {}\n");
        f.run();
        add_worktree_row(&f, "dead", "/no/such/root", 0, Some(1));
        {
            let conn = raw(&f);
            conn.execute(
                "INSERT INTO content(sha256, grammar, parser, partial) VALUES (?1, 'text', 'p', 0)",
                [format!("{:0>64}", "dead")],
            )
            .expect("content");
            let content = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO manifest(worktree, path, content, sha256, size, mtime_ns, status)
                 SELECT id, 'only_dead.rs', ?1, '', 0, 0, 'ok' FROM worktrees WHERE key = 'dead'",
                [content],
            )
            .expect("manifest");
        }
        let before = count(&raw(&f), "SELECT COUNT(*) FROM content");

        // Act
        clean(&f);

        // Assert
        assert_eq!(count(&raw(&f), "SELECT COUNT(*) FROM content"), before - 1);
    }

    #[test]
    fn an_old_store_with_no_sidecars_is_deleted_with_its_lock() {
        let _g = pinned();
        // Arrange
        let f = fx("gc-stale-bare");
        f.write("a.rs", "fn a() {}\n");
        f.run();
        let files = old_store(&f, 3, STALE_STORE_GRACE_MS + 1000);
        std::fs::remove_file(&files[1]).expect("wal");
        std::fs::remove_file(&files[2]).expect("shm");

        // Act
        clean(&f);

        // Assert
        assert!(!files[0].exists());
        assert!(!files[3].exists());
    }

    #[test]
    fn a_run_passes_its_clock_to_the_cleanup() {
        let _g = pinned();
        // Arrange
        let f = fx("gc-run-clock");
        f.write("a.rs", "fn a() {}\n");
        f.run();
        add_worktree_row(
            &f,
            "dead",
            "/no/such/root",
            NOW - 100 * DEAD_ROOT_GRACE_MS,
            None,
        );
        let lock = f.index.with_file_name("dead.lock");
        std::fs::write(&lock, b"").expect("lock");
        let opts = Options {
            now_ms: Some(NOW),
            ..Options::default()
        };

        // Act
        index::run(&f.root, &f.index, &f.worktree, false, &opts).expect("run");

        // Assert
        assert!(!has_worktree(&f, "dead"));
        assert!(!lock.exists());
    }

    fn automerge_value(f: &Fx) -> Option<i64> {
        raw(f)
            .query_row(
                "SELECT v FROM chunks_fts_config WHERE k = 'automerge'",
                [],
                |r| r.get(0),
            )
            .ok()
    }

    #[test]
    fn the_merge_is_deferred_during_the_delete_loops_and_restored_after() {
        let _g = pinned();
        // Arrange
        let f = fx("gc-automerge");
        f.write("a.rs", "fn a() {}\n");
        f.run();
        add_unreferenced_content(&f, 120);
        let index = f.index.clone();
        let during = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&during);
        let hooks = Hooks {
            after_cleanup_chunk: Some(Box::new(move |_| {
                let conn = Connection::open(&index).expect("probe");
                let value = conn
                    .query_row(
                        "SELECT v FROM chunks_fts_config WHERE k = 'automerge'",
                        [],
                        |r| r.get::<_, i64>(0),
                    )
                    .ok();
                log.lock().expect("log").push(value);
            })),
            ..Hooks::default()
        };

        // Act
        let outcome = clean_with(&f, &hooks, &TxHooks::default());

        // Assert
        assert_eq!(outcome, Cleanup::Done);
        let during = during.lock().expect("during");
        assert_eq!(during.len(), 3);
        assert!(
            during.iter().all(|v| *v == Some(AUTOMERGE_OFF)),
            "{during:?}"
        );
        assert_eq!(automerge_value(&f), Some(AUTOMERGE_DEFAULT));
    }

    #[test]
    fn a_failed_cleanup_still_restores_the_merge_setting() {
        let _g = pinned();
        // Arrange
        let f = fx("gc-automerge-fail");
        f.write("a.rs", "fn a() {}\n");
        f.run();
        add_unreferenced_content(&f, 3);
        let hooks = Hooks {
            fail_cleanup: true,
            ..Hooks::default()
        };

        // Act
        let outcome = clean_with(&f, &hooks, &TxHooks::default());

        // Assert
        assert_eq!(outcome, Cleanup::Skipped);
        assert_ne!(automerge_value(&f), Some(AUTOMERGE_OFF));
    }

    #[test]
    fn a_setting_left_off_by_a_stopped_cleanup_is_restored_on_the_next_one() {
        let _g = pinned();
        // Arrange
        let f = fx("gc-automerge-left");
        f.write("a.rs", "fn a() {}\n");
        f.run();
        raw(&f)
            .execute(
                "INSERT INTO chunks_fts(chunks_fts, rank) VALUES ('automerge', 0)",
                [],
            )
            .expect("leave it off");
        assert_eq!(automerge_value(&f), Some(AUTOMERGE_OFF));

        // Act
        clean(&f);

        // Assert
        assert_eq!(automerge_value(&f), Some(AUTOMERGE_DEFAULT));
    }
}
