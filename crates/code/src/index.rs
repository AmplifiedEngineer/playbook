// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The indexing run. Every run hashes every listed file, so an edit that keeps
//! both size and mtime is still caught. Changed files are read once, prepared
//! on worker threads and written by the calling thread, one `SAVEPOINT` per
//! file inside a transaction per batch. A file is the atomic unit: an error in
//! one file rolls back only that file.

use super::cleanup::{self, Cleanup};
use super::db::{self, OpenMode, ScopeKind, TxHooks};
use super::host;
use super::lang::{self, Lang};
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
    /// The parser key stored with each content row. A manifest row whose
    /// content has another key counts as changed.
    pub parser: String,
    /// The clock of the run in milliseconds since the epoch, for tests. The
    /// run reads the system clock when it is `None`.
    pub now_ms: Option<i64>,
    /// Called once the store is open and before each write transaction. The
    /// `code index` command sets it in debug builds, for the two-runs binary
    /// test.
    pub before_write: Option<Box<dyn Fn() + Send + Sync>>,
    #[cfg(test)]
    pub hooks: Hooks,
    #[cfg(test)]
    pub cleanup_hooks: cleanup::Hooks,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            limits: walker::Limits::default(),
            batch_size: 200,
            max_threads: MAX_WORKERS,
            chunk_lines: 60,
            parser: lang::parser_key(),
            now_ms: None,
            before_write: None,
            #[cfg(test)]
            hooks: Hooks::default(),
            #[cfg(test)]
            cleanup_hooks: cleanup::Hooks::default(),
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
    /// The transaction hooks of the batch writes.
    pub tx: TxHooks,
    /// Makes each batch fail with this SQLite result code right after `BEGIN`.
    pub fail_write: Option<i32>,
    /// Runs after the content hint is read and before the first batch.
    pub after_hint: Option<Box<dyn Fn() + Send + Sync>>,
}

/// How often a batch is tried again after `SQLITE_BUSY`.
const BATCH_RETRIES: usize = 3;

#[cfg(not(test))]
#[cfg(not(test))]
static NO_TX_HOOKS: TxHooks = TxHooks {
    on_write_tx: None,
    fail_begin: None,
    fail_commit: None,
};

#[cfg(not(test))]
static NO_CLEANUP_HOOKS: cleanup::Hooks = cleanup::Hooks {
    fail_cleanup: false,
    after_cleanup_chunk: None,
    before_vacuum: None,
    delete_file: None,
};

impl Options {
    fn before_write(&self) {
        if let Some(hook) = &self.before_write {
            hook();
        }
    }

    fn now(&self) -> i64 {
        self.now_ms.unwrap_or_else(now_ms)
    }

    fn cleanup_hooks(&self) -> &cleanup::Hooks {
        #[cfg(test)]
        return &self.cleanup_hooks;
        #[cfg(not(test))]
        &NO_CLEANUP_HOOKS
    }

    fn tx_hooks(&self) -> &TxHooks {
        #[cfg(test)]
        return &self.hooks.tx;
        #[cfg(not(test))]
        &NO_TX_HOOKS
    }

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

/// How much of the store a run replaced.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Only the rows of the worktree that ran.
    #[default]
    Worktree,
    /// The whole store, because it was damaged and every worktree lost its rows.
    Store,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Worktree => "worktree",
            Scope::Store => "store",
        }
    }
}

/// What one run did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Whether the run replaced one worktree or the whole store.
    pub scope: Scope,
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
    /// Files that took stored content with the same key and were not parsed.
    pub reused: usize,
    /// Whether the cleanup at the end of the run finished.
    pub cleanup: Cleanup,
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
    /// The content row's grammar: the language, or `text` with none.
    grammar: &'static str,
    chunks: Vec<Chunk>,
    lang: Option<&'static str>,
    symbols: Vec<lang::Symbol>,
    imports: Vec<lang::Import>,
    /// The file went through a Tree-sitter parser.
    parsed: bool,
}

/// The content keys of the current parser that the store held at the start of
/// a run, as `(sha256, grammar)`. A hint only: it decides which files to
/// parse, and the batch looks the row up again when it adopts one.
type Hint = HashSet<(String, String)>;

/// A file whose content the store already holds.
struct Adopted {
    path: String,
    sha: String,
    grammar: &'static str,
    lang: Option<&'static str>,
    size: u64,
    mtime_ns: i64,
}

enum Outcome {
    Unchanged,
    Adopt(Adopted),
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
    /// The parser key of the manifest row's content, `None` without content.
    pub(crate) parser: Option<String>,
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

fn prepare(
    root: &Path,
    path: &str,
    existing: Option<&Existing>,
    hint: Option<&Hint>,
    opts: &Options,
) -> Outcome {
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
        if old.sha == sha
            && (old.status == "ok" || old.status == "partial")
            && old.parser.as_deref() == Some(opts.parser.as_str())
        {
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
    let grammar = Lang::for_path(path).map_or(lang::TEXT_GRAMMAR, Lang::grammar_id);
    if hint.is_some_and(|h| h.contains(&(sha.clone(), grammar.to_string()))) {
        return Outcome::Adopt(Adopted {
            path: path.to_string(),
            sha,
            grammar,
            lang: Lang::for_path(path).map(Lang::name),
            size,
            mtime_ns: mtime,
        });
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
        grammar,
        chunks: make_chunks(&text, opts.chunk_lines),
        lang: syntax.lang,
        symbols: syntax.symbols,
        imports: syntax.imports,
        parsed: syntax.parsed,
    }))
}

/// A manifest row to write.
struct ManifestRow<'a> {
    path: &'a str,
    content: Option<i64>,
    lang: Option<&'a str>,
    sha: &'a str,
    size: u64,
    mtime_ns: i64,
    status: &'a str,
    error: Option<&'a str>,
}

/// Stores the content row for `(sha, grammar, parser)` once and returns its
/// id, with whether this call inserted it. The key is the bytes, so a second
/// path with the same bytes reuses the row and its children.
fn ensure_content(conn: &Connection, p: &Prepared, parser: &str) -> rusqlite::Result<(i64, bool)> {
    let inserted = conn.execute(
        "INSERT INTO content(sha256, grammar, parser, partial) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(sha256, grammar, parser) DO NOTHING",
        params![p.sha, p.grammar, parser, p.status == "partial"],
    )? == 1;
    let id = conn.query_row(
        "SELECT id FROM content WHERE sha256 = ?1 AND grammar = ?2 AND parser = ?3",
        params![p.sha, p.grammar, parser],
        |r| r.get(0),
    )?;
    Ok((id, inserted))
}

/// Deletes the symbols, imports, chunks and FTS rows of a content row, so a
/// rebuild can write them again.
fn clear_children(conn: &Connection, content: i64) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM chunks_fts WHERE rowid IN (SELECT id FROM chunks WHERE content = ?1)",
        [content],
    )?;
    for table in ["symbols", "imports", "chunks"] {
        conn.execute(
            &format!("DELETE FROM {table} WHERE content = ?1"),
            [content],
        )?;
    }
    Ok(())
}

