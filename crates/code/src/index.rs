// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The indexing run. Every run hashes every listed file, so an edit that keeps
//! both size and mtime is still caught. Changed files are read once, prepared
//! on worker threads and written by the calling thread, one `SAVEPOINT` per
//! file inside a transaction per batch. A file is the atomic unit: an error in
//! one file rolls back only that file.

use super::db::{self, OpenMode};
use super::host;
use super::lang;
use super::walker::{self, Skipped};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_WORKERS: usize = 8;

/// Injectable knobs. Production uses [`Options::default`].
pub struct Options {
    pub limits: walker::Limits,
    /// Changed files written per transaction.
    pub batch_size: usize,
    pub max_threads: usize,
    /// Lines per text chunk.
    pub chunk_lines: usize,
    #[cfg(test)]
    pub hooks: Hooks,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            limits: walker::Limits::default(),
            batch_size: 200,
            max_threads: MAX_WORKERS,
            chunk_lines: 60,
            #[cfg(test)]
            hooks: Hooks::default(),
        }
    }
}

/// Test hooks. `after_batch_commit` runs on the writer after each batch commit
/// and an `Err` stops the run there. `before_file_insert` runs inside a file's
/// savepoint and an `Err` rolls that file back.
#[cfg(test)]
pub type BatchHook = Box<dyn Fn(usize) -> Result<(), String> + Send + Sync>;
#[cfg(test)]
pub type FileHook = Box<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

#[cfg(test)]
#[derive(Default)]
pub struct Hooks {
    pub after_batch_commit: Option<BatchHook>,
    pub before_file_insert: Option<FileHook>,
}

impl Options {
    fn after_batch(&self, _batch: usize) -> Result<(), String> {
        #[cfg(test)]
        if let Some(hook) = &self.hooks.after_batch_commit {
            return hook(_batch);
        }
        Ok(())
    }

    fn before_file(&self, _path: &str) -> Result<(), String> {
        #[cfg(test)]
        if let Some(hook) = &self.hooks.before_file_insert {
            return hook(_path);
        }
        Ok(())
    }
}

/// What one run did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Files with stored text after the run (status `ok` or `partial`).
    pub indexed: usize,
    /// Files written this run, new or edited.
    pub changed: usize,
    /// Files whose hash matched the stored one.
    pub unchanged: usize,
    /// Files parsed for symbols this run.
    pub parsed: usize,
    /// Rows dropped because the file is no longer listed.
    pub removed: usize,
    /// Files listed by git but not indexed.
    pub skipped: usize,
    /// Files that could not be read or written this run.
    pub failed: usize,
}

struct Chunk {
    start_line: usize,
    end_line: usize,
    body: String,
}

struct Prepared {
    path: String,
    sha: String,
    size: u64,
    mtime_ns: i64,
    status: &'static str,
    chunks: Vec<Chunk>,
    lang: Option<&'static str>,
    symbols: Vec<lang::Symbol>,
    imports: Vec<lang::Import>,
    /// The file went through a Tree-sitter parser.
    parsed: bool,
}

enum Outcome {
    Unchanged,
    Touch {
        path: String,
        size: u64,
        mtime_ns: i64,
    },
    Write(Box<Prepared>),
    Skip(Skipped),
    Missing(String),
    Failed {
        path: String,
        error: String,
    },
}

pub(crate) struct Existing {
    pub(crate) sha: String,
    pub(crate) status: String,
    pub(crate) error: Option<String>,
    pub(crate) size: i64,
    pub(crate) mtime_ns: i64,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// Splits an identifier into lowercase pieces at `_`, at a lower or digit to
/// upper boundary, and before the last capital of an acronym run.
pub fn identifier_pieces(word: &str) -> Vec<String> {
    let mut pieces: Vec<String> = Vec::new();
    for part in word.split('_').filter(|p| !p.is_empty()) {
        let chars: Vec<char> = part.chars().collect();
        let mut current = String::new();
        for (i, &c) in chars.iter().enumerate() {
            if i > 0 {
                let prev = chars[i - 1];
                let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
                let boundary = c.is_uppercase()
                    && (prev.is_lowercase()
                        || prev.is_ascii_digit()
                        || (prev.is_uppercase() && next_lower));
                if boundary {
                    pieces.push(std::mem::take(&mut current));
                }
            }
            current.extend(c.to_lowercase());
        }
        if !current.is_empty() {
            pieces.push(current);
        }
    }
    pieces
}

/// Whether `word` is ASCII with no capital and no `_`, so `identifier_pieces`
/// would return the word itself as its only piece.
fn is_plain_lowercase(word: &str) -> bool {
    word.bytes()
        .all(|b| b.is_ascii() && b != b'_' && !b.is_ascii_uppercase())
}

/// The FTS body for a chunk: its text, then the pieces of every identifier
/// whose pieces differ from the identifier itself, each once.
fn fts_body(text: &str) -> String {
    let mut seen: HashSet<String> = HashSet::new();
    let mut extra = String::new();
    for word in text.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        // Most words are plain lowercase ASCII: one piece equal to the word,
        // so there is nothing to split or add.
        if word.is_empty() || is_plain_lowercase(word) {
            continue;
        }
        let pieces = identifier_pieces(word);
        if pieces.len() == 1 && pieces[0] == word.to_lowercase() {
            continue;
        }
        for piece in pieces {
            if !seen.contains(&piece) {
                extra.push(' ');
                extra.push_str(&piece);
                seen.insert(piece);
            }
        }
    }
    format!("{text}\n{extra}")
}

