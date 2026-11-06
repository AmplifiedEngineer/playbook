// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The migration framework that moves a database from its old path into the
//! data folder.
//!
//! A step is a [`Move`]: an old file, a new file and a [`Policy`]. Running it
//! is idempotent and safe against concurrent processes:
//!
//! * The old file is copied with `VACUUM INTO`, which reads a consistent
//!   snapshot including the WAL, then checked table by table, then renamed
//!   into place. Nothing whose data exists nowhere else is deleted.
//! * The old file and its `-wal` and `-shm` are renamed to `*.migrated` once
//!   the copy is verified. A `*.migrated` copy from an earlier run holds data
//!   already in the new file, so it is replaced, and only one copy stays.
//! * When the new file already exists (for example an older binary kept
//!   writing the old path and recreated it), the rows are merged with
//!   `INSERT OR IGNORE`, which cannot duplicate a row with a primary key.
//! * A corrupt old file is left alone and reported on each run.
//! * The old file's write lock is held from the copy to the rename, and
//!   merged tables must have a primary key, so a repeat cannot double count.

use playbook_common::atomic::{acquire_dir_lock_aged, ensure_private_dir, LONG_LOCK_STALE_AGE};
use playbook_store::{Connection, OpenFlags};
use std::cell::Cell;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How a step treats a new file that already exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// Merge these tables from the old file with `INSERT OR IGNORE`: a row
    /// already in the new file wins.
    MergeTables(&'static [&'static str]),
    /// Merge these tables, each with its own conflict rule.
    Merge(&'static [(&'static str, Rule)]),
    /// The data is a rebuildable cache: keep the new file and set the old one
    /// aside.
    KeepNew,
    /// A rebuildable cache that is moved by renaming the files (no copy, no
    /// checks). When the new file exists the old one is set aside. A writer
    /// holding the old file, or any failure, leaves it where it is and the
    /// cache is rebuilt at the new path.
    Cache,
}

/// How one merged table resolves a row that exists in both files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// The row in the new file stays.
    Ignore,
    /// The row with the greater value in this column wins (an ISO timestamp
    /// or a counter), so the older binary's later write is not lost.
    Newer(&'static str),
    /// Rows form series by these columns. A series that exists in the new
    /// file stays whole; one that does not is copied whole. Never a mix of
    /// two histories.
    Series(&'static [&'static str]),
}

/// One database to move into the data folder.
#[derive(Clone, Debug)]
pub struct Move {
    pub old: PathBuf,
    pub new: PathBuf,
    pub policy: Policy,
}

/// What a step did.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// There was no old file.
    Nothing,
    /// The old file became the new file.
    Moved,
    /// Rows from the old file were merged into the existing new file.
    Merged { inserted: u64 },
    /// The old file was set aside because the new file wins.
    SetAside,
    /// The old file is unreadable and was left in place.
    Corrupt(String),
}

const SIDE_SUFFIXES: [&str; 3] = ["", "-wal", "-shm"];
const LOCK_RETRIES: u32 = 300;
const LOCK_DELAY: Duration = Duration::from_millis(20);

/// Test seam for the lock budget. A test that holds the migrate lock on
/// purpose would otherwise wait out all of [`LOCK_RETRIES`]: about 6 s, and
/// 30 s on a busy macOS runner. A debug build reads this variable, so a test
/// can shorten the budget of the binary it runs.
#[doc(hidden)]
pub const TEST_LOCK_RETRIES_ENV: &str = "PLAYBOOK_TEST_MIGRATE_LOCK_RETRIES";

thread_local! {
    static TEST_LOCK_RETRIES: Cell<Option<u32>> = const { Cell::new(None) };
}

/// Runs `f` with a lock budget of `retries` on this thread, for a test that
/// holds the migrate lock and runs in the test process.
#[doc(hidden)]
pub fn with_test_lock_retries<T>(retries: u32, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<u32>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_LOCK_RETRIES.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(TEST_LOCK_RETRIES.with(|c| c.replace(Some(retries))));
    f()
}

/// The lock budget. A release build always uses the full budget; a debug
/// build takes a test setting, which can only shorten it.
fn lock_retries() -> u32 {
    if !cfg!(debug_assertions) {
        return LOCK_RETRIES;
    }
    let env = std::env::var(TEST_LOCK_RETRIES_ENV).ok();
    test_budget(TEST_LOCK_RETRIES.with(Cell::get), env.as_deref())
}

/// The thread setting wins over the variable. A value that does not parse is
/// ignored, and every value is clamped to `1..=LOCK_RETRIES`.
fn test_budget(thread: Option<u32>, env: Option<&str>) -> u32 {
    thread
        .or_else(|| env?.trim().parse().ok())
        .map_or(LOCK_RETRIES, |n| n.clamp(1, LOCK_RETRIES))
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn q(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn tables(conn: &Connection, schema: &str) -> playbook_store::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT name FROM {schema}.sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'"
    ))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.collect()
}

fn columns(conn: &Connection, schema: &str, table: &str) -> playbook_store::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA {schema}.table_info({})", q(table)))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    rows.collect()
}