/// Points the manifest row of `a.path` at the stored content with its key.
/// Returns false, and writes nothing, when a cleanup removed that content
/// after the hint was read: the file is then parsed.
fn adopt(conn: &Connection, a: &Adopted, parser: &str, wt: i64) -> rusqlite::Result<bool> {
    let found: Option<(i64, bool)> = conn
        .query_row(
            "SELECT id, partial FROM content WHERE sha256 = ?1 AND grammar = ?2 AND parser = ?3",
            params![a.sha, a.grammar, parser],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((content, partial)) = found else {
        return Ok(false);
    };
    upsert_manifest(
        conn,
        wt,
        &ManifestRow {
            path: &a.path,
            content: Some(content),
            lang: a.lang,
            sha: &a.sha,
            size: a.size,
            mtime_ns: a.mtime_ns,
            status: if partial { "partial" } else { "ok" },
            error: None,
        },
    )?;
    Ok(true)
}

/// Deletes a content row that no manifest row references, with its FTS rows
/// first. The children go with it through the foreign keys.
fn drop_unreferenced_content(conn: &Connection, content: i64) -> rusqlite::Result<()> {
    let used: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM manifest WHERE content = ?1)",
        [content],
        |r| r.get(0),
    )?;
    if !used {
        conn.execute(
            "DELETE FROM chunks_fts WHERE rowid IN (SELECT id FROM chunks WHERE content = ?1)",
            [content],
        )?;
        conn.execute("DELETE FROM content WHERE id = ?1", [content])?;
    }
    Ok(())
}

/// Writes one manifest row. When it moves to other content, the same
/// transaction drops the old content row if nothing else uses it.
fn upsert_manifest(conn: &Connection, wt: i64, row: &ManifestRow) -> rusqlite::Result<()> {
    let old: Option<Option<i64>> = conn
        .query_row(
            "SELECT content FROM manifest WHERE worktree = ?1 AND path = ?2",
            params![wt, row.path],
            |r| r.get(0),
        )
        .optional()?;
    conn.execute(
        "INSERT INTO manifest(worktree, path, content, lang, sha256, size, mtime_ns, status, error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(worktree, path) DO UPDATE SET content = excluded.content,
             lang = excluded.lang, sha256 = excluded.sha256, size = excluded.size,
             mtime_ns = excluded.mtime_ns, status = excluded.status, error = excluded.error",
        params![
            wt,
            row.path,
            row.content,
            row.lang,
            row.sha,
            i64::try_from(row.size).unwrap_or(i64::MAX),
            row.mtime_ns,
            row.status,
            row.error
        ],
    )?;
    if let Some(Some(previous)) = old {
        if row.content != Some(previous) {
            drop_unreferenced_content(conn, previous)?;
        }
    }
    Ok(())
}

/// Deletes a manifest row and its content row when nothing else uses it.
fn delete_manifest(conn: &Connection, wt: i64, path: &str) -> rusqlite::Result<()> {
    let content: Option<Option<i64>> = conn
        .query_row(
            "SELECT content FROM manifest WHERE worktree = ?1 AND path = ?2",
            params![wt, path],
            |r| r.get(0),
        )
        .optional()?;
    conn.execute(
        "DELETE FROM manifest WHERE worktree = ?1 AND path = ?2",
        params![wt, path],
    )?;
    if let Some(Some(id)) = content {
        drop_unreferenced_content(conn, id)?;
    }
    Ok(())
}

/// Stores a file's symbols and imports; `parent_id` follows the extractor's parent index.
fn insert_syntax(conn: &Connection, content: i64, p: &Prepared) -> rusqlite::Result<()> {
    let to_i64 = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
    let mut ids: Vec<i64> = Vec::with_capacity(p.symbols.len());
    for sym in &p.symbols {
        let parent_id = sym.parent.and_then(|i| ids.get(i).copied());
        conn.execute(
            "INSERT INTO symbols(content, parent_id, name, qualified_name, kind, signature,
                 signature_cut, exported, start_byte, end_byte, start_line, end_line)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                content,
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
            "INSERT INTO imports(content, module, names, line) VALUES (?1, ?2, ?3, ?4)",
            params![content, import.module, import.names, to_i64(import.line)],
        )?;
    }
    Ok(())
}

fn apply(
    conn: &Connection,
    outcome: &Outcome,
    opts: &Options,
    wt: i64,
    replace: bool,
) -> Result<(), String> {
    let sql = |e: rusqlite::Error| e.to_string();
    match outcome {
        Outcome::Write(p) => {
            opts.before_file(&p.path)?;
            let (content, inserted) = ensure_content(conn, p, &opts.parser).map_err(sql)?;
            if replace && !inserted {
                clear_children(conn, content).map_err(sql)?;
            }
            if inserted || replace {
                insert_syntax(conn, content, p).map_err(sql)?;
                for chunk in &p.chunks {
                    conn.execute(
                        "INSERT INTO chunks(content, start_line, end_line) VALUES (?1, ?2, ?3)",
                        params![content, chunk.start_line as i64, chunk.end_line as i64],
                    )
                    .map_err(sql)?;
                    conn.execute(
                        "INSERT INTO chunks_fts(rowid, body) VALUES (?1, ?2)",
                        params![conn.last_insert_rowid(), chunk.body],
                    )
                    .map_err(sql)?;
                }
            }
            upsert_manifest(
                conn,
                wt,
                &ManifestRow {
                    path: &p.path,
                    content: Some(content),
                    lang: p.lang,
                    sha: &p.sha,
                    size: p.size,
                    mtime_ns: p.mtime_ns,
                    status: p.status,
                    error: None,
                },
            )
            .map_err(sql)?;
        }
        Outcome::Skip(s) => {
            opts.before_file(&s.path)?;
            upsert_manifest(
                conn,
                wt,
                &ManifestRow {
                    path: &s.path,
                    content: None,
                    lang: None,
                    sha: "",
                    size: s.size,
                    mtime_ns: s.mtime_ns,
                    status: "skipped",
                    error: Some(s.reason.as_str()),
                },
            )
            .map_err(sql)?;
        }
        Outcome::Touch {
            path,
            size,
            mtime_ns,
        } => {
            conn.execute(
                "UPDATE manifest SET size = ?3, mtime_ns = ?4 WHERE worktree = ?1 AND path = ?2",
                params![wt, path, i64::try_from(*size).unwrap_or(i64::MAX), mtime_ns],
            )
            .map_err(sql)?;
        }
        Outcome::Unchanged | Outcome::Adopt(_) | Outcome::Missing(_) | Outcome::Failed { .. } => {}
    }
    Ok(())
}

fn mark_failed(conn: &Connection, wt: i64, path: &str, error: &str) -> rusqlite::Result<()> {
    // A failed path that had content keeps it, as it keeps its children.
    let changed = conn.execute(
        "UPDATE manifest SET status = 'failed', error = ?3 WHERE worktree = ?1 AND path = ?2",
        params![wt, path, error],
    )?;
    if changed == 0 {
        conn.execute(
            "INSERT INTO manifest(worktree, path, content, sha256, size, mtime_ns, status, error)
             VALUES (?1, ?2, NULL, '', 0, 0, 'failed', ?3)",
            params![wt, path, error],
        )?;
    }
    Ok(())
}

/// Writes one batch in a single transaction and returns the paths of adopted
/// files whose content vanished, which the run parses in a later batch. A busy store makes the whole
/// batch start again, up to [`BATCH_RETRIES`] times, and the report counts
/// only the attempt that committed.
fn write_batch(
    conn: &mut Connection,
    batch: &[Outcome],
    opts: &Options,
    wt: i64,
    replace: bool,
    report: &mut Report,
) -> Result<Vec<String>, String> {
    opts.before_write();
    for attempt in 0..=BATCH_RETRIES {
        let mut staged = Report::default();
        match write_batch_once(conn, batch, opts, wt, replace, &mut staged) {
            Ok(redo) => {
                report.changed += staged.changed;
                report.parsed += staged.parsed;
                report.failed += staged.failed;
                report.reused += staged.reused;
                return Ok(redo);
            }
            Err(e) if is_busy(&e) && attempt < BATCH_RETRIES => {}
            Err(e) => return Err(db::map_sql_error("failed to write the index", &e)),
        }
    }
    unreachable!("the last attempt returns")
}