/// Windows of `chunk_lines` lines with their FTS bodies.
fn make_chunks(text: &str, chunk_lines: usize) -> Vec<Chunk> {
    let lines: Vec<&str> = text.lines().collect();
    lines
        .chunks(chunk_lines.max(1))
        .enumerate()
        .map(|(i, window)| {
            let start_line = i * chunk_lines.max(1) + 1;
            Chunk {
                start_line,
                end_line: start_line + window.len() - 1,
                body: fts_body(&window.join("\n")),
            }
        })
        .collect()
}

/// What the parser found in one file.
#[derive(Default)]
struct Syntax {
    lang: Option<&'static str>,
    symbols: Vec<lang::Symbol>,
    imports: Vec<lang::Import>,
    partial: bool,
    parsed: bool,
}

fn prepare(root: &Path, path: &str, existing: Option<&Existing>, opts: &Options) -> Outcome {
    let full = root.join(path);
    let meta = match std::fs::metadata(&full) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Outcome::Missing(path.to_string())
        }
        Err(e) => {
            return Outcome::Failed {
                path: path.to_string(),
                error: e.to_string(),
            }
        }
    };
    let bytes = match std::fs::read(&full) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Outcome::Missing(path.to_string())
        }
        Err(e) => {
            return Outcome::Failed {
                path: path.to_string(),
                error: e.to_string(),
            }
        }
    };
    let sha = playbook_common::hash::sha256_hex(&bytes);
    let size = bytes.len() as u64;
    let mtime = walker::mtime_ns(&meta);
    if let Some(old) = existing {
        if old.sha == sha && (old.status == "ok" || old.status == "partial") {
            if i64::try_from(size).ok() == Some(old.size) && old.mtime_ns == mtime {
                return Outcome::Unchanged;
            }
            return Outcome::Touch {
                path: path.to_string(),
                size,
                mtime_ns: mtime,
            };
        }
    }
    let valid = std::str::from_utf8(&bytes).is_ok();
    let text = String::from_utf8_lossy(&bytes);
    let mut syntax = Syntax::default();
    if let Some(language) = lang::Lang::for_path(path) {
        syntax.lang = Some(language.name());
        match lang::parse(language, &bytes) {
            Ok(tree) => {
                let found = lang::extract(language, &tree, &bytes);
                syntax.partial = tree.partial;
                syntax.symbols = found.symbols;
                syntax.imports = found.imports;
                syntax.parsed = true;
            }
            Err(_) => syntax.partial = true,
        }
    }
    Outcome::Write(Box::new(Prepared {
        path: path.to_string(),
        sha,
        size,
        mtime_ns: mtime,
        status: if valid && !syntax.partial {
            "ok"
        } else {
            "partial"
        },
        chunks: make_chunks(&text, opts.chunk_lines),
        lang: syntax.lang,
        symbols: syntax.symbols,
        imports: syntax.imports,
        parsed: syntax.parsed,
    }))
}

/// Deletes a file's chunks, FTS rows (first, while the chunk ids exist),
/// symbols and imports.
fn clear_children(conn: &Connection, file_id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM chunks_fts WHERE rowid IN (SELECT id FROM chunks WHERE file_id = ?1)",
        [file_id],
    )?;
    conn.execute("DELETE FROM chunks WHERE file_id = ?1", [file_id])?;
    conn.execute("DELETE FROM symbols WHERE file_id = ?1", [file_id])?;
    conn.execute("DELETE FROM imports WHERE file_id = ?1", [file_id])?;
    Ok(())
}

fn file_id(conn: &Connection, path: &str) -> rusqlite::Result<Option<i64>> {
    conn.query_row("SELECT id FROM files WHERE path = ?1", [path], |r| r.get(0))
        .optional()
}

#[allow(clippy::too_many_arguments)]
fn upsert_file(
    conn: &Connection,
    path: &str,
    sha: &str,
    size: u64,
    mtime_ns: i64,
    status: &str,
    error: Option<&str>,
    lang: Option<&str>,
    now_s: i64,
) -> rusqlite::Result<i64> {
    let size = i64::try_from(size).unwrap_or(i64::MAX);
    if let Some(id) = file_id(conn, path)? {
        clear_children(conn, id)?;
        conn.execute(
            "UPDATE files SET sha256 = ?2, size = ?3, mtime_ns = ?4, indexed_at = ?5,
                 status = ?6, error = ?7, lang = ?8 WHERE id = ?1",
            params![id, sha, size, mtime_ns, now_s, status, error, lang],
        )?;
        Ok(id)
    } else {
        conn.execute(
            "INSERT INTO files(path, sha256, size, mtime_ns, indexed_at, status, error, lang)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![path, sha, size, mtime_ns, now_s, status, error, lang],
        )?;
        Ok(conn.last_insert_rowid())
    }
}

/// Stores a file's symbols and imports; `parent_id` follows the extractor's parent index.
fn insert_syntax(conn: &Connection, file_id: i64, p: &Prepared) -> rusqlite::Result<()> {
    let to_i64 = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
    let mut ids: Vec<i64> = Vec::with_capacity(p.symbols.len());
    for sym in &p.symbols {
        let parent_id = sym.parent.and_then(|i| ids.get(i).copied());
        conn.execute(
            "INSERT INTO symbols(file_id, parent_id, name, qualified_name, kind, signature,
                 signature_cut, exported, start_byte, end_byte, start_line, end_line)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                file_id,
                parent_id,
                sym.name,
                sym.qualified_name,
                sym.kind,
                sym.signature,
                sym.signature_cut,
                sym.exported,
                to_i64(sym.start_byte),
                to_i64(sym.end_byte),
                to_i64(sym.start_line),
                to_i64(sym.end_line),
            ],
        )?;
        ids.push(conn.last_insert_rowid());
    }
    for import in &p.imports {
        conn.execute(
            "INSERT INTO imports(file_id, module, names, line) VALUES (?1, ?2, ?3, ?4)",
            params![file_id, import.module, import.names, to_i64(import.line)],
        )?;
    }
    Ok(())
}