fn count(conn: &Connection, schema: &str, table: &str) -> playbook_store::Result<i64> {
    conn.query_row(
        &format!("SELECT COUNT(*) FROM {schema}.{}", q(table)),
        [],
        |r| r.get(0),
    )
}

/// Why the old file could not be opened for a move.
enum OldError {
    /// The file is not a usable database: left in place.
    Corrupt(String),
    /// A transient failure (busy, permissions): the next run retries.
    Transient(String),
}

fn classify(e: playbook_store::Error) -> OldError {
    match e.sqlite_error_code() {
        Some(
            playbook_store::ErrorCode::NotADatabase | playbook_store::ErrorCode::DatabaseCorrupt,
        ) => OldError::Corrupt(e.to_string()),
        _ => OldError::Transient(e.to_string()),
    }
}

/// Open the old file read only and run an integrity check.
fn open_old(old: &Path) -> Result<Connection, OldError> {
    let conn =
        Connection::open_with_flags(old, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(classify)?;
    conn.busy_timeout(Duration::from_millis(5000))
        .map_err(classify)?;
    let verdict: String = conn
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .map_err(classify)?;
    if verdict != "ok" {
        return Err(OldError::Corrupt(verdict));
    }
    Ok(conn)
}

/// Take the old file's write lock until dropped, so no writer can commit
/// between the copy and the rename. A writer that holds it longer than the
/// wait makes the move retry on the next run.
fn lock_old(old: &Path) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(old, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|e| e.to_string())?;
    conn.busy_timeout(Duration::from_millis(2000))
        .map_err(|e| e.to_string())?;
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| format!("the old database is busy: {e}"))?;
    Ok(conn)
}

/// Row counts per table of `conn`, sorted by table name.
fn table_counts(conn: &Connection) -> Result<Vec<(String, i64)>, String> {
    let mut out = Vec::new();
    let mut names = tables(conn, "main").map_err(|e| e.to_string())?;
    names.sort();
    for t in names {
        let n = count(conn, "main", &t).map_err(|e| e.to_string())?;
        out.push((t, n));
    }
    Ok(out)
}

/// Rename the old file and its sidecars to `*.migrated` after its data is
/// verified in the new file. An earlier `*.migrated` copy that reads as a
/// sound database came from such a move too (an older binary recreated the
/// old path and it was merged again), so its data is in the new file: it is
/// removed first and only the newest copy stays. An earlier copy that does
/// not read was parked as corrupt and may hold data found nowhere else, so
/// it stays and the new copy takes a numbered name.
pub fn set_aside_verified(old: &Path) -> Result<(), String> {
    let kept = with_suffix(old, ".migrated");
    if kept.exists() && open_old(&kept).is_err() {
        return set_aside(old);
    }
    for side in SIDE_SUFFIXES {
        let earlier = with_suffix(&kept, side);
        if earlier.is_file() {
            fs::remove_file(&earlier)
                .map_err(|e| format!("could not remove {}: {e}", earlier.display()))?;
        }
    }
    set_aside(old)
}

/// Rename the old file and its sidecars to `*.migrated`; when that name is
/// taken the name gets a numeric suffix. Nothing is overwritten. Used for a
/// file whose data could not be checked, such as a corrupt one.
pub fn set_aside(old: &Path) -> Result<(), String> {
    let mut n = 0u32;
    let suffix = loop {
        let s = if n == 0 {
            ".migrated".to_string()
        } else {
            format!(".migrated.{n}")
        };
        if !with_suffix(old, &s).exists() {
            break s;
        }
        n += 1;
    };
    for side in SIDE_SUFFIXES {
        let from = with_suffix(old, side);
        if from.exists() {
            // SQLite pairs a main file with `<main>-wal`, so the sidecars
            // keep that shape next to the renamed main file.
            let to = with_suffix(&with_suffix(old, &suffix), side);
            fs::rename(&from, &to)
                .map_err(|e| format!("could not rename {}: {e}", from.display()))?;
        }
    }
    Ok(())
}

fn copy_then_verify(step: &Move, old_conn: &Connection) -> Result<(), String> {
    let tmp = with_suffix(&step.new, &format!(".tmp-{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let result = (|| {
        old_conn
            .execute("VACUUM INTO ?1", [tmp.to_string_lossy().as_ref()])
            .map_err(|e| format!("copy failed: {e}"))?;
        let copy = Connection::open_with_flags(&tmp, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?;
        if table_counts(&copy)? != table_counts(old_conn)? {
            return Err("row counts differ after the copy".to_string());
        }
        drop(copy);
        playbook_common::atomic::make_db_file_private(&tmp)?;
        // A hard link fails when the target exists, unlike rename, which
        // would silently replace a file another process just created.
        fs::hard_link(&tmp, &step.new).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                "the new file appeared during the copy".to_string()
            } else {
                e.to_string()
            }
        })?;
        Ok(())
    })();
    let _ = fs::remove_file(&tmp);
    result
}