fn is_busy(err: &rusqlite::Error) -> bool {
    err.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy)
}

fn write_batch_once(
    conn: &mut Connection,
    batch: &[Outcome],
    opts: &Options,
    wt: i64,
    replace: bool,
    report: &mut Report,
) -> rusqlite::Result<Vec<String>> {
    let hooks = opts.tx_hooks();
    let mut tx = db::write_tx(conn, hooks)?;
    #[cfg(test)]
    if let Some(code) = opts.hooks.fail_write {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(code),
            None,
        ));
    }
    let mut failures: Vec<(String, String)> = Vec::new();
    let mut redo: Vec<String> = Vec::new();
    for outcome in batch {
        if let Outcome::Adopt(a) = outcome {
            let sp = tx.savepoint()?;
            if adopt(&sp, a, &opts.parser, wt)? {
                sp.commit()?;
                report.changed += 1;
                report.reused += 1;
            } else {
                drop(sp);
                redo.push(a.path.clone());
            }
            continue;
        }
        let path = match outcome {
            Outcome::Write(p) => p.path.clone(),
            Outcome::Skip(s) => s.path.clone(),
            Outcome::Touch { path, .. } => path.clone(),
            Outcome::Failed { path, error } => {
                failures.push((path.clone(), error.clone()));
                continue;
            }
            Outcome::Unchanged | Outcome::Adopt(_) | Outcome::Missing(_) => continue,
        };
        let sp = tx.savepoint()?;
        match apply(&sp, outcome, opts, wt, replace) {
            Ok(()) => {
                sp.commit()?;
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
        mark_failed(&tx, wt, &path, &error)?;
        report.failed += 1;
    }
    db::commit_tx(tx, hooks)?;
    Ok(redo)
}

/// The content keys of `parser` that the store holds.
fn load_hint(conn: &Connection, parser: &str) -> Result<Hint, String> {
    let fail = |e: rusqlite::Error| db::map_sql_error("failed to read the index", &e);
    let mut stmt = conn
        .prepare("SELECT sha256, grammar FROM content WHERE parser = ?1")
        .map_err(fail)?;
    let rows = stmt
        .query_map([parser], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(fail)?;
    rows.collect::<Result<Hint, _>>().map_err(fail)
}

pub(crate) fn load_existing(conn: &Connection) -> Result<HashMap<String, Existing>, String> {
    let mut stmt = conn
        .prepare("SELECT path, sha256, status, error, size, mtime_ns, parser FROM files")
        .map_err(|e| db::map_sql_error("failed to read the index", &e))?;
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
                    parser: r.get(6)?,
                },
            ))
        })
        .map_err(|e| db::map_sql_error("failed to read the index", &e))?;
    rows.collect::<Result<HashMap<_, _>, _>>()
        .map_err(|e| db::map_sql_error("failed to read the index", &e))
}

/// Adds this worktree's row, or refreshes it, and returns its id. The id of an
/// existing row stays, so its manifest rows stay attached.
fn register_worktree(
    conn: &mut Connection,
    root: &Path,
    worktree: &str,
    started: i64,
) -> Result<i64, String> {
    let key = worktree;
    let hooks = TxHooks::default();
    let tx = db::write_tx(conn, &hooks).map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO worktrees(key, root, created_at, tool_version, last_run_started)
         VALUES (?1, ?2, ?3, ?4, ?3)
         ON CONFLICT(key) DO UPDATE SET root = excluded.root,
             tool_version = excluded.tool_version,
             last_run_started = excluded.last_run_started",
        params![
            key,
            root.display().to_string(),
            started,
            host::tool_version()
        ],
    )
    .map_err(|e| e.to_string())?;
    let id = tx
        .query_row("SELECT id FROM worktrees WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    db::commit_tx(tx, &hooks).map_err(|e| e.to_string())?;
    Ok(id)
}

/// Drops this worktree's row and, with it, its manifest, so the run indexes
/// every file again. Content that no other worktree lists stays in the store.
fn forget_worktree(conn: &mut Connection, worktree: &str) -> Result<(), String> {
    let hooks = TxHooks::default();
    let tx = db::write_tx(conn, &hooks).map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM worktrees WHERE key = ?1", [worktree])
        .map_err(|e| e.to_string())?;
    db::commit_tx(tx, &hooks).map_err(|e| e.to_string())
}

/// The run lock of one worktree, beside the store. Two worktrees index at the
/// same time; one worktree runs once. Held for the whole run and released by
/// the OS if the process dies.
pub fn lock_path(store: &Path, worktree: &str) -> PathBuf {
    store.with_file_name(format!("{worktree}.lock"))
}

/// The run lock of an index at the state folder fallback path, which has no
/// worktree key in its name and serves one worktree. It is not `index.lock`,
/// because the store lock of that path is.
pub fn fallback_lock_path(index: &Path) -> PathBuf {
    index.with_file_name("index.run.lock")
}

/// The run lock file for `index`: the fallback lock at the fallback path, the
/// worktree lock beside a shared store.
pub fn run_lock_path(index: &Path, worktree: &str) -> PathBuf {
    if index.file_name().is_some_and(|n| n == host::FALLBACK_FILE) {
        fallback_lock_path(index)
    } else {
        lock_path(index, worktree)
    }
}

/// The held run lock. Dropping it unlocks explicitly, so a copy of the file
/// descriptor that a concurrent fork briefly holds cannot keep the lock alive.
pub struct RunLock(File);