fn apply(conn: &Connection, outcome: &Outcome, opts: &Options, now_s: i64) -> Result<(), String> {
    let sql = |e: rusqlite::Error| e.to_string();
    match outcome {
        Outcome::Write(p) => {
            opts.before_file(&p.path)?;
            let id = upsert_file(
                conn, &p.path, &p.sha, p.size, p.mtime_ns, p.status, None, p.lang, now_s,
            )
            .map_err(sql)?;
            insert_syntax(conn, id, p).map_err(sql)?;
            for chunk in &p.chunks {
                conn.execute(
                    "INSERT INTO chunks(file_id, start_line, end_line) VALUES (?1, ?2, ?3)",
                    params![id, chunk.start_line as i64, chunk.end_line as i64],
                )
                .map_err(sql)?;
                conn.execute(
                    "INSERT INTO chunks_fts(rowid, body) VALUES (?1, ?2)",
                    params![conn.last_insert_rowid(), chunk.body],
                )
                .map_err(sql)?;
            }
        }
        Outcome::Skip(s) => {
            opts.before_file(&s.path)?;
            upsert_file(
                conn,
                &s.path,
                "",
                s.size,
                s.mtime_ns,
                "skipped",
                Some(s.reason.as_str()),
                None,
                now_s,
            )
            .map_err(sql)?;
        }
        Outcome::Touch {
            path,
            size,
            mtime_ns,
        } => {
            conn.execute(
                "UPDATE files SET size = ?2, mtime_ns = ?3 WHERE path = ?1",
                params![path, i64::try_from(*size).unwrap_or(i64::MAX), mtime_ns],
            )
            .map_err(sql)?;
        }
        Outcome::Unchanged | Outcome::Missing(_) | Outcome::Failed { .. } => {}
    }
    Ok(())
}

fn mark_failed(conn: &Connection, path: &str, error: &str, now_s: i64) -> rusqlite::Result<()> {
    let changed = conn.execute(
        "UPDATE files SET status = 'failed', error = ?2 WHERE path = ?1",
        params![path, error],
    )?;
    if changed == 0 {
        conn.execute(
            "INSERT INTO files(path, sha256, size, mtime_ns, indexed_at, status, error)
             VALUES (?1, '', 0, 0, ?3, 'failed', ?2)",
            params![path, error, now_s],
        )?;
    }
    Ok(())
}

fn write_batch(
    conn: &mut Connection,
    batch: &[Outcome],
    opts: &Options,
    report: &mut Report,
) -> Result<(), String> {
    let now_s = now_ms() / 1000;
    let mut tx = conn.transaction().map_err(|e| e.to_string())?;
    let mut failures: Vec<(String, String)> = Vec::new();
    for outcome in batch {
        let path = match outcome {
            Outcome::Write(p) => p.path.clone(),
            Outcome::Skip(s) => s.path.clone(),
            Outcome::Touch { path, .. } => path.clone(),
            Outcome::Failed { path, error } => {
                failures.push((path.clone(), error.clone()));
                continue;
            }
            Outcome::Unchanged | Outcome::Missing(_) => continue,
        };
        let sp = tx.savepoint().map_err(|e| e.to_string())?;
        match apply(&sp, outcome, opts, now_s) {
            Ok(()) => {
                sp.commit().map_err(|e| e.to_string())?;
                if let Outcome::Write(p) = outcome {
                    report.changed += 1;
                    if p.parsed {
                        report.parsed += 1;
                    }
                }
            }
            Err(error) => {
                drop(sp);
                failures.push((path, error));
            }
        }
    }
    for (path, error) in failures {
        mark_failed(&tx, &path, &error, now_s).map_err(|e| e.to_string())?;
        report.failed += 1;
    }
    tx.commit().map_err(|e| e.to_string())
}

pub(crate) fn load_existing(conn: &Connection) -> Result<HashMap<String, Existing>, String> {
    let mut stmt = conn
        .prepare("SELECT path, sha256, status, error, size, mtime_ns FROM files")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Existing {
                    sha: r.get(1)?,
                    status: r.get(2)?,
                    error: r.get(3)?,
                    size: r.get(4)?,
                    mtime_ns: r.get(5)?,
                },
            ))
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<HashMap<_, _>, _>>()
        .map_err(|e| e.to_string())
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<(), String> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// The lock file beside the index. Held for the whole run and released by the
/// OS if the process dies.
pub fn lock_path(index: &Path) -> PathBuf {
    index.with_file_name("index.lock")
}

/// The held run lock. Dropping it unlocks explicitly, so a copy of the file
/// descriptor that a concurrent fork briefly holds cannot keep the lock alive.
pub struct RunLock(File);