/// Remove partial copies a killed process left next to `new`. Called with
/// the migrate lock held.
fn remove_stale_tmp(new: &Path) {
    let (Some(dir), Some(name)) = (new.parent(), new.file_name()) else {
        return;
    };
    let prefix = format!("{}.tmp-", name.to_string_lossy());
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}

fn merge(step: &Move, rules: &[(&str, Rule)]) -> Result<u64, String> {
    let mut conn = Connection::open(&step.new).map_err(|e| e.to_string())?;
    conn.busy_timeout(Duration::from_millis(5000))
        .map_err(|e| e.to_string())?;
    conn.execute(
        "ATTACH DATABASE ?1 AS old",
        [step.old.to_string_lossy().as_ref()],
    )
    .map_err(|e| e.to_string())?;
    let old_tables = tables(&conn, "old").map_err(|e| e.to_string())?;
    // Deferred: an immediate transaction would also lock the attached old
    // file, which this process already holds a write lock on.
    let tx = conn
        .transaction_with_behavior(playbook_store::TransactionBehavior::Deferred)
        .map_err(|e| e.to_string())?;
    // Every non-empty old table must be listed, or it would be dropped when
    // the old file is set aside.
    for t in &old_tables {
        if !rules.iter().any(|(n, _)| n == t)
            && count(&tx, "old", t).map_err(|e| e.to_string())? > 0
        {
            return Err(format!(
                "old table {t} has rows but is not in the merge list"
            ));
        }
    }
    let mut inserted = 0u64;
    for (table, rule) in rules {
        if !old_tables.iter().any(|t| t == table) {
            continue;
        }
        let new_cols = columns(&tx, "main", table).map_err(|e| e.to_string())?;
        if new_cols.is_empty() {
            return Err(format!("table {table} is missing in the new file"));
        }
        if !has_primary_key(&tx, table)? {
            // Without a key INSERT OR IGNORE appends every row, so a repeat
            // after a crash would double count.
            return Err(format!("table {table} has no primary key, cannot merge"));
        }
        let old_cols = columns(&tx, "old", table).map_err(|e| e.to_string())?;
        let common: Vec<String> = new_cols
            .iter()
            .filter(|c| old_cols.contains(c))
            .map(|c| q(c))
            .collect();
        let list = common.join(", ");
        let pks = pk_columns(&tx, table)?;
        let sql = match rule {
            Rule::Ignore => format!(
                "INSERT OR IGNORE INTO main.{t} ({list}) SELECT {list} FROM old.{t}",
                t = q(table)
            ),
            Rule::Newer(col) => {
                let sets: Vec<String> = common
                    .iter()
                    .filter(|c| !pks.iter().any(|p| &q(p) == *c))
                    .map(|c| format!("{c} = excluded.{c}"))
                    .collect();
                if !common.contains(&q(col)) || sets.is_empty() {
                    return Err(format!("table {table} cannot be merged by {col}"));
                }
                let key = pks.iter().map(|p| q(p)).collect::<Vec<_>>().join(", ");
                format!(
                    "INSERT INTO main.{t} ({list}) SELECT {list} FROM old.{t} WHERE true \
                     ON CONFLICT ({key}) DO UPDATE SET {sets} \
                     WHERE excluded.{c} > main.{t}.{c}",
                    t = q(table),
                    sets = sets.join(", "),
                    c = q(col)
                )
            }
            Rule::Series(group) => {
                if group.iter().any(|g| !common.contains(&q(g))) {
                    return Err(format!("table {table} lacks a series column"));
                }
                let on = group
                    .iter()
                    .map(|g| format!("n.{c} IS o.{c}", c = q(g)))
                    .collect::<Vec<_>>()
                    .join(" AND ");
                let o_list = common
                    .iter()
                    .map(|c| format!("o.{c}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "INSERT OR IGNORE INTO main.{t} ({list}) SELECT {o_list} FROM old.{t} o \
                     WHERE NOT EXISTS (SELECT 1 FROM main.{t} n WHERE {on})",
                    t = q(table)
                )
            }
        };
        let n = tx.execute(&sql, []).map_err(|e| e.to_string())?;
        inserted += n as u64;
        if let Rule::Series(group) = rule {
            // Every old series must exist in the new file, whole or not.
            let cols = group.iter().map(|g| q(g)).collect::<Vec<_>>().join(", ");
            let on = group
                .iter()
                .map(|g| format!("n.{c} IS o.{c}", c = q(g)))
                .collect::<Vec<_>>()
                .join(" AND ");
            let lost: i64 = tx
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM (SELECT DISTINCT {cols} FROM old.{t}) o \
                         WHERE NOT EXISTS (SELECT 1 FROM main.{t} n WHERE {on})",
                        t = q(table)
                    ),
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            if lost > 0 {
                return Err(format!("{lost} series of {table} were not merged"));
            }
            continue;
        }
        // Every old row must now exist in the new file, by the primary key.
        let missing: i64 = tx
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM (SELECT {list} FROM old.{t} EXCEPT SELECT {list} FROM main.{t})",
                    t = q(table)
                ),
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        // A row whose key exists with different content stays as its rule
        // says, so only rows whose key is absent count as lost.
        if missing > 0 {
            let lost = lost_rows(&tx, table, &common)?;
            if lost > 0 {
                return Err(format!("{lost} rows of {table} were not merged"));
            }
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(inserted)
}

/// The primary key columns of `table` in `main`, in key order.
fn pk_columns(conn: &Connection, table: &str) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA main.table_info({})", q(table)))
        .map_err(|e| e.to_string())?;
    let mut cols: Vec<(i64, String)> = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(5)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .filter(|(pk, _)| *pk > 0)
        .collect();
    cols.sort();
    Ok(cols.into_iter().map(|(_, n)| n).collect())
}

