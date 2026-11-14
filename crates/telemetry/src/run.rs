// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `playbook usage` commands' shared steps: resolve where the local data
//! lives, ingest incrementally, render.

use super::account::{account_label, UNKNOWN_ACCOUNT};
use super::backfill;
use super::claude_code::ClaudeCodeSource;
use super::codex::CodexSource;
use super::db::{load_tool_events, load_usage_events, open_db};
use super::ingest::{ingest, ingest_claude, IngestStats};
use super::limits_source;
use super::query::Range;
use super::summary;
use super::UsageEvent;
use playbook_store::Connection;
use std::path::{Path, PathBuf};

/// Where Claude Code's and Codex's data and playbook's usage store live for one home.
#[derive(Clone)]
pub struct Paths {
    pub claude_projects: PathBuf,
    pub claude_json: PathBuf,
    pub codex_sessions: PathBuf,
    pub db: PathBuf,
}

impl Paths {
    pub fn from_home(home: &Path) -> Self {
        Self {
            claude_projects: home.join(".claude").join("projects"),
            claude_json: home.join(".claude.json"),
            codex_sessions: home.join(".codex").join("sessions"),
            db: super::host::state_dir_from(home).join("usage.db"),
        }
    }

    pub fn real() -> Self {
        let mut paths = Self::from_home(&super::host::home_dir());
        paths.db = super::host::state_dir().join("usage.db");
        let config = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty());
        paths.with_claude_dir(config.map(PathBuf::from).as_deref())
    }

    /// Reads Claude Code history from `dir` when it is set: Claude Code keeps
    /// its history there instead of in `~/.claude` under `CLAUDE_CONFIG_DIR`.
    pub fn with_claude_dir(mut self, dir: Option<&Path>) -> Self {
        if let Some(dir) = dir {
            self.claude_projects = dir.join("projects");
            self.claude_json = dir.join(".claude.json");
        }
        self
    }
}

/// Ingests new transcript events into the store and returns what was added.
/// Blocks until done. A first read of a large history that runs past two
/// seconds says how far it is on stderr, as a file count.
pub fn ingest_new(paths: &Paths) -> Result<(Connection, IngestStats), String> {
    let conn = open_db(&paths.db)?;
    let started = std::time::Instant::now();
    let mut shown = false;
    let stats = ingest_into(paths, &conn, &mut |done, total| {
        if started.elapsed() >= std::time::Duration::from_secs(2)
            && total > 0
            && !crate::say::is_held()
        {
            eprint!("\rindexing {done}/{total} files");
            shown = true;
        }
        true
    })?;
    if shown {
        eprintln!();
    }
    Ok((conn, stats))
}

/// The steps of `ingest_new` on an open store. `on_batch(done, total)` reports
/// the first-run backfill after each committed batch.
pub fn ingest_into(
    paths: &Paths,
    conn: &Connection,
    on_batch: &mut dyn FnMut(usize, usize) -> bool,
) -> Result<IngestStats, String> {
    let source = ClaudeCodeSource::new(paths.claude_projects.clone());
    backfill::run_once(&source, conn)?;
    if let Some(dir) = paths.db.parent() {
        super::session_account::import(conn, dir)?;
        super::session_account::repair_joined_keys(conn)?;
    }
    let account = account_label(&paths.claude_json);
    let mut stats = ingest_claude(&source, &account, conn, on_batch)?;
    if paths.codex_sessions.is_dir() {
        let codex = CodexSource::new(paths.codex_sessions.clone());
        let more = ingest(&codex, UNKNOWN_ACCOUNT, conn)?;
        stats.usage_inserted += more.usage_inserted;
        stats.usage_updated += more.usage_updated;
        stats.tools_inserted += more.tools_inserted;
        stats.duplicates_skipped += more.duplicates_skipped;
    }
    // Telemetry never changes the totals; a failure here leaves transcripts as the source.
    let _ = super::agent_names::resolve_custom(conn, &paths.claude_projects);
    let _ = super::reconcile::reconcile(conn, crate::util::time::now_secs());
    Ok(stats)
}