impl Drop for RunLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Takes the exclusive run lock of `worktree`, or says another run holds it.
pub fn acquire_lock(index: &Path, worktree: &str) -> Result<RunLock, String> {
    if let Some(parent) = index.parent() {
        host::ensure_private_dir(parent)
            .map_err(|e| format!("failed to create directory {}: {e}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(run_lock_path(index, worktree))
        .map_err(|e| format!("failed to open the index lock: {e}"))?;
    match file.try_lock() {
        Ok(()) => Ok(RunLock(file)),
        Err(TryLockError::WouldBlock) => Err("another index run is in progress".to_string()),
        Err(TryLockError::Error(e)) => Err(format!("failed to lock the index: {e}")),
    }
}

/// Runs one indexing pass over the repo at `root`, storing into `index`.
pub fn run(
    root: &Path,
    index: &Path,
    worktree: &str,
    rebuild: bool,
    opts: &Options,
) -> Result<Report, String> {
    let _lock = acquire_lock(index, worktree)?;
    let mut conn = db::open_index(index, OpenMode::Index).map_err(|e| e.to_string())?;
    opts.before_write();
    let mut scope = if conn.recovered() {
        Scope::Store
    } else {
        Scope::Worktree
    };
    if rebuild {
        if db::is_healthy(&conn) {
            forget_worktree(&mut conn, worktree)?;
        } else {
            drop(conn);
            conn = db::rebuild_store(index).map_err(|e| e.to_string())?;
            scope = Scope::Store;
        }
    }
    let started = opts.now();
    let wt = register_worktree(&mut conn, root, worktree, started)?;
    db::create_scope_views(&conn, wt, ScopeKind::Writer).map_err(|e| e.to_string())?;

    let found = walker::discover(root, &opts.limits)?;
    let existing = load_existing(&conn)?;
    let hint = if rebuild {
        None
    } else {
        Some(load_hint(&conn, &opts.parser)?)
    };
    #[cfg(test)]
    if let Some(after_hint) = &opts.hooks.after_hint {
        after_hint();
    }
    let mut report = Report {
        skipped: found.skipped.len(),
        scope,
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
    let mut redo: Vec<String> = Vec::new();

    std::thread::scope(|scope| {
        for _ in 0..workers {
            let tx = tx.clone();
            let (files, existing, next, hint) = (&found.files, &existing, &next, &hint);
            scope.spawn(move || loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(path) = files.get(i) else { break };
                if tx
                    .send(prepare(root, path, existing.get(path), hint.as_ref(), opts))
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
            redo.extend(write_batch(&mut conn, batch, opts, wt, rebuild, report)?);
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
    for chunk in redo.chunks(opts.batch_size.max(1)) {
        let mut writes: Vec<Outcome> = Vec::new();
        for path in chunk {
            match prepare(root, path, None, None, opts) {
                Outcome::Missing(path) => {
                    listed.remove(&path);
                }
                other => writes.push(other),
            }
        }
        write_batch(&mut conn, &writes, opts, wt, rebuild, &mut report)?;
    }

    report.removed = remove_unlisted(&mut conn, wt, &listed)?;
    report.indexed = conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE status IN ('ok', 'partial')",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map_err(|e| e.to_string())
        .map(|n| usize::try_from(n).unwrap_or(0))?;
    let completed = opts.now().max(started);
    conn.execute(
        "UPDATE worktrees SET last_run_completed = ?2 WHERE id = ?1",
        params![wt, completed],
    )
    .map_err(|e| e.to_string())?;
    let env = cleanup::Env {
        now_ms: completed,
        tx: opts.tx_hooks(),
        hooks: opts.cleanup_hooks(),
    };
    report.cleanup = cleanup::collect_garbage(&mut conn, index, &env);
    Ok(report)
}

/// Drops rows (FTS rows first) for files that are no longer listed.
fn remove_unlisted(
    conn: &mut Connection,
    wt: i64,
    listed: &HashSet<String>,
) -> Result<usize, String> {
    let paths: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT path FROM manifest WHERE worktree = ?1")
            .map_err(|e| e.to_string())?;
        let mapped = stmt
            .query_map([wt], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        mapped
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?
    };
    let gone: Vec<String> = paths
        .into_iter()
        .filter(|path| !listed.contains(path))
        .collect();
    let hooks = TxHooks::default();
    let tx = db::write_tx(conn, &hooks).map_err(|e| e.to_string())?;
    for path in &gone {
        delete_manifest(&tx, wt, path).map_err(|e| e.to_string())?;
    }
    db::commit_tx(tx, &hooks).map_err(|e| e.to_string())?;
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
        let writer = db::open_index(&f.index, OpenMode::Index).expect("writer");
        let stored = count(&writer, "SELECT COUNT(*) FROM main.chunks");
        let stay_chunks = count(&conn, "SELECT COUNT(*) FROM chunks");
        assert!(stay_chunks > 0);
        assert_eq!(stored, stay_chunks);
        assert_eq!(
            count(
                &writer,
                "SELECT COUNT(*) FROM chunks_fts WHERE chunks_fts MATCH 'vanishingtext'"
            ),
            0
        );
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
                ..Default::default()
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
        let worktree = f.worktree.clone();
        let writer = std::thread::spawn(move || run(&root, &index, &worktree, false, &opts));
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
        let held = acquire_lock(&f.index, &f.worktree).expect("first lock");

        // Act
        let second = run(&f.root, &f.index, &f.worktree, false, &Options::default());

        // Assert
        assert_eq!(
            second.expect_err("must be refused"),
            "another index run is in progress"
        );
        drop(held);
        assert!(run(&f.root, &f.index, &f.worktree, false, &Options::default()).is_ok());
    }

    #[test]
    fn the_lock_is_released_while_a_copy_of_the_descriptor_is_still_open() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-lock-dup");
        write(&f.root, "a.rs", b"x\n");
        let held = acquire_lock(&f.index, &f.worktree).expect("first lock");
        let copy = held.0.try_clone().expect("clone");

        // Act
        drop(held);

        // Assert
        assert!(acquire_lock(&f.index, &f.worktree).is_ok());
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
                ..Default::default()
            },
            ..Options::default()
        };

        // Act
        let stopped = run(&f.root, &f.index, &f.worktree, false, &opts);

        // Assert
        assert_eq!(stopped.expect_err("stopped"), "stop");
        let conn = f.conn();
        let times = |conn: &Connection| -> (Option<i64>, Option<i64>) {
            conn.query_row(
                "SELECT last_run_started, last_run_completed FROM worktrees",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("worktree row")
        };
        let (started, completed) = times(&conn);
        assert!(started.is_some());
        assert!(completed.is_none());
        drop(conn);
        f.run();
        let (started, done) = times(&f.conn());
        let (started, done) = (started.expect("started"), done.expect("completed"));
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
                ..Default::default()
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
        let report =
            run(&f.root, &f.index, &f.worktree, true, &Options::default()).expect("rebuild");

        // Assert
        assert_eq!((report.changed, report.unchanged), (1, 0));
        assert_eq!(report.indexed, 1);
    }

    /// Append only. One row per extractor revision, with the digest of what the
    /// extractors and the chunker produce for `DIGEST_FIXTURES`. Never edit or
    /// delete an old row.
    const EXTRACTOR_HISTORY: &[(u32, &str)] = &[(
        1,
        "41a8b01d96802962c92d2d199283a2682b1c2dc1729b1472d5274f3c930bc22f",
    )];

    /// One source per grammar, with enough syntax to touch symbols, imports,
    /// nesting and the identifier pieces of the full text body.
    const DIGEST_FIXTURES: &[(&str, &str)] = &[
        (
            "a.rs",
            "use std::fmt::Display;\n/// Doc.\npub struct HttpClient { pub retry_count: u32 }\nimpl HttpClient {\n    pub fn send_request(&self) -> u32 { 1 }\n}\nfn helper() {}\npub enum Mode { Fast, Slow }\npub trait Runner { fn run_all(&self); }\nmod inner { pub const MAX_RETRIES: u32 = 3; }\nstatic GLOBAL_NAME: &str = \"x\";\nmacro_rules! make_it { () => {}; }\n",
        ),
        (
            "broken.rs",
            "pub fn good_one() {}\nimpl Broken {\n    fn half_done( {\n",
        ),
        (
            "a.py",
            "import os\nfrom typing import List\nfrom __future__ import annotations\n\nclass UserStore:\n    def load_user(self, user_id):\n        return user_id\n\n@decorator\ndef main():\n    pass\n\nMAX_USERS = 10\n",
        ),
        (
            "a.js",
            "import { readFile } from 'fs';\nexport function loadConfig(path) { return readFile(path); }\nconst maxRetries = 3;\n",
        ),
        (
            "a.ts",
            "import type { Config } from './config';\nexport interface RequestOptions { retryCount: number }\nexport class ApiClient { sendRequest(o: RequestOptions): void {} }\nexport enum Level { Low, High }\nexport type Handler = (e: string) => void;\nexport abstract class Base { abstract go(): void; }\n",
        ),
        (
            "a.tsx",
            "import React from 'react';\nexport function UserCard(props: { userName: string }) { return <div>{props.userName}</div>; }\n",
        ),
        (
            "a.go",
            "package main\n\nimport (\n\t\"fmt\"\n\t\"os\"\n)\n\ntype HttpServer struct{ Port int }\n\ntype Alias = HttpServer\n\nconst MaxConns = 5\n\nvar defaultName = \"x\"\n\nfunc (s *HttpServer) StartServer() { fmt.Println(s.Port, os.Args) }\n\nfunc main() {}\n",
        ),
        (
            "long.py",
            LONG_FIXTURE,
        ),
    ];

    /// 74 lines, so the chunker makes more than one window.
    const LONG_FIXTURE: &str = concat!(
        "def first_function():\n    pass\n",
        "# line\n# line\n# line\n# line\n# line\n# line\n# line\n# line\n# line\n# line\n",
        "# line\n# line\n# line\n# line\n# line\n# line\n# line\n# line\n# line\n# line\n",
        "# line\n# line\n# line\n# line\n# line\n# line\n# line\n# line\n# line\n# line\n",
        "# line\n# line\n# line\n# line\n# line\n# line\n# line\n# line\n# line\n# line\n",
        "# line\n# line\n# line\n# line\n# line\n# line\n# line\n# line\n# line\n# line\n",
        "# line\n# line\n# line\n# line\n# line\n# line\n# line\n# line\n# line\n# line\n",
        "# line\n# line\n# line\n# line\n# line\n# line\n# line\n# line\n# line\n# line\n",
        "def second_function():\n    pass\n",
    );

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
            for chunk in make_chunks(source, Options::default().chunk_lines) {
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
    fn the_digest_fixtures_cover_a_broken_file_and_a_second_chunk_window() {
        // Arrange
        let (path, source) = DIGEST_FIXTURES
            .iter()
            .find(|(p, _)| *p == "broken.rs")
            .expect("broken fixture");
        let parsed = lang::parse(lang::Lang::for_path(path).unwrap(), source.as_bytes()).unwrap();

        // Assert
        assert!(parsed.partial);
        assert!(make_chunks(LONG_FIXTURE, Options::default().chunk_lines).len() > 1);
    }

    #[test]
    fn extractor_history_only_grows_in_revision_order() {
        assert!(EXTRACTOR_HISTORY.windows(2).all(|w| w[0].0 < w[1].0));
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).expect("count")
    }

    #[test]
    fn two_paths_with_the_same_bytes_share_one_content_row() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-share-bytes");
        write(&f.root, "a.rs", b"fn same() {}\n");
        write(&f.root, "b.rs", b"fn same() {}\n");

        // Act
        f.run();

        // Assert
        let conn = f.conn();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM content"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM manifest"), 2);
        assert_eq!(
            count(&conn, "SELECT COUNT(DISTINCT content) FROM manifest"),
            1
        );
    }

    #[test]
    fn a_syntax_error_makes_partial_content_and_a_partial_manifest_row() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-partial-content");
        write(&f.root, "a.rs", b"fn good() {}\nfn broken( {\n");

        // Act
        f.run();

        // Assert
        let conn = f.conn();
        assert_eq!(count(&conn, "SELECT partial FROM content"), 1);
        let status: String = conn
            .query_row("SELECT status FROM manifest", [], |r| r.get(0))
            .expect("status");
        assert_eq!(status, "partial");
    }

    #[test]
    fn a_repo_with_nothing_to_index_has_no_content_row() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-no-content");
        write(&f.root, "bin.dat", b"a\0b");

        // Act
        let report = f.run();

        // Assert
        assert_eq!(report.indexed, 0);
        let conn = f.conn();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM content"), 0);
    }

    #[test]
    fn typescript_and_tsx_with_the_same_bytes_get_two_content_rows() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-tsx-apart");
        write(&f.root, "a.ts", b"export const x = 1;\n");
        write(&f.root, "a.tsx", b"export const x = 1;\n");

        // Act
        f.run();

        // Assert
        let conn = f.conn();
        let grammars: Vec<String> = conn
            .prepare("SELECT grammar FROM content ORDER BY grammar")
            .expect("prepare")
            .query_map([], |r| r.get(0))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect();
        assert_eq!(grammars, vec!["tsx", "typescript"]);
        let langs: Vec<String> = conn
            .prepare("SELECT lang FROM manifest ORDER BY path")
            .expect("prepare")
            .query_map([], |r| r.get(0))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect();
        assert_eq!(langs, vec!["typescript", "typescript"]);
    }

    #[test]
    fn a_file_with_no_grammar_takes_the_text_grammar_and_still_gets_chunks() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-text-grammar");
        write(&f.root, "notes.txt", b"plain words here\n");

        // Act
        f.run();

        // Assert
        let conn = f.conn();
        let grammar: String = conn
            .query_row("SELECT grammar FROM content", [], |r| r.get(0))
            .expect("grammar");
        assert_eq!(grammar, lang::TEXT_GRAMMAR);
        assert_eq!(fts_paths(&conn, "plain"), vec!["notes.txt"]);
    }

    #[test]
    fn a_skipped_and_a_failed_new_path_have_a_manifest_row_without_content() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-no-content-rows");
        write(&f.root, "bin.dat", b"a\0b");
        write(&f.root, "bad.rs", b"fn bad() {}\n");
        let opts = Options {
            hooks: Hooks {
                after_batch_commit: None,
                before_file_insert: Some(Box::new(|path| {
                    if path == "bad.rs" {
                        Err("injected".to_string())
                    } else {
                        Ok(())
                    }
                })),
                ..Default::default()
            },
            ..Options::default()
        };

        // Act
        f.run_with(&opts);

        // Assert
        let conn = f.conn();
        let rows: Vec<(String, String, Option<i64>)> = conn
            .prepare("SELECT path, status, content FROM manifest ORDER BY path")
            .expect("prepare")
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("bad.rs".to_string(), "failed".to_string(), None),
                ("bin.dat".to_string(), "skipped".to_string(), None),
            ]
        );
    }

    #[test]
    fn an_edit_points_the_manifest_at_new_content_and_drops_the_old_row() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-repoint");
        write(&f.root, "a.rs", b"fn one() {}\n");
        f.run();
        let first = count(&f.conn(), "SELECT id FROM content");

        // Act
        write(&f.root, "a.rs", b"fn two() {}\n");
        f.set_mtime("a.rs", 2_000_000);
        f.run();

        // Assert
        let conn = f.conn();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM content"), 1);
        assert!(count(&conn, "SELECT id FROM content") > first);
        let linked = count(
            &conn,
            "SELECT COUNT(*) FROM manifest m JOIN content c ON c.id = m.content",
        );
        assert_eq!(linked, 1);
    }

    #[test]
    fn a_removed_path_drops_its_manifest_row_and_unused_content() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-drop-content");
        write(&f.root, "gone.rs", b"fn gone() {}\n");
        write(&f.root, "stay.rs", b"fn stay() {}\n");
        f.run();
        std::fs::remove_file(f.root.join("gone.rs")).expect("remove");

        // Act
        let report = f.run();

        // Assert
        assert_eq!(report.removed, 1);
        let conn = f.conn();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM manifest"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM content"), 1);
        let writer = db::open_index(&f.index, OpenMode::Index).expect("writer");
        assert_eq!(
            count(
                &writer,
                "SELECT COUNT(*) FROM chunks_fts WHERE chunks_fts MATCH 'gone'"
            ),
            0
        );
    }

    #[test]
    fn content_of_another_parser_counts_as_changed() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-parser-key");
        write(&f.root, "a.rs", b"fn a() {}\n");
        let old = Options {
            parser: "old-parser".to_string(),
            ..Options::default()
        };
        f.run_with(&old);

        // Act
        let same = f.run_with(&old);
        let other = f.run();

        // Assert
        assert_eq!((same.changed, same.unchanged), (0, 1));
        assert_eq!((other.changed, other.unchanged), (1, 0));
        let conn = f.conn();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM content"), 1);
    }

    #[test]
    fn a_run_records_its_worktree_row_once() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-worktree-row");
        write(&f.root, "a.rs", b"fn a() {}\n");

        // Act
        f.run();
        f.run();

        // Assert
        let conn = f.conn();
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM worktrees"), 1);
        let (key, started, completed): (String, i64, i64) = conn
            .query_row(
                "SELECT key, last_run_started, last_run_completed FROM worktrees",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("row");
        assert_eq!(key, f.worktree);
        assert!(completed >= started);
    }

    #[test]
    fn no_other_module_begins_a_transaction_by_hand() {
        let sources = [
            include_str!("index.rs"),
            include_str!("lib.rs"),
            include_str!("search.rs"),
            include_str!("ranked.rs"),
            include_str!("refs.rs"),
            include_str!("context.rs"),
            include_str!("status.rs"),
        ];
        for source in sources {
            let production = source
                .split("\n#[cfg(test)]\nmod tests")
                .next()
                .expect("production part");
            for banned in [
                ".transaction(",
                "unchecked_transaction",
                "transaction_with_behavior",
            ] {
                assert!(!production.contains(banned), "found {banned}");
            }
        }
    }

    #[test]
    fn a_failed_edit_keeps_the_manifest_content_and_marks_the_row_failed() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-failed-keeps-content");
        write(&f.root, "a.rs", b"fn keepme() {}\n");
        f.run();
        let before = count(&f.conn(), "SELECT content FROM manifest");
        write(&f.root, "a.rs", b"fn keepme2() {}\n");
        let opts = Options {
            hooks: Hooks {
                after_batch_commit: None,
                before_file_insert: Some(Box::new(|_| Err("injected".to_string()))),
                ..Default::default()
            },
            ..Options::default()
        };

        // Act
        let report = f.run_with(&opts);

        // Assert
        assert_eq!(report.failed, 1);
        let conn = f.conn();
        let (status, content): (String, i64) = conn
            .query_row("SELECT status, content FROM manifest", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .expect("row");
        assert_eq!((status.as_str(), content), ("failed", before));
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM content"), 1);
    }

    fn temp_views(conn: &Connection) -> Vec<String> {
        conn.prepare("SELECT name FROM sqlite_temp_master WHERE type = 'view' ORDER BY name")
            .expect("prepare")
            .query_map([], |r| r.get(0))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect()
    }

    #[test]
    fn a_query_connection_reads_four_views_and_a_writer_reads_only_files() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-views");
        write(&f.root, "a.rs", b"fn one() {}\n");
        f.run();

        // Act
        let reader = f.conn();
        let writer = db::open_index(&f.index, OpenMode::Index).expect("writer");
        let id: i64 = writer
            .query_row("SELECT id FROM worktrees", [], |r| r.get(0))
            .expect("row");
        db::create_scope_views(&writer, id, ScopeKind::Writer).expect("views");

        // Assert
        assert_eq!(
            temp_views(&reader),
            vec!["chunks", "files", "imports", "symbols"]
        );
        assert_eq!(temp_views(&writer), vec!["files"]);
    }

    #[test]
    fn two_worktrees_in_one_store_see_only_their_own_paths() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-two-worktrees");
        write(&f.root, "a.rs", b"fn shared_name() {}\n");
        write(&f.root, "only_a.rs", b"fn only_in_a() {}\n");
        let other = crate::walker::test_repo::init("idx-two-worktrees-b");
        write(&other, "b.rs", b"fn shared_name() {}\n");
        f.run();
        run(&other, &f.index, "other", false, &Options::default()).expect("second worktree");

        // Act
        let a = f.conn();
        let b = db::open_query(&f.index, "other").expect("other");

        // Assert
        assert_eq!(paths(&a), vec!["a.rs", "only_a.rs"]);
        assert_eq!(paths(&b), vec!["b.rs"]);
        assert_eq!(symbol_names(&a, "a.rs"), vec!["shared_name"]);
        assert_eq!(count(&b, "SELECT COUNT(*) FROM symbols"), 1);
        assert_eq!(fts_paths(&b, "only_in_a"), Vec::<String>::new());
        assert_eq!(count(&f.conn(), "SELECT COUNT(*) FROM content"), 2);
    }

    #[test]
    fn removing_a_path_keeps_content_another_worktree_still_uses() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-shared-content");
        write(&f.root, "a.rs", b"fn shared_name() {}\n");
        let other = crate::walker::test_repo::init("idx-shared-content-b");
        write(&other, "b.rs", b"fn shared_name() {}\n");
        f.run();
        run(&other, &f.index, "other", false, &Options::default()).expect("second worktree");
        std::fs::remove_file(f.root.join("a.rs")).expect("remove");

        // Act
        f.run();

        // Assert
        let b = db::open_query(&f.index, "other").expect("other");
        assert_eq!(symbol_names(&b, "b.rs"), vec!["shared_name"]);
        assert_eq!(count(&b, "SELECT COUNT(*) FROM chunks"), 1);
    }

    #[test]
    fn a_worktree_with_no_row_reads_as_missing() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-no-row");
        write(&f.root, "a.rs", b"fn a() {}\n");
        f.run();

        // Act
        let got = db::open_query(&f.index, "never-indexed");

        // Assert
        assert_eq!(got.err(), Some(db::DbError::Missing));
    }

    #[test]
    fn two_paths_with_the_same_bytes_return_the_declaration_once_per_path() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-same-bytes-view");
        write(&f.root, "a.rs", b"fn twin() {}\n");
        write(&f.root, "b.rs", b"fn twin() {}\n");
        f.run();

        // Act
        let conn = f.conn();
        let hits = count(
            &conn,
            "SELECT COUNT(*) FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE s.name = 'twin'",
        );

        // Assert
        assert_eq!(hits, 2);
        assert_eq!(
            count(&conn, "SELECT COUNT(DISTINCT id) FROM files"),
            1,
            "both paths share the content id"
        );
    }

    #[test]
    fn a_path_without_content_shows_in_files_with_a_null_content() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-null-content-view");
        write(&f.root, "bin.dat", b"a\0b");
        write(&f.root, "ok.rs", b"fn ok() {}\n");

        // Act
        f.run();

        // Assert
        let conn = f.conn();
        let (id, content): (Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT id, content FROM files WHERE path = 'bin.dat'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("row");
        assert_eq!((id, content), (None, None));
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files"), 2);
    }

    #[test]
    fn the_hot_queries_use_an_index_search_through_the_views() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-plans");
        for i in 0..5 {
            write(
                &f.root,
                &format!("f{i}.rs"),
                format!("fn name{i}() {{}}\n").as_bytes(),
            );
        }
        f.run();
        let conn = f.conn();
        let queries = [
            "SELECT f.path FROM symbols s JOIN files f ON f.id = s.file_id WHERE s.name = 'name1'",
            "SELECT f.path FROM chunks_fts JOIN chunks c ON c.id = chunks_fts.rowid
             JOIN files f ON f.id = c.file_id WHERE chunks_fts MATCH 'name1'",
            "SELECT f.path FROM imports i JOIN files f ON f.id = i.file_id WHERE i.module = 'm'",
        ];

        // Act and assert, one row per query
        for sql in queries {
            let plan: Vec<String> = conn
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .expect("prepare")
                .query_map([], |r| r.get::<_, String>(3))
                .expect("plan")
                .map(|r| r.expect("row"))
                .collect();
            let text = plan.join(" | ");
            assert!(
                text.contains("SEARCH") || text.contains("VIRTUAL TABLE"),
                "{text}"
            );
            for table in ["main.symbols", "main.chunks", "main.imports"] {
                let scan = format!("SCAN {table}");
                assert!(!plan.iter().any(|row| row.starts_with(&scan)), "{text}");
            }
        }
    }

    #[test]
    fn the_query_modules_never_name_the_manifest() {
        let sources = [
            include_str!("lib.rs"),
            include_str!("search.rs"),
            include_str!("ranked.rs"),
            include_str!("refs.rs"),
            include_str!("context.rs"),
            include_str!("status.rs"),
        ];
        for source in sources {
            let production = source
                .split("\n#[cfg(test)]\nmod tests")
                .next()
                .expect("production part");
            assert!(!production.contains("manifest"));
        }
    }

    #[test]
    fn a_held_lock_blocks_only_its_own_worktree() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-lock-per-worktree");
        write(&f.root, "a.rs", b"fn a() {}\n");
        let other = crate::walker::test_repo::init("idx-lock-per-worktree-b");
        write(&other, "b.rs", b"fn b() {}\n");
        let _held = acquire_lock(&f.index, &f.worktree).expect("lock of the first worktree");

        // Act
        let same = run(&f.root, &f.index, &f.worktree, false, &Options::default());
        let sibling = run(&other, &f.index, "other", false, &Options::default());

        // Assert
        assert_eq!(
            same.expect_err("must be refused"),
            "another index run is in progress"
        );
        assert!(sibling.is_ok(), "{sibling:?}");
    }

    #[test]
    fn the_fallback_path_takes_one_lock_named_for_the_index() {
        // Arrange
        let fallback = Path::new("/state/code").join(host::FALLBACK_FILE);
        let store = Path::new("/data/code/o/r/store-v4.sqlite3");

        // Act and assert
        assert_eq!(
            run_lock_path(&fallback, "wt"),
            Path::new("/state/code/index.run.lock")
        );
        assert_eq!(
            fallback_lock_path(&fallback),
            run_lock_path(&fallback, "wt")
        );
        assert_eq!(
            run_lock_path(store, "wt"),
            Path::new("/data/code/o/r/wt.lock")
        );
    }

    #[test]
    fn a_run_at_the_fallback_path_is_refused_while_its_lock_is_held() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-fallback-lock");
        write(&f.root, "a.rs", b"x\n");
        let index = f.index.with_file_name(host::FALLBACK_FILE);
        let held = acquire_lock(&index, &f.worktree).expect("fallback lock");

        // Act
        let got = run(&f.root, &index, &f.worktree, false, &Options::default());

        // Assert
        assert_eq!(
            got.expect_err("must be refused"),
            "another index run is in progress"
        );
        drop(held);
    }

    #[test]
    fn a_rebuild_of_a_healthy_store_replaces_only_this_worktree() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-rebuild-scope");
        write(&f.root, "a.rs", b"fn a() {}\n");
        let other = crate::walker::test_repo::init("idx-rebuild-scope-b");
        write(&other, "b.rs", b"fn b() {}\n");
        f.run();
        run(&other, &f.index, "other", false, &Options::default()).expect("second worktree");
        let before = paths(&db::open_query(&f.index, "other").expect("other"));

        // Act
        let report =
            run(&f.root, &f.index, &f.worktree, true, &Options::default()).expect("rebuild");

        // Assert
        assert_eq!(report.scope, Scope::Worktree);
        assert_eq!(report.parsed, 1);
        assert_eq!(report.unchanged, 0);
        assert_eq!(
            paths(&db::open_query(&f.index, "other").expect("other")),
            before
        );
        assert_eq!(paths(&f.conn()), vec!["a.rs"]);
    }

    #[test]
    fn a_rebuild_of_a_damaged_store_replaces_the_whole_store() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-rebuild-store");
        write(&f.root, "a.rs", b"fn a() {}\n");
        let other = crate::walker::test_repo::init("idx-rebuild-store-b");
        write(&other, "b.rs", b"fn b() {}\n");
        f.run();
        run(&other, &f.index, "other", false, &Options::default()).expect("second worktree");
        std::fs::write(&f.index, vec![0x5Au8; 8192]).expect("damage");
        let _ = std::fs::remove_file(f.index.with_extension("sqlite3-wal"));

        // Act
        let report =
            run(&f.root, &f.index, &f.worktree, true, &Options::default()).expect("rebuild");

        // Assert
        assert_eq!(report.scope, Scope::Store);
        assert_eq!(paths(&f.conn()), vec!["a.rs"]);
        assert_eq!(
            db::open_query(&f.index, "other").err(),
            Some(db::DbError::Missing)
        );
    }

    fn busy() -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY), None)
    }

    /// Hooks whose `kind` step fails with SQLITE_BUSY for the first `n` calls.
    fn busy_for(n: usize, at_commit: bool) -> Hooks {
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let fail: Box<dyn Fn() -> rusqlite::Result<()> + Send + Sync> = Box::new(move || {
            if calls.fetch_add(1, Ordering::SeqCst) < n {
                Err(busy())
            } else {
                Ok(())
            }
        });
        let mut hooks = Hooks::default();
        if at_commit {
            hooks.tx.fail_commit = Some(fail);
        } else {
            hooks.tx.fail_begin = Some(fail);
        }
        hooks
    }

    #[test]
    fn a_busy_batch_is_retried_and_counted_once() {
        let _g = pinned();
        for at_commit in [false, true] {
            // Arrange
            let f = fx(if at_commit {
                "idx-busy-commit"
            } else {
                "idx-busy-begin"
            });
            write(&f.root, "a.rs", b"fn a() {}\n");
            write(&f.root, "b.rs", b"fn b() {}\n");
            let opts = Options {
                hooks: busy_for(2, at_commit),
                ..Options::default()
            };

            // Act
            let report = f.run_with(&opts);

            // Assert
            assert_eq!(report.failed, 0, "at_commit {at_commit}");
            assert_eq!(report.changed, 2);
            assert_eq!(report.parsed, 2);
        }
    }

    #[test]
    fn a_batch_that_stays_busy_fails_the_run_with_the_busy_error() {
        let _g = pinned();
        for at_commit in [false, true] {
            // Arrange
            let f = fx("idx-busy-forever");
            write(&f.root, "a.rs", b"fn a() {}\n");
            let opts = Options {
                hooks: busy_for(BATCH_RETRIES + 1, at_commit),
                ..Options::default()
            };

            // Act
            let got = run(&f.root, &f.index, &f.worktree, false, &opts);

            // Assert
            let message = got.expect_err("must fail");
            assert!(
                message.contains("busy") || message.contains("locked"),
                "{message}"
            );
        }
    }

    #[test]
    fn corruption_inside_a_run_write_names_rebuild() {
        let _g = pinned();
        // Arrange
        let f = fx("idx-corrupt-write");
        write(&f.root, "a.rs", b"fn a() {}\n");
        let opts = Options {
            hooks: Hooks {
                fail_write: Some(rusqlite::ffi::SQLITE_CORRUPT),
                ..Hooks::default()
            },
            ..Options::default()
        };

        // Act
        let got = run(&f.root, &f.index, &f.worktree, false, &opts);

        // Assert
        let message = got.expect_err("must fail");
        assert!(message.contains("--rebuild"), "{message}");
    }

    /// A second worktree of the fixture's store, with its own checkout.
    fn sibling(f: &crate::testkit::Fx, tag: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let root = crate::walker::test_repo::init(tag);
        for (name, bytes) in files {
            write(&root, name, bytes);
        }
        let _ = f;
        root
    }

    fn run_in(f: &crate::testkit::Fx, root: &Path, key: &str, opts: &Options) -> Report {
        run(root, &f.index, key, false, opts).expect("sibling run")
    }

    const THREE: [(&str, &[u8]); 3] = [
        ("a.rs", b"fn a() {}\n"),
        ("b.py", b"def b():\n    pass\n"),
        ("c.ts", b"export const c = 1;\n"),
    ];

    fn seeded(tag: &str) -> crate::testkit::Fx {
        let f = fx(tag);
        for (name, bytes) in THREE {
            write(&f.root, name, bytes);
        }
        f.run();
        f
    }

    #[test]
    fn a_new_worktree_with_the_same_files_parses_nothing() {
        let _g = pinned();
        // Arrange
        let f = seeded("reuse-same");
        let b = sibling(&f, "reuse-same-b", &THREE);

        // Act
        let report = run_in(&f, &b, "other", &Options::default());

        // Assert
        assert_eq!(report.parsed, 0);
        assert_eq!(report.reused, 3);
        assert_eq!(count(&f.conn(), "SELECT COUNT(*) FROM content"), 3);
    }

    #[test]
    fn one_different_file_is_parsed_and_the_rest_reused() {
        let _g = pinned();
        // Arrange
        let f = seeded("reuse-one-diff");
        let b = sibling(
            &f,
            "reuse-one-diff-b",
            &[THREE[0], THREE[1], ("c.ts", b"export const other = 2;\n")],
        );

        // Act
        let report = run_in(&f, &b, "other", &Options::default());

        // Assert
        assert_eq!(report.parsed, 1);
        assert_eq!(report.reused, 2);
    }

    #[test]
    fn an_edit_to_bytes_the_store_holds_moves_the_row_and_drops_the_old_content() {
        let _g = pinned();
        // Arrange
        let f = seeded("reuse-edit");
        let b = sibling(&f, "reuse-edit-b", &[("x.rs", b"fn only_in_b() {}\n")]);
        run_in(&f, &b, "other", &Options::default());
        write(&b, "x.rs", b"fn a() {}\n");

        // Act
        let report = run_in(&f, &b, "other", &Options::default());

        // Assert
        assert_eq!(report.reused, 1);
        assert_eq!(report.parsed, 0);
        let conn = db::open_query(&f.index, "other").expect("b");
        assert_eq!(symbol_names(&conn, "x.rs"), vec!["a"]);
        assert_eq!(count(&f.conn(), "SELECT COUNT(*) FROM content"), 3);
    }

    #[test]
    fn content_removed_after_the_hint_is_parsed_in_a_trailing_batch() {
        let _g = pinned();
        // Arrange
        let f = seeded("reuse-race");
        let b = sibling(&f, "reuse-race-b", &THREE);
        let index = f.index.clone();
        let hook = move || {
            let raw = Connection::open(&index).expect("raw");
            raw.pragma_update(None, "foreign_keys", "ON").expect("fk");
            let id: i64 = raw
                .query_row(
                    "SELECT content FROM manifest WHERE path = 'c.ts'",
                    [],
                    |r| r.get(0),
                )
                .expect("content id");
            raw.execute("DELETE FROM manifest WHERE path = 'c.ts'", [])
                .expect("manifest");
            raw.execute(
                "DELETE FROM chunks_fts WHERE rowid IN (SELECT id FROM chunks WHERE content = ?1)",
                [id],
            )
            .expect("fts");
            raw.execute("DELETE FROM content WHERE id = ?1", [id])
                .expect("content");
        };
        let opts = Options {
            hooks: Hooks {
                after_hint: Some(Box::new(hook)),
                ..Hooks::default()
            },
            ..Options::default()
        };

        // Act
        let report = run_in(&f, &b, "other", &opts);

        // Assert
        assert_eq!(report.failed, 0);
        assert_eq!(report.reused, 2);
        assert_eq!(report.parsed, 1);
        let conn = db::open_query(&f.index, "other").expect("b");
        assert_eq!(paths(&conn), vec!["a.rs", "b.py", "c.ts"]);
        assert_eq!(symbol_names(&conn, "c.ts"), vec!["c"]);
    }

    #[test]
    fn a_new_parser_key_parses_every_file_and_leaves_the_old_queries_alone() {
        let _g = pinned();
        // Arrange
        let f = fx("reuse-parser");
        for (name, bytes) in THREE {
            write(&f.root, name, bytes);
        }
        let old = Options {
            parser: "0.39.0.1".to_string(),
            ..Options::default()
        };
        f.run_with(&old);
        let b = sibling(&f, "reuse-parser-b", &THREE);
        let new = Options {
            parser: "0.40.0.1".to_string(),
            ..Options::default()
        };

        // Act
        let report = run_in(&f, &b, "other", &new);

        // Assert
        assert_eq!(report.parsed, 3);
        assert_eq!(report.reused, 0);
        assert_eq!(count(&f.conn(), "SELECT COUNT(*) FROM content"), 6);
        assert_eq!(symbol_names(&f.conn(), "a.rs"), vec!["a"]);
    }

    #[test]
    fn a_partial_file_stays_partial_in_the_worktree_that_adopts_it() {
        let _g = pinned();
        // Arrange
        let f = fx("reuse-partial");
        let files: [(&str, &[u8]); 2] = [
            ("bad.rs", b"fn ok() {}\nfn broken( {\n"),
            ("bin.txt", b"caf\xe9\n"),
        ];
        for (name, bytes) in files {
            write(&f.root, name, bytes);
        }
        f.run();
        let b = sibling(&f, "reuse-partial-b", &files);

        // Act
        let report = run_in(&f, &b, "other", &Options::default());

        // Assert
        assert_eq!(report.reused, 2);
        let conn = db::open_query(&f.index, "other").expect("b");
        let statuses: Vec<String> = conn
            .prepare("SELECT status FROM files ORDER BY path")
            .expect("prepare")
            .query_map([], |r| r.get(0))
            .expect("rows")
            .map(|r| r.expect("status"))
            .collect();
        assert_eq!(statuses, vec!["partial", "partial"]);
    }

    #[test]
    fn a_text_file_is_adopted_with_the_text_grammar() {
        let _g = pinned();
        // Arrange
        let f = fx("reuse-text");
        write(&f.root, "notes.txt", b"plain words\n");
        f.run();
        let b = sibling(&f, "reuse-text-b", &[("notes.txt", b"plain words\n")]);

        // Act
        let report = run_in(&f, &b, "other", &Options::default());

        // Assert
        assert_eq!((report.parsed, report.reused), (0, 1));
        assert_eq!(
            count(
                &f.conn(),
                "SELECT COUNT(*) FROM content WHERE grammar = 'text'"
            ),
            1
        );
    }

    #[test]
    fn the_same_bytes_under_another_grammar_are_parsed() {
        let _g = pinned();
        // Arrange
        let f = fx("reuse-grammar");
        write(&f.root, "a.ts", b"export const v = 1;\n");
        f.run();
        let b = sibling(
            &f,
            "reuse-grammar-b",
            &[("a.tsx", b"export const v = 1;\n")],
        );

        // Act
        let report = run_in(&f, &b, "other", &Options::default());

        // Assert
        assert_eq!((report.parsed, report.reused), (1, 0));
    }

    #[test]
    fn a_rebuild_writes_the_symbols_of_stored_content_again() {
        let _g = pinned();
        // Arrange
        let f = seeded("reuse-rebuild");
        let b = sibling(&f, "reuse-rebuild-b", &THREE);
        run_in(&f, &b, "other", &Options::default());
        {
            let raw = Connection::open(&f.index).expect("raw");
            raw.execute("DELETE FROM symbols", []).expect("delete");
        }

        // Act
        let report =
            run(&f.root, &f.index, &f.worktree, true, &Options::default()).expect("rebuild");

        // Assert
        assert_eq!(report.parsed, 3);
        assert_eq!(symbol_names(&f.conn(), "a.rs"), vec!["a"]);
        let conn = db::open_query(&f.index, "other").expect("b");
        assert_eq!(symbol_names(&conn, "a.rs"), vec!["a"]);
    }
}