fn has_primary_key(conn: &Connection, table: &str) -> Result<bool, String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA main.table_info({})", q(table)))
        .map_err(|e| e.to_string())?;
    let any = stmt
        .query_map([], |r| r.get::<_, i64>(5))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .any(|pk| pk > 0);
    Ok(any)
}

/// Old rows with no counterpart sharing the table's primary key.
fn lost_rows(conn: &Connection, table: &str, cols: &[String]) -> Result<i64, String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA main.table_info({})", q(table)))
        .map_err(|e| e.to_string())?;
    let pks: Vec<String> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(5)?)))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .filter(|(n, pk)| *pk > 0 && cols.contains(&q(n)))
        .map(|(n, _)| q(&n))
        .collect();
    if pks.is_empty() {
        return Ok(1);
    }
    let on = pks
        .iter()
        .map(|k| format!("o.{k} IS n.{k}"))
        .collect::<Vec<_>>()
        .join(" AND ");
    conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM old.{t} o WHERE NOT EXISTS (SELECT 1 FROM main.{t} n WHERE {on})",
            t = q(table)
        ),
        [],
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}

/// Run one step. Takes a lock directory next to the new file so concurrent
/// processes serialize; the second finds the old file gone and does nothing.
pub fn migrate(step: &Move) -> Result<Outcome, String> {
    if !step.old.is_file() {
        return Ok(Outcome::Nothing);
    }
    let parent = step.new.parent().ok_or("the new path has no parent")?;
    ensure_private_dir(parent)
        .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    let lock = with_suffix(&step.new, ".migrate.lock");
    // Without the lock another process may be mid-move, so do nothing and let
    // the next run retry. Only the holder removes the lock directory.
    if !acquire_dir_lock_aged(&lock, lock_retries(), LOCK_DELAY, LONG_LOCK_STALE_AGE) {
        return Ok(Outcome::Nothing);
    }
    let result = run_locked(step);
    let _ = fs::remove_dir(&lock);
    result
}

/// Move a rebuildable cache by renaming it. Never an error: a cache that
/// cannot move is simply rebuilt.
fn move_cache(step: &Move) -> Outcome {
    // A cache that is not a database is junk: set it aside and rebuild.
    if let Err(OldError::Corrupt(_)) = open_old(&step.old) {
        return match set_aside(&step.old) {
            Ok(()) => Outcome::SetAside,
            Err(_) => Outcome::Nothing,
        };
    }
    // Held until the files have moved, so no writer commits in between.
    let Ok(writer_lock) = lock_old(&step.old) else {
        return Outcome::Nothing;
    };
    // Windows cannot rename a file that is open, so it releases the lock
    // first and accepts the small window.
    #[cfg(windows)]
    let writer_lock = {
        drop(writer_lock);
        None::<Connection>
    };
    let outcome = if step.new.exists() {
        match set_aside_verified(&step.old) {
            Ok(()) => Outcome::SetAside,
            Err(_) => Outcome::Nothing,
        }
    } else {
        let mut moved = true;
        // The sidecars first, so a failure never leaves the main file
        // without the log it needs at the new path.
        for side in ["-wal", "-shm", ""] {
            let from = with_suffix(&step.old, side);
            if from.exists() && fs::rename(&from, with_suffix(&step.new, side)).is_err() {
                moved = false;
                break;
            }
        }
        if moved {
            Outcome::Moved
        } else {
            Outcome::Nothing
        }
    };
    drop(writer_lock);
    outcome
}

fn run_locked(step: &Move) -> Result<Outcome, String> {
    if !step.old.is_file() {
        return Ok(Outcome::Nothing);
    }
    remove_stale_tmp(&step.new);
    if step.policy == Policy::Cache {
        return Ok(move_cache(step));
    }
    let old_conn = match open_old(&step.old) {
        Ok(c) => c,
        Err(OldError::Corrupt(e)) => return Ok(Outcome::Corrupt(e)),
        Err(OldError::Transient(e)) => return Err(e),
    };
    // Held until the old file is set aside.
    let writer_lock = lock_old(&step.old)?;
    let outcome = if !step.new.exists() {
        copy_then_verify(step, &old_conn)?;
        Outcome::Moved
    } else {
        match step.policy {
            Policy::KeepNew | Policy::Cache => Outcome::SetAside,
            Policy::MergeTables(_) | Policy::Merge(_) => {
                let rules: Vec<(&str, Rule)> = match step.policy {
                    Policy::MergeTables(list) => list.iter().map(|t| (*t, Rule::Ignore)).collect(),
                    Policy::Merge(rules) => rules.to_vec(),
                    _ => Vec::new(),
                };
                let inserted = merge(step, &rules)?;
                drop(old_conn);
                set_aside_verified(&step.old)?;
                drop(writer_lock);
                return Ok(Outcome::Merged { inserted });
            }
        }
    };
    drop(old_conn);
    set_aside_verified(&step.old)?;
    drop(writer_lock);
    Ok(outcome)
}