/// The account key behind an `--account` value, or `None` without the flag.
/// Errors when no event carries the key, so a typo never reads as an empty report.
pub fn resolve_account(conn: &Connection, input: Option<&str>) -> Result<Option<String>, String> {
    let Some(input) = input else {
        return Ok(None);
    };
    let key = super::account_store::resolve(conn, input)?;
    if !super::account_store::has_events(conn, &key)? {
        return Err(format!(
            "no usage is recorded for the account `{input}`; run `playbook usage account list` to see the accounts"
        ));
    }
    Ok(Some(key))
}

/// The text report. `range` is `None` for every recorded session, `account`
/// an account key or name that keeps only that account's events.
pub fn run_summary(
    paths: &Paths,
    range: Option<Range>,
    now: i64,
    account: Option<&str>,
) -> Result<String, String> {
    let (conn, _) = ingest_new(paths)?;
    let key = resolve_account(&conn, account)?;
    let mut usage = load_usage_events(&conn)?;
    super::tui::account::keep(&mut usage, key.as_deref());
    let mut tools = load_tool_events(&conn)?;
    super::tui::account::keep_tools(&mut tools, key.as_deref());
    let mask_on = super::tui::start_mask_account(&super::host::home_dir());
    let choices: Vec<(String, String)> = if mask_on || key.is_some() {
        super::account_store::accounts(&conn)?
            .into_iter()
            .map(|a| (a.key, a.name))
            .collect()
    } else {
        Vec::new()
    };
    let mask = mask_on.then_some(choices.as_slice());
    let mut text = match range {
        Some(range) => summary::render_range(&usage, &tools, range, now, mask),
        None => summary::render(&usage, &tools, mask),
    };
    if let Some(key) = &key {
        let shown = super::account_store::display(&choices, key);
        let shown = if mask.is_some() {
            super::tui::account::header_label(key, &shown, &choices)
        } else {
            shown
        };
        let at = text.find('\n').map_or(text.len(), |i| i + 1);
        text.insert_str(at, &format!("Account: {shown}\n"));
    }
    if !usage.is_empty() {
        text.push_str(&limits_source::summary_section(
            &limit_sources(
                paths,
                &conn,
                limits_source::events_since(now, super::entry::configured_week_start()),
                now,
            ),
            &usage,
            now,
            super::entry::configured_week_start(),
        ));
    }
    Ok(text)
}

/// The stored plan limits, the manual budgets from the config and what
/// Claude Code caches in its config file.
///
/// The 429 times from `since` through `now` come from the usage store and are
/// left empty when it cannot be read: the marker is a hint, never an error.
/// The window readings come from `rate_limits.jsonl`.
pub fn limit_sources(
    paths: &Paths,
    conn: &Connection,
    since: i64,
    now: i64,
) -> limits_source::Sources {
    let mut sources = limits_source::Sources::load(
        paths.db.parent(),
        &paths.claude_json,
        &super::host::home_dir(),
    );
    sources.history = paths
        .db
        .parent()
        .map_or_else(Vec::new, super::limits_history::load);
    sources.rate_limited = super::telemetry_db::rate_limited_at(conn, since, now.saturating_add(1))
        .unwrap_or_default();
    sources
}

/// The `limits` object of `--json`. `events` must reach back to
/// `limits_source::events_since`.
pub fn limits_json(
    paths: &Paths,
    conn: &Connection,
    events: &[UsageEvent],
    now: i64,
) -> serde_json::Value {
    limits_source::report_json(
        &limit_sources(
            paths,
            conn,
            limits_source::events_since(now, super::entry::configured_week_start()),
            now,
        ),
        events,
        now,
        super::entry::configured_week_start(),
    )
}

#[cfg(test)]
mod claude_dir_tests {
    use super::*;

    #[test]
    fn a_claude_config_dir_moves_the_transcripts_and_the_account_file() {
        let base = Paths::from_home(Path::new("/fake/home"));
        let moved = base.clone().with_claude_dir(Some(Path::new("/fake/cfg")));
        assert_eq!(moved.claude_projects, Path::new("/fake/cfg/projects"));
        assert_eq!(moved.claude_json, Path::new("/fake/cfg/.claude.json"));
        assert_eq!(moved.db, base.db);
        let same = base.clone().with_claude_dir(None);
        assert_eq!(same.claude_projects, base.claude_projects);
    }
}
