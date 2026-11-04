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
pub mod location;
pub mod output;
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
    let root = location::repo_root(cwd)?;
    let index_path = location::index_path()?;
    let report = index::run(
        &root,
        &index_path,
        &host::worktree_id(),
        rebuild,
        &index::Options::default(),
    )?;
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
        &rusqlite::Connection,
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