/// Print one notice line, only when stderr is a terminal. Hooks and pipes
/// have a captured stderr, so they stay silent.
pub fn notice(line: &str) {
    if std::io::stderr().is_terminal() {
        eprintln!("{line}");
    }
}

/// Run `step` and report a move or a skipped corrupt file with one line.
pub fn migrate_with_notice(step: &Move, what: &str) -> Result<Outcome, String> {
    let outcome = migrate(step)?;
    match &outcome {
        Outcome::Moved => notice(&format!(
            "playbook: moved the {what} database to {}",
            step.new.display()
        )),
        Outcome::Corrupt(e) => notice(&format!(
            "playbook: left the {what} database at {} in place ({e})",
            step.old.display()
        )),
        _ => {}
    }
    Ok(outcome)
}

/// One file in the data folder, for `playbook doctor`.
pub struct Entry {
    pub path: PathBuf,
    pub bytes: u64,
}

/// Every regular file under `dir`, sorted, with its size.
pub fn list_files(dir: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(p),
                Ok(t) if t.is_file() => {
                    let bytes = e.metadata().map(|m| m.len()).unwrap_or(0);
                    out.push(Entry { path: p, bytes });
                }
                _ => {}
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Whether a path is a leftover from a finished migration.
pub fn is_migrated_leftover(path: &Path) -> bool {
    let p = path.to_string_lossy();
    p.contains(".migrated") || p.contains(".tmp-") || p.ends_with(".migrate.lock")
}

#[cfg(test)]
mod tests {
    use super::*;
    use playbook_common::test_support::scratch_dir;

    const T: &[&str] = &["t"];

    fn make(path: &Path, rows: &[(i64, &str)]) {
        let c = Connection::open(path).unwrap();
        c.pragma_update(None, "journal_mode", "WAL").unwrap();
        c.execute_batch("CREATE TABLE IF NOT EXISTS t (k INTEGER PRIMARY KEY, v TEXT NOT NULL)")
            .unwrap();
        for (k, v) in rows {
            c.execute("INSERT OR REPLACE INTO t VALUES (?1, ?2)", (k, v))
                .unwrap();
        }
    }

    fn rows(path: &Path) -> Vec<(i64, String)> {
        let c = Connection::open(path).unwrap();
        let mut s = c.prepare("SELECT k, v FROM t ORDER BY k").unwrap();
        s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    fn step(tag: &str, policy: Policy) -> (Move, PathBuf) {
        let dir = scratch_dir(tag);
        fs::create_dir_all(&dir).unwrap();
        let m = Move {
            old: dir.join("old.db"),
            new: dir.join("data").join("new.db"),
            policy,
        };
        (m, dir)
    }

    #[test]
    fn none_present_does_nothing() {
        let (m, _d) = step("dm-none", Policy::MergeTables(T));
        assert_eq!(migrate(&m).unwrap(), Outcome::Nothing);
        assert!(!m.new.exists());
    }

    #[test]
    fn only_old_is_moved_and_set_aside_with_wal_content() {
        let (m, _d) = step("dm-old", Policy::MergeTables(T));
        make(&m.old, &[(1, "a"), (2, "b")]);
        // A live connection keeps rows in the WAL.
        let keep = Connection::open(&m.old).unwrap();
        keep.execute("INSERT INTO t VALUES (3, 'c')", []).unwrap();
        assert_eq!(migrate(&m).unwrap(), Outcome::Moved);
        drop(keep);
        assert_eq!(rows(&m.new).len(), 3);
        assert!(!m.old.exists());
        assert!(with_suffix(&m.old, ".migrated").exists());
    }

    #[test]
    fn only_new_is_untouched() {
        let (m, _d) = step("dm-new", Policy::MergeTables(T));
        fs::create_dir_all(m.new.parent().unwrap()).unwrap();
        make(&m.new, &[(1, "a")]);
        assert_eq!(migrate(&m).unwrap(), Outcome::Nothing);
        assert_eq!(rows(&m.new).len(), 1);
    }

    #[test]
    fn both_merge_without_double_counting_and_rerun_is_idempotent() {
        let (m, _d) = step("dm-both", Policy::MergeTables(T));
        fs::create_dir_all(m.new.parent().unwrap()).unwrap();
        make(&m.new, &[(1, "new"), (2, "b")]);
        make(&m.old, &[(1, "old"), (3, "c")]);
        assert_eq!(migrate(&m).unwrap(), Outcome::Merged { inserted: 1 });
        assert_eq!(
            rows(&m.new),
            vec![(1, "new".into()), (2, "b".into()), (3, "c".into())]
        );
        assert_eq!(migrate(&m).unwrap(), Outcome::Nothing);
        // An older binary writes the old path again: merged once, quietly.
        fs::write(with_suffix(&m.old, ".migrated-wal"), b"stale").unwrap();
        make(&m.old, &[(3, "c"), (4, "d")]);
        assert_eq!(migrate(&m).unwrap(), Outcome::Merged { inserted: 1 });
        assert_eq!(rows(&m.new).len(), 4);
        // The earlier copy's rows are in the new file, so one copy stays,
        // and a log left next to the earlier copy goes with it.
        assert!(with_suffix(&m.old, ".migrated").exists());
        assert!(!with_suffix(&m.old, ".migrated.1").exists());
        let wal = fs::read(with_suffix(&m.old, ".migrated-wal")).unwrap_or_default();
        assert_ne!(wal, b"stale".to_vec(), "the earlier copy's log stayed");
        assert_eq!(rows(&with_suffix(&m.old, ".migrated")).len(), 2);
    }

    #[test]
    fn a_corrupt_earlier_copy_is_kept_and_the_new_one_is_numbered() {
        let (m, _d) = step("dm-keep-corrupt", Policy::MergeTables(T));
        fs::create_dir_all(m.new.parent().unwrap()).unwrap();
        make(&m.new, &[(1, "a")]);
        let parked = with_suffix(&m.old, ".migrated");
        // A page or more, so SQLite reads it as a bad header, not an empty file.
        let garbage = b"not a database, parked as corrupt by an earlier run. ".repeat(100);
        fs::write(&parked, &garbage).unwrap();
        make(&m.old, &[(2, "b")]);
        assert_eq!(migrate(&m).unwrap(), Outcome::Merged { inserted: 1 });
        assert_eq!(fs::read(&parked).unwrap(), garbage);
        assert!(with_suffix(&m.old, ".migrated.1").exists());
    }

    #[test]
    fn corrupt_old_is_left_in_place() {
        let (m, _d) = step("dm-bad", Policy::MergeTables(T));
        fs::write(
            &m.old,
            b"this is not a database at all, just text padding.....",
        )
        .unwrap();
        assert!(matches!(migrate(&m).unwrap(), Outcome::Corrupt(_)));
        assert!(m.old.exists());
        assert!(!m.new.exists());
    }

    #[test]
    fn keep_new_sets_the_old_cache_aside() {
        let (m, _d) = step("dm-keep", Policy::KeepNew);
        fs::create_dir_all(m.new.parent().unwrap()).unwrap();
        make(&m.new, &[(1, "a")]);
        make(&m.old, &[(9, "z")]);
        assert_eq!(migrate(&m).unwrap(), Outcome::SetAside);
        assert_eq!(rows(&m.new), vec![(1, "a".into())]);
        assert!(!m.old.exists());
    }

    #[test]
    fn concurrent_writer_and_two_migrators_lose_nothing() {
        let (m, _d) = step("dm-conc", Policy::MergeTables(T));
        make(&m.old, &[(1, "a")]);
        let m2 = m.clone();
        let m3 = m.clone();
        let t1 = std::thread::spawn(move || migrate(&m2));
        let t2 = std::thread::spawn(move || migrate(&m3));
        let r1 = t1.join().unwrap().unwrap();
        let r2 = t2.join().unwrap().unwrap();
        let moved = [&r1, &r2].iter().filter(|r| ***r == Outcome::Moved).count();
        assert_eq!(moved, 1);
        assert_eq!(rows(&m.new), vec![(1, "a".into())]);
    }

    #[test]
    fn held_lock_makes_the_step_wait_and_do_nothing() {
        let (m, _d) = step("dm-held", Policy::MergeTables(T));
        make(&m.old, &[(1, "a")]);
        fs::create_dir_all(m.new.parent().unwrap()).unwrap();
        fs::create_dir(with_suffix(&m.new, ".migrate.lock")).unwrap();
        assert_eq!(
            with_test_lock_retries(2, || migrate(&m)).unwrap(),
            Outcome::Nothing
        );
        assert!(m.old.exists());
        assert!(!m.new.exists());
        // The lock that was not ours is still there.
        assert!(with_suffix(&m.new, ".migrate.lock").is_dir());
    }

    #[test]
    fn the_test_lock_budget_only_shortens_and_is_restored() {
        assert_eq!(with_test_lock_retries(2, lock_retries), 2);
        assert_eq!(TEST_LOCK_RETRIES.with(Cell::get), None);
    }

    #[test]
    fn the_test_lock_budget_reads_the_thread_then_the_variable() {
        assert_eq!(test_budget(None, None), LOCK_RETRIES);
        assert_eq!(test_budget(None, Some("2")), 2);
        assert_eq!(test_budget(None, Some(" 2 ")), 2);
        assert_eq!(test_budget(Some(3), Some("2")), 3);
        assert_eq!(test_budget(None, Some("0")), 1);
        assert_eq!(test_budget(Some(0), None), 1);
        assert_eq!(test_budget(None, Some("100000")), LOCK_RETRIES);
        assert_eq!(test_budget(None, Some("abc")), LOCK_RETRIES);
        assert_eq!(test_budget(None, Some("-1")), LOCK_RETRIES);
    }

    #[test]
    fn a_writer_holding_the_old_file_makes_the_move_fail_and_keep_it() {
        let (m, _d) = step("dm-writer", Policy::MergeTables(T));
        make(&m.old, &[(1, "a")]);
        let w = Connection::open(&m.old).unwrap();
        w.execute_batch("BEGIN IMMEDIATE; INSERT INTO t VALUES (2, 'b');")
            .unwrap();
        assert!(migrate(&m).is_err());
        assert!(m.old.exists());
        assert!(!m.new.exists());
        w.execute_batch("COMMIT").unwrap();
        drop(w);
        assert_eq!(migrate(&m).unwrap(), Outcome::Moved);
        assert_eq!(rows(&m.new).len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn moved_file_and_folder_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let (m, _d) = step("dm-perm", Policy::MergeTables(T));
        make(&m.old, &[(1, "a")]);
        migrate(&m).unwrap();
        let file = fs::metadata(&m.new).unwrap().permissions().mode() & 0o777;
        let dir = fs::metadata(m.new.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!((file, dir), (0o600, 0o700));
    }

    #[test]
    fn merge_refuses_a_table_without_a_primary_key_and_keeps_the_old_file() {
        let (m, _d) = step("dm-nopk", Policy::MergeTables(&["t"]));
        fs::create_dir_all(m.new.parent().unwrap()).unwrap();
        for p in [&m.old, &m.new] {
            let c = Connection::open(p).unwrap();
            c.execute_batch("CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('x');")
                .unwrap();
        }
        assert!(migrate(&m).is_err());
        assert!(m.old.exists());
    }

    #[test]
    fn merge_refuses_an_unlisted_non_empty_old_table() {
        let (m, _d) = step("dm-unlisted", Policy::MergeTables(T));
        fs::create_dir_all(m.new.parent().unwrap()).unwrap();
        make(&m.new, &[(1, "a")]);
        make(&m.old, &[(2, "b")]);
        Connection::open(&m.old)
            .unwrap()
            .execute_batch(
                "CREATE TABLE extra (k INTEGER PRIMARY KEY); INSERT INTO extra VALUES (1);",
            )
            .unwrap();
        assert!(migrate(&m).is_err());
        assert!(m.old.exists());
        assert_eq!(rows(&m.new).len(), 1);
    }

    #[test]
    fn sidecars_follow_the_renamed_main_file() {
        let (m, _d) = step("dm-side", Policy::KeepNew);
        fs::create_dir_all(m.new.parent().unwrap()).unwrap();
        make(&m.new, &[(1, "a")]);
        make(&m.old, &[(2, "b")]);
        fs::write(with_suffix(&m.old, "-wal"), b"").unwrap();
        migrate(&m).unwrap();
        assert!(with_suffix(&m.old, ".migrated").exists());
        assert!(with_suffix(&m.old, ".migrated-wal").exists());
    }

    #[test]
    fn a_stale_partial_copy_is_removed_before_the_move() {
        let (m, _d) = step("dm-tmp", Policy::MergeTables(T));
        make(&m.old, &[(1, "a")]);
        fs::create_dir_all(m.new.parent().unwrap()).unwrap();
        let stale = with_suffix(&m.new, ".tmp-99999");
        fs::write(&stale, b"partial").unwrap();
        migrate(&m).unwrap();
        assert!(!stale.exists());
    }

    fn make_log(path: &Path, rows: &[(i64, i64, &str, &str)]) {
        let c = Connection::open(path).unwrap();
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS ev (g INTEGER NOT NULL, n INTEGER NOT NULL, ts TEXT NOT NULL, v TEXT NOT NULL, PRIMARY KEY (g, n));",
        )
        .unwrap();
        for (g, n, ts, v) in rows {
            c.execute(
                "INSERT OR REPLACE INTO ev VALUES (?1,?2,?3,?4)",
                (g, n, ts, v),
            )
            .unwrap();
        }
    }

    fn log(path: &Path) -> Vec<(i64, i64, String)> {
        let c = Connection::open(path).unwrap();
        let mut s = c.prepare("SELECT g, n, v FROM ev ORDER BY g, n").unwrap();
        s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    static NEWER: &[(&str, Rule)] = &[("ev", Rule::Newer("ts"))];
    static SERIES: &[(&str, Rule)] = &[("ev", Rule::Series(&["g"]))];

    #[test]
    fn newer_rule_keeps_the_later_write_from_either_file() {
        let (m, _d) = step("dm-newer", Policy::Merge(NEWER));
        fs::create_dir_all(m.new.parent().unwrap()).unwrap();
        make_log(
            &m.new,
            &[
                (1, 1, "2026-01-02", "new-later"),
                (2, 1, "2026-01-01", "new-older"),
            ],
        );
        make_log(
            &m.old,
            &[
                (1, 1, "2026-01-01", "old-older"),
                (2, 1, "2026-01-02", "old-later"),
                (3, 1, "2026-01-01", "only-old"),
            ],
        );
        migrate(&m).unwrap();
        let got: Vec<String> = log(&m.new).into_iter().map(|r| r.2).collect();
        assert_eq!(got, vec!["new-later", "old-later", "only-old"]);
    }

    #[test]
    fn series_rule_never_mixes_two_histories() {
        let (m, _d) = step("dm-series", Policy::Merge(SERIES));
        fs::create_dir_all(m.new.parent().unwrap()).unwrap();
        make_log(&m.new, &[(1, 1, "t", "new-1")]);
        make_log(
            &m.old,
            &[
                (1, 1, "t", "old-1"),
                (1, 2, "t", "old-2"),
                (2, 1, "t", "old-g2"),
            ],
        );
        assert!(matches!(migrate(&m).unwrap(), Outcome::Merged { .. }));
        assert_eq!(
            log(&m.new),
            vec![(1, 1, "new-1".into()), (2, 1, "old-g2".into())]
        );
        // Merging again changes nothing.
        make_log(&m.old, &[(1, 1, "t", "old-1"), (1, 2, "t", "old-2")]);
        migrate(&m).unwrap();
        assert_eq!(log(&m.new).len(), 2);
    }

    #[test]
    fn a_cache_moves_by_rename_with_its_rows() {
        let (m, _d) = step("dm-cache", Policy::Cache);
        make(&m.old, &[(1, "a"), (2, "b")]);
        // Committed frames stay in the WAL while a connection is open.
        let keep = Connection::open(&m.old).unwrap();
        keep.execute("INSERT INTO t VALUES (3, 'c')", []).unwrap();
        assert_eq!(migrate(&m).unwrap(), Outcome::Moved);
        for side in ["", "-wal", "-shm"] {
            assert!(!with_suffix(&m.old, side).exists(), "{side}");
        }
        assert_eq!(rows(&m.new).len(), 3);
        drop(keep);
        assert_eq!(rows(&m.new).len(), 3);
    }

    #[test]
    fn a_cache_with_a_new_file_sets_the_old_one_aside() {
        let (m, _d) = step("dm-cache-both", Policy::Cache);
        fs::create_dir_all(m.new.parent().unwrap()).unwrap();
        make(&m.new, &[(1, "new")]);
        make(&m.old, &[(9, "old")]);
        assert_eq!(migrate(&m).unwrap(), Outcome::SetAside);
        assert_eq!(rows(&m.new), vec![(1, "new".into())]);
        assert!(with_suffix(&m.old, ".migrated").exists());
    }

    #[test]
    fn a_cache_held_by_a_writer_stays_put_then_is_set_aside() {
        let (m, _d) = step("dm-cache-busy", Policy::Cache);
        make(&m.old, &[(1, "a")]);
        let w = Connection::open(&m.old).unwrap();
        w.execute_batch("BEGIN IMMEDIATE; INSERT INTO t VALUES (2, 'b');")
            .unwrap();
        assert_eq!(migrate(&m).unwrap(), Outcome::Nothing);
        assert!(m.old.exists());
        assert!(!m.new.exists());
        // The index is rebuilt at the new path; the stale old file is then
        // set aside instead of replacing it.
        w.execute_batch("ROLLBACK").unwrap();
        drop(w);
        fs::create_dir_all(m.new.parent().unwrap()).unwrap();
        make(&m.new, &[(7, "fresh")]);
        assert_eq!(migrate(&m).unwrap(), Outcome::SetAside);
        assert_eq!(rows(&m.new), vec![(7, "fresh".into())]);
    }

    #[test]
    fn a_junk_cache_is_set_aside() {
        let (m, _d) = step("dm-cache-junk", Policy::Cache);
        fs::write(&m.old, vec![b'x'; 4096]).unwrap();
        assert_eq!(migrate(&m).unwrap(), Outcome::SetAside);
        assert!(with_suffix(&m.old, ".migrated").exists());
    }

    #[test]
    fn two_cache_migrators_move_it_exactly_once() {
        let (m, _d) = step("dm-cache-race", Policy::Cache);
        make(&m.old, &[(1, "a")]);
        let (m2, m3) = (m.clone(), m.clone());
        let a = std::thread::spawn(move || migrate(&m2));
        let b = std::thread::spawn(move || migrate(&m3));
        let r = [a.join().unwrap().unwrap(), b.join().unwrap().unwrap()];
        assert_eq!(r.iter().filter(|o| **o == Outcome::Moved).count(), 1);
        assert_eq!(rows(&m.new), vec![(1, "a".into())]);
    }

    #[test]
    fn list_files_reports_sizes_and_leftovers() {
        let d = scratch_dir("dm-list");
        fs::create_dir_all(d.join("x")).unwrap();
        fs::write(d.join("x").join("a.db"), b"12345").unwrap();
        fs::write(d.join("a.db.migrated"), b"1").unwrap();
        fs::write(d.join("b.db.tmp-1"), b"1").unwrap();
        let files = list_files(&d);
        assert_eq!(files.len(), 3);
        assert!(files.iter().any(|f| f.bytes == 5));
        assert!(files.iter().any(|f| is_migrated_leftover(&f.path)));
    }
}