impl Drop for RunLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Takes the exclusive run lock, or says another run holds it.
pub fn acquire_lock(index: &Path) -> Result<RunLock, String> {
    if let Some(parent) = index.parent() {
        host::ensure_private_dir(parent)
            .map_err(|e| format!("failed to create directory {}: {e}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path(index))
        .map_err(|e| format!("failed to open the index lock: {e}"))?;
    match file.try_lock() {
        Ok(()) => Ok(RunLock(file)),
        Err(TryLockError::WouldBlock) => Err("another index run is in progress".to_string()),
        Err(TryLockError::Error(e)) => Err(format!("failed to lock the index: {e}")),
    }
}

/// Runs one indexing pass over the repo at `root`, storing into `index`.
pub fn run(root: &Path, index: &Path, rebuild: bool, opts: &Options) -> Result<Report, String> {
    let _lock = acquire_lock(index)?;
    if rebuild {
        db::delete_index_files(index)?;
    }
    let mut conn = db::open_index(index, OpenMode::Index).map_err(|e| e.to_string())?;
    let started = now_ms();
    set_meta(&conn, "last_run_started", &started.to_string())?;
    set_meta(&conn, "repo_root", &root.display().to_string())?;
    set_meta(&conn, "worktree_id", &host::worktree_id())?;
    set_meta(&conn, "tool_version", host::tool_version())?;

    let found = walker::discover(root, &opts.limits)?;
    let existing = load_existing(&conn)?;
    let mut report = Report {
        skipped: found.skipped.len(),
        ..Report::default()
    };
    let mut listed: HashSet<String> = found.files.iter().cloned().collect();
    listed.extend(found.skipped.iter().map(|s| s.path.clone()));

    let mut pending: Vec<Outcome> = Vec::new();
    for skipped in &found.skipped {
        let same = existing.get(&skipped.path).is_some_and(|old| {
            old.status == "skipped"
                && old.error.as_deref() == Some(skipped.reason.as_str())
                && i64::try_from(skipped.size).ok() == Some(old.size)
                && old.mtime_ns == skipped.mtime_ns
        });
        if !same {
            pending.push(Outcome::Skip(skipped.clone()));
        }
    }

    let workers = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .min(opts.max_threads)
        .clamp(1, MAX_WORKERS);
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::sync_channel::<Outcome>(64);
    let mut batch_no = 0usize;
    let mut batch: Vec<Outcome> = Vec::new();
    let mut run_error: Option<String> = None;

    std::thread::scope(|scope| {
        for _ in 0..workers {
            let tx = tx.clone();
            let (files, existing, next) = (&found.files, &existing, &next);
            scope.spawn(move || loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(path) = files.get(i) else { break };
                if tx
                    .send(prepare(root, path, existing.get(path), opts))
                    .is_err()
                {
                    break;
                }
            });
        }
        drop(tx);

        let mut flush = |batch: &mut Vec<Outcome>, report: &mut Report| -> Result<(), String> {
            if batch.is_empty() {
                return Ok(());
            }
            write_batch(&mut conn, batch, opts, report)?;
            batch.clear();
            batch_no += 1;
            opts.after_batch(batch_no)
        };
        let mut accept = |outcome: Outcome, batch: &mut Vec<Outcome>, report: &mut Report| {
            match outcome {
                Outcome::Unchanged => {
                    report.unchanged += 1;
                    return Ok(());
                }
                Outcome::Missing(path) => {
                    listed.remove(&path);
                    return Ok(());
                }
                Outcome::Failed { path, error } => {
                    batch.push(Outcome::Failed { path, error });
                }
                Outcome::Touch {
                    path,
                    size,
                    mtime_ns,
                } => {
                    report.unchanged += 1;
                    batch.push(Outcome::Touch {
                        path,
                        size,
                        mtime_ns,
                    });
                }
                other => batch.push(other),
            }
            if batch.len() >= opts.batch_size.max(1) {
                flush(batch, report)
            } else {
                Ok(())
            }
        };
        for outcome in pending.drain(..).chain(rx.iter()) {
            if let Err(e) = accept(outcome, &mut batch, &mut report) {
                run_error = Some(e);
                break;
            }
        }
        if run_error.is_none() {
            if let Err(e) = flush(&mut batch, &mut report) {
                run_error = Some(e);
            }
        }
        drop(rx);
    });
    if let Some(e) = run_error {
        return Err(e);
    }

    report.removed = remove_unlisted(&mut conn, &listed)?;
    report.indexed = conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE status IN ('ok', 'partial')",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map_err(|e| e.to_string())
        .map(|n| usize::try_from(n).unwrap_or(0))?;
    set_meta(
        &conn,
        "last_run_completed",
        &now_ms().max(started).to_string(),
    )?;
    Ok(report)
}

/// Drops rows (FTS rows first) for files that are no longer listed.
fn remove_unlisted(conn: &mut Connection, listed: &HashSet<String>) -> Result<usize, String> {
    let rows: Vec<(i64, String)> = {
        let mut stmt = conn
            .prepare("SELECT id, path FROM files")
            .map_err(|e| e.to_string())?;
        let mapped = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?;
        mapped
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?
    };
    let gone: Vec<i64> = rows
        .into_iter()
        .filter(|(_, path)| !listed.contains(path))
        .map(|(id, _)| id)
        .collect();
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for id in &gone {
        clear_children(&tx, *id).map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM files WHERE id = ?1", [id])
            .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(gone.len())
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{fx, pinned};
    use super::super::walker::test_repo::{git_in, write};
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    fn fts_paths(conn: &Connection, term: &str) -> Vec<String> {
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT f.path FROM chunks_fts
                 JOIN chunks c ON c.id = chunks_fts.rowid
                 JOIN files f ON f.id = c.file_id
                 WHERE chunks_fts MATCH ?1 ORDER BY f.path",
            )
            .expect("prepare");
        let q = format!("\"{}\"", term.replace('"', "\"\""));
        stmt.query_map([q], |r| r.get(0))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect()
    }

    fn paths(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT path FROM files ORDER BY path")
            .expect("prepare");
        stmt.query_map([], |r| r.get(0))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect()
    }

    #[test]
    fn an_edited_file_is_changed_and_the_rest_are_unchanged() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-edit");
        for n in ["a", "b", "c"] {
            write(&f.root, &format!("{n}.rs"), b"fn x() {}\n");
        }
        let first = f.run();
        assert_eq!((first.changed, first.indexed), (3, 3));
        write(&f.root, "b.rs", b"fn y() {}\nfn z() {}\n");
        f.set_mtime("b.rs", 1_000_000);

        // Act
        let second = f.run();

        // Assert
        assert_eq!((second.changed, second.unchanged), (1, 2));
    }

    #[test]
    fn an_edit_that_keeps_size_and_mtime_is_still_caught_by_the_hash() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-samesize");
        write(&f.root, "a.rs", b"fn aaa() {}\n");
        f.set_mtime("a.rs", 2_000_000);
        f.run();
        write(&f.root, "a.rs", b"fn bbb() {}\n");
        f.set_mtime("a.rs", 2_000_000);

        // Act
        let report = f.run();

        // Assert
        assert_eq!(report.changed, 1);
        assert_eq!(fts_paths(&f.conn(), "bbb"), vec!["a.rs"]);
    }

    #[test]
    fn a_rename_drops_the_old_path_and_removes_its_text_from_search() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-rename");
        write(&f.root, "old.rs", b"fn uniquemarker() {}\n");
        f.run();
        std::fs::rename(f.root.join("old.rs"), f.root.join("new.rs")).expect("rename");

        // Act
        let report = f.run();

        // Assert
        assert_eq!(report.removed, 1);
        let conn = f.conn();
        assert_eq!(paths(&conn), vec!["new.rs"]);
        assert_eq!(fts_paths(&conn, "uniquemarker"), vec!["new.rs"]);
    }

    #[test]
    fn a_deleted_file_leaves_no_rows_and_no_search_hit() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-delete");
        write(&f.root, "gone.rs", b"fn vanishingtext() {}\n");
        write(&f.root, "stay.rs", b"fn other() {}\n");
        git_in(&f.root, &["add", "."]);
        f.run();
        std::fs::remove_file(f.root.join("gone.rs")).expect("remove");

        // Act
        let report = f.run();

        // Assert: still listed by git, gone from disk, counted as removed.
        assert_eq!(report.removed, 1);
        let conn = f.conn();
        assert_eq!(paths(&conn), vec!["stay.rs"]);
        assert!(fts_paths(&conn, "vanishingtext").is_empty());
        let orphans: i64 = conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
            .expect("count");
        let stay_chunks: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM chunks c JOIN files f ON f.id = c.file_id WHERE f.path = 'stay.rs'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(orphans, stay_chunks);
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_file_keeps_its_old_rows_and_counts_as_failed() {
        use std::os::unix::fs::PermissionsExt;
        let _g = pinned();
        // Arrange
        let f = fx("idx-unreadable");
        write(&f.root, "a.rs", b"fn keepme() {}\n");
        f.run();
        write(&f.root, "a.rs", b"fn keepme2() {}\n");
        std::fs::set_permissions(f.root.join("a.rs"), std::fs::Permissions::from_mode(0o000))
            .expect("chmod");
        let is_root = rustix::process::geteuid().is_root();

        // Act
        let report = f.run();

        // Assert
        std::fs::set_permissions(f.root.join("a.rs"), std::fs::Permissions::from_mode(0o644))
            .expect("chmod back");
        if !is_root {
            assert_eq!(report.failed, 1);
            let conn = f.conn();
            assert_eq!(fts_paths(&conn, "keepme"), vec!["a.rs"]);
            let status: String = conn
                .query_row("SELECT status FROM files WHERE path = 'a.rs'", [], |r| {
                    r.get(0)
                })
                .expect("status");
            assert_eq!(status, "failed");
        }
    }

    #[test]
    fn swapping_the_file_set_leaves_no_row_outside_the_new_set() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-branch");
        write(&f.root, "one.rs", b"fn one() {}\n");
        write(&f.root, "two.rs", b"fn two() {}\n");
        f.run();
        std::fs::remove_file(f.root.join("one.rs")).expect("rm");
        write(&f.root, "three.rs", b"fn three() {}\n");

        // Act
        f.run();

        // Assert
        assert_eq!(paths(&f.conn()), vec!["three.rs", "two.rs"]);
    }

    #[test]
    fn readers_see_exactly_the_committed_batch_while_the_writer_is_paused() {
        let _g = pinned();
        // Arrange: 10 changed files, batches of 3, writer paused after batch one.
        let f = fx("idx-paused");
        for i in 0..10 {
            write(&f.root, &format!("f{i}.rs"), b"fn old() {}\n");
        }
        let opts = Options {
            batch_size: 3,
            max_threads: 1,
            ..Options::default()
        };
        f.run_with(&opts);
        for i in 0..10 {
            write(&f.root, &format!("f{i}.rs"), b"fn newword() {}\n");
        }
        let (paused_tx, paused_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        let paused_tx = Mutex::new(paused_tx);
        let opts = Options {
            batch_size: 3,
            max_threads: 1,
            hooks: Hooks {
                after_batch_commit: Some(Box::new(move |n| {
                    if n == 1 {
                        let _ = paused_tx.lock().map(|t| t.send(()));
                        let _ = release_rx
                            .lock()
                            .map(|r| r.recv_timeout(Duration::from_secs(30)));
                    }
                    Ok(())
                })),
                before_file_insert: None,
            },
            ..Options::default()
        };
        struct Release(mpsc::Sender<()>);
        impl Drop for Release {
            fn drop(&mut self) {
                let _ = self.0.send(());
            }
        }
        let root = f.root.clone();
        let index = f.index.clone();
        let writer = std::thread::spawn(move || run(&root, &index, false, &opts));
        let _release = Release(release_tx);

        // Act
        paused_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("writer reached the pause");
        let a = f.conn();
        let b = f.conn();
        let count = |c: &Connection, term: &str| fts_paths(c, term).len();

        // Assert
        assert_eq!(count(&a, "newword"), 3);
        assert_eq!(count(&b, "newword"), 3);
        assert_eq!(count(&a, "old"), 7);
        drop(_release);
        let report = writer.join().expect("join").expect("run completes");
        assert_eq!(report.changed, 10);
        assert_eq!(count(&f.conn(), "newword"), 10);
    }

    #[test]
    fn a_second_run_is_refused_while_the_lock_is_held() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-lock");
        write(&f.root, "a.rs", b"x\n");
        let held = acquire_lock(&f.index).expect("first lock");

        // Act
        let second = run(&f.root, &f.index, false, &Options::default());

        // Assert
        assert_eq!(
            second.expect_err("must be refused"),
            "another index run is in progress"
        );
        drop(held);
        assert!(run(&f.root, &f.index, false, &Options::default()).is_ok());
    }

    #[test]
    fn the_lock_is_released_while_a_copy_of_the_descriptor_is_still_open() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-lock-dup");
        write(&f.root, "a.rs", b"x\n");
        let held = acquire_lock(&f.index).expect("first lock");
        let copy = held.0.try_clone().expect("clone");

        // Act
        drop(held);

        // Assert
        assert!(acquire_lock(&f.index).is_ok());
        drop(copy);
    }

    #[test]
    fn a_run_stopped_after_it_started_reads_as_interrupted() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-interrupted");
        write(&f.root, "a.rs", b"x\n");
        let opts = Options {
            hooks: Hooks {
                after_batch_commit: Some(Box::new(|_| Err("stop".to_string()))),
                before_file_insert: None,
            },
            ..Options::default()
        };

        // Act
        let stopped = run(&f.root, &f.index, false, &opts);

        // Assert
        assert_eq!(stopped.expect_err("stopped"), "stop");
        let conn = f.conn();
        let meta = |k: &str| -> Option<i64> {
            conn.query_row("SELECT value FROM meta WHERE key = ?1", [k], |r| {
                r.get::<_, String>(0)
            })
            .ok()
            .and_then(|v| v.parse().ok())
        };
        assert!(meta("last_run_started").is_some());
        assert!(meta("last_run_completed").is_none());
        drop(conn);
        f.run();
        let conn = f.conn();
        let done: i64 = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'last_run_completed'",
                [],
                |r| r.get::<_, String>(0),
            )
            .expect("completed")
            .parse()
            .expect("number");
        let started: i64 = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'last_run_started'",
                [],
                |r| r.get::<_, String>(0),
            )
            .expect("started")
            .parse()
            .expect("number");
        assert!(done >= started);
    }

    #[test]
    fn an_error_inside_one_file_rolls_back_only_that_file() {
        let _g = pinned();
        // Arrange: 7 files, batches of 3, the fifth written file fails.
        let f = fx("idx-savepoint");
        for i in 0..7 {
            write(&f.root, &format!("f{i}.rs"), b"fn old() {}\n");
        }
        f.run();
        for i in 0..7 {
            write(&f.root, &format!("f{i}.rs"), b"fn newword() {}\n");
        }
        let opts = Options {
            batch_size: 3,
            max_threads: 1,
            hooks: Hooks {
                after_batch_commit: None,
                before_file_insert: Some(Box::new(|path| {
                    if path == "f4.rs" {
                        Err("injected".to_string())
                    } else {
                        Ok(())
                    }
                })),
            },
            ..Options::default()
        };

        // Act
        let report = f.run_with(&opts);

        // Assert
        assert_eq!(report.failed, 1);
        let conn = f.conn();
        assert_eq!(fts_paths(&conn, "newword").len(), 6);
        assert_eq!(fts_paths(&conn, "old"), vec!["f4.rs"]);
        let err: String = conn
            .query_row("SELECT error FROM files WHERE path = 'f4.rs'", [], |r| {
                r.get(0)
            })
            .expect("error");
        assert_eq!(err, "injected");
    }

    #[test]
    fn chunk_windows_follow_the_line_counts() {
        // Arrange
        let cases = [
            (0usize, vec![]),
            (1, vec![(1, 1)]),
            (59, vec![(1, 59)]),
            (60, vec![(1, 60)]),
            (61, vec![(1, 60), (61, 61)]),
            (120, vec![(1, 60), (61, 120)]),
        ];
        for (lines, want) in cases {
            let text = (0..lines).map(|_| "x").collect::<Vec<_>>().join("\n");

            // Act
            let got: Vec<(usize, usize)> = make_chunks(&text, 60)
                .iter()
                .map(|c| (c.start_line, c.end_line))
                .collect();

            // Assert
            assert_eq!(got, want, "{lines} lines");
        }
    }

    #[test]
    fn identifier_pieces_split_snake_camel_acronym_and_digit_forms() {
        // Arrange, Act, Assert
        let cases: [(&str, &[&str]); 5] = [
            ("processPayment", &["process", "payment"]),
            ("create_payment", &["create", "payment"]),
            ("HTTPServer", &["http", "server"]),
            ("v2Api", &["v2", "api"]),
            ("_private", &["private"]),
        ];
        for (word, want) in cases {
            assert_eq!(identifier_pieces(word), want, "{word}");
        }
    }

    #[test]
    fn fts_body_appends_each_distinct_identifier_piece_once() {
        // Arrange: plain words add nothing, repeats add nothing, and a capital
        // sigma lowercases differently as a char than at the end of a word.
        let text = "fn processPayment() { create_payment(); processPayment(); let userId = \
                    HTTPServer; plain v2Api \u{3a3}\u{3af}\u{3c3}\u{3c5}\u{3c6}\u{3bf}\u{3c2} \
                    \u{3a3}\u{391}\u{3a3} }";

        // Act
        let body = fts_body(text);

        // Assert
        let want_extra = " process payment create user id http server v2 api \u{3c3}\u{3b1}\u{3c3}";
        assert_eq!(body, format!("{text}\n{want_extra}"));
        assert_eq!(fts_body("plain words only"), "plain words only\n");
    }

    #[test]
    fn a_pieces_search_finds_camel_and_snake_identifiers() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-pieces");
        write(&f.root, "a.ts", b"function processPayment() {}\n");
        write(&f.root, "b.py", b"def create_payment():\n    pass\n");
        write(&f.root, "c.rs", b"fn unrelated() {}\n");

        // Act
        f.run();

        // Assert
        let conn = f.conn();
        assert_eq!(fts_paths(&conn, "payment"), vec!["a.ts", "b.py"]);
        assert!(fts_paths(&conn, "create").contains(&"b.py".to_string()));
    }

    #[test]
    fn skipped_files_get_rows_with_size_and_mtime_and_stay_unchanged() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-skipped");
        write(&f.root, "bin.dat", b"a\0b");
        write(&f.root, "ok.rs", b"fn ok() {}\n");

        // Act
        let first = f.run();
        let second = f.run();

        // Assert
        assert_eq!((first.skipped, first.indexed), (1, 1));
        assert_eq!(second.skipped, 1);
        let conn = f.conn();
        let (status, error, size): (String, String, i64) = conn
            .query_row(
                "SELECT status, error, size FROM files WHERE path = 'bin.dat'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("row");
        assert_eq!(
            (status.as_str(), error.as_str(), size),
            ("skipped", "binary", 3)
        );
    }

    fn symbol_names(conn: &Connection, path: &str) -> Vec<String> {
        let mut stmt = conn
            .prepare(
                "SELECT s.qualified_name FROM symbols s JOIN files f ON f.id = s.file_id
                 WHERE f.path = ?1 ORDER BY s.start_line, s.id",
            )
            .expect("prepare");
        stmt.query_map([path], |r| r.get::<_, String>(0))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect()
    }

    #[test]
    fn symbols_and_imports_are_stored_per_language_with_parents_and_paths() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-symbols");
        write(
            &f.root,
            "a.rs",
            b"use std::io;
struct P;
impl P { fn go(&self) {} }
",
        );
        write(&f.root, "b.ts", b"export class C {\n  run() {}\n}\n");
        write(&f.root, "c.py", b"import os\ndef top():\n    pass\n");
        write(&f.root, "d.txt", b"plain text\n");
        write(
            &f.root,
            "e.go",
            b"package e\nimport \"os\"\nfunc Top() {}\n",
        );

        // Act
        let report = f.run();

        // Assert
        let conn = f.conn();
        assert_eq!(symbol_names(&conn, "a.rs"), vec!["P", "P::go"]);
        assert_eq!(symbol_names(&conn, "b.ts"), vec!["C", "C.run"]);
        assert_eq!(symbol_names(&conn, "c.py"), vec!["top"]);
        assert!(symbol_names(&conn, "d.txt").is_empty());
        let parent: Option<String> = conn
            .query_row(
                "SELECT p.name FROM symbols s JOIN symbols p ON p.id = s.parent_id
                 WHERE s.qualified_name = 'C.run'",
                [],
                |r| r.get(0),
            )
            .optional()
            .expect("parent");
        assert_eq!(parent.as_deref(), Some("C"));
        let langs: Vec<(String, Option<String>)> = conn
            .prepare("SELECT path, lang FROM files ORDER BY path")
            .expect("prepare")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect();
        assert_eq!(langs[0].1.as_deref(), Some("rust"));
        assert_eq!(langs[1].1.as_deref(), Some("typescript"));
        assert_eq!(langs[2].1.as_deref(), Some("python"));
        assert_eq!(langs[3].1, None);
        assert_eq!(langs[4].1.as_deref(), Some("go"));
        assert_eq!(symbol_names(&conn, "e.go"), vec!["Top"]);
        let imports: i64 = conn
            .query_row("SELECT COUNT(*) FROM imports", [], |r| r.get(0))
            .expect("count");
        assert_eq!(imports, 3);
        assert_eq!(report.parsed, 4);
    }

    #[test]
    fn a_renamed_function_leaves_only_the_new_name() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-rename-symbol");
        write(&f.root, "a.rs", b"fn old_name() {}\n");
        f.run();

        // Act
        write(&f.root, "a.rs", b"fn new_name() {}\n");
        f.run();

        // Assert
        assert_eq!(symbol_names(&f.conn(), "a.rs"), vec!["new_name"]);
    }

    #[test]
    fn a_syntax_error_marks_the_file_partial_and_keeps_earlier_symbols() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-partial-syntax");
        write(&f.root, "a.rs", b"fn good() {}\nfn broken( {\n");

        // Act
        f.run();

        // Assert
        let conn = f.conn();
        let status: String = conn
            .query_row("SELECT status FROM files WHERE path = 'a.rs'", [], |r| {
                r.get(0)
            })
            .expect("status");
        assert_eq!(status, "partial");
        assert!(symbol_names(&conn, "a.rs").contains(&"good".to_string()));
    }

    #[test]
    fn parsed_counts_five_then_zero_then_one_after_an_edit() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-parsed-counts");
        for i in 0..5 {
            write(
                &f.root,
                &format!("f{i}.rs"),
                format!("fn f{i}() {{}}\n").as_bytes(),
            );
        }
        write(&f.root, "a.txt", b"one\n");
        write(&f.root, "b.md", b"two\n");

        // Act, Assert
        assert_eq!(f.run().parsed, 5);
        assert_eq!(f.run().parsed, 0);
        write(&f.root, "f0.rs", b"fn changed() {}\n");
        assert_eq!(f.run().parsed, 1);
    }

    #[test]
    fn invalid_utf8_is_indexed_lossily_and_marked_partial() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-utf8");
        write(&f.root, "a.txt", b"hello \xff\xfe world\n");

        // Act
        f.run();

        // Assert
        let conn = f.conn();
        let status: String = conn
            .query_row("SELECT status FROM files WHERE path = 'a.txt'", [], |r| {
                r.get(0)
            })
            .expect("status");
        assert_eq!(status, "partial");
        assert_eq!(fts_paths(&conn, "world"), vec!["a.txt"]);
    }

    #[test]
    fn rebuild_deletes_the_cache_and_indexes_everything_again() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-rebuild");
        write(&f.root, "a.rs", b"fn a() {}\n");
        f.run();

        // Act
        let report = run(&f.root, &f.index, true, &Options::default()).expect("rebuild");

        // Assert
        assert_eq!((report.changed, report.unchanged), (1, 0));
    }

    /// Append only. One row per extractor revision, with the digest of what the
    /// extractors and the chunker produce for `DIGEST_FIXTURES`. Never edit or
    /// delete an old row.
    const EXTRACTOR_HISTORY: &[(u32, &str)] = &[(
        1,
        "1d212189516758012408041d84182c27c80d60cf9fc6b5b92bea2dcba75ed47f",
    )];

    /// One source per grammar, with enough syntax to touch symbols, imports,
    /// nesting and the identifier pieces of the full text body.
    const DIGEST_FIXTURES: &[(&str, &str)] = &[
        (
            "a.rs",
            "use std::fmt::Display;\n/// Doc.\npub struct HttpClient { pub retry_count: u32 }\nimpl HttpClient {\n    pub fn send_request(&self) -> u32 { 1 }\n}\nfn helper() {}\n",
        ),
        (
            "a.py",
            "import os\nfrom typing import List\n\nclass UserStore:\n    def load_user(self, user_id):\n        return user_id\n\ndef main():\n    pass\n",
        ),
        (
            "a.js",
            "import { readFile } from 'fs';\nexport function loadConfig(path) { return readFile(path); }\nconst maxRetries = 3;\n",
        ),
        (
            "a.ts",
            "import type { Config } from './config';\nexport interface RequestOptions { retryCount: number }\nexport class ApiClient { sendRequest(o: RequestOptions): void {} }\n",
        ),
        (
            "a.tsx",
            "import React from 'react';\nexport function UserCard(props: { userName: string }) { return <div>{props.userName}</div>; }\n",
        ),
        (
            "a.go",
            "package main\n\nimport \"fmt\"\n\ntype HttpServer struct{ Port int }\n\nfunc (s *HttpServer) StartServer() { fmt.Println(s.Port) }\n\nfunc main() {}\n",
        ),
    ];

    fn extractor_digest() -> String {
        let mut out = String::new();
        for (path, source) in DIGEST_FIXTURES {
            let language = lang::Lang::for_path(path).expect("fixture language");
            let bytes = source.as_bytes();
            let parsed = lang::parse(language, bytes).expect("parse");
            let found = lang::extract(language, &parsed, bytes);
            out.push_str(&format!(
                "{path}|{}|{}\n",
                language.grammar_id(),
                parsed.partial
            ));
            out.push_str(&format!("{:?}\n{:?}\n", found.symbols, found.imports));
            for chunk in make_chunks(source, 60) {
                out.push_str(&format!(
                    "{}-{}:{}\n",
                    chunk.start_line, chunk.end_line, chunk.body
                ));
            }
        }
        playbook_common::hash::sha256_hex(out.as_bytes())
    }

    #[test]
    fn extractor_output_matches_the_last_history_row() {
        // Arrange
        let (revision, pinned_digest) = EXTRACTOR_HISTORY.last().expect("history has a row");

        // Assert
        assert_eq!(
            *revision,
            lang::EXTRACTOR_REVISION,
            "EXTRACTOR_REVISION must equal the revision of the last EXTRACTOR_HISTORY row"
        );
        assert_eq!(
            extractor_digest(),
            *pinned_digest,
            "extractor output changed: append a row with a higher revision and the new digest \
             to EXTRACTOR_HISTORY, bump EXTRACTOR_REVISION to match, and keep the old rows"
        );
    }

    #[test]
    fn extractor_history_only_grows_in_revision_order() {
        assert!(EXTRACTOR_HISTORY.windows(2).all(|w| w[0].0 < w[1].0));
    }
}
