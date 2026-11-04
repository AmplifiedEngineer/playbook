// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Watch mode for GitHub events (ADR 0055): `playbook watch once`, `dry-run`,
//! `approve` and `reject`. The side effects (GitHub reads, the `gh` writes
//! and the clock) arrive through [`Ctx`], so tests run the same code with
//! fakes. Only [`run`] builds the real ones.

use std::path::Path;

use serde_json::Value;

use crate::common::time::parse_iso8601_utc_epoch;
use crate::config::{self, store as config_store};
use crate::WatchCommand;

pub mod audit;
pub mod clock;
pub mod engine;
pub mod event;
pub mod gh;
pub mod github;
pub mod http;
pub mod lock;
pub mod rule;
pub mod settings;
pub mod source;
pub mod store;

use audit::AuditLog;
use clock::{Clock, SystemClock};
use engine::{matches, ApproveOutcome, Engine, GithubWrite, NamedRule};
use github::{Endpoint, GithubPoller, Http};
use lock::{acquire, Acquire};
use rule::Autonomy;
use settings::{ceiling, effective, resolve_rule};
use source::{EventSource, Poll, SourceError};
use store::{Cursor, Store};

const ENDPOINTS: [Endpoint; 3] = [
    Endpoint::Issues,
    Endpoint::IssueComments,
    Endpoint::PullComments,
];
const DEFAULT_TOKEN_ENV: &str = "GITHUB_TOKEN";
const OFF_MESSAGE: &str = "watch is off: run `playbook config set watch.enabled true`\n";

/// What a command needs from outside the process.
pub struct Ctx<'a, W, C> {
    pub home: &'a Path,
    pub data: &'a Path,
    pub write: &'a W,
    pub clock: &'a C,
    /// The login of the authenticated user.
    pub me: &'a str,
}

pub struct Settings {
    pub enabled: bool,
    pub repos: Vec<String>,
    pub poll_seconds: i64,
    pub max_catchup_seconds: i64,
    pub token_env: String,
}

impl Settings {
    pub fn load(home: &Path) -> Result<Self, String> {
        let get = |key: &str| -> Result<Value, String> {
            config::resolve_valid(key, home, None)
                .map(|(value, _, _)| value)
                .map_err(|e| e.to_string())
        };
        let repos = get("watch.repos")?
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            enabled: get("watch.enabled")?.as_bool().unwrap_or(false),
            repos,
            poll_seconds: get("watch.pollSeconds")?.as_i64().unwrap_or(60),
            max_catchup_seconds: get("watch.maxCatchupHours")?.as_i64().unwrap_or(72) * 3600,
            token_env: get("watch.tokenEnv")?
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or(DEFAULT_TOKEN_ENV)
                .to_string(),
        })
    }
}

/// The rules that apply to `repo` and the autonomy ceiling across its tiers.
fn repo_rules(home: &Path, repo: &str) -> Result<(Vec<NamedRule>, Autonomy), String> {
    let root = crate::common::paths::playbook_root_from(home);
    let text = |v: Option<Value>| v.and_then(|v| v.as_str().map(String::from));
    let mut rules = Vec::new();
    for key in config_store::keys_with_prefix(&root, "watch.rule.").map_err(|e| e.to_string())? {
        let tiers = config_store::lookup(&root, &key, Some(repo)).map_err(|e| e.to_string())?;
        let texts = [text(tiers.repo), text(tiers.org), text(tiers.global)];
        let texts: Vec<Option<&str>> = texts
            .iter()
            .map(|t| t.as_deref().filter(|s| !s.is_empty()))
            .collect();
        let id = key.trim_start_matches("watch.rule.").to_string();
        if let Some(rule) = resolve_rule(&texts).map_err(|e| format!("rule {id}: {e}"))? {
            rules.push((id, rule));
        }
    }
    let tiers =
        config_store::lookup(&root, "watch.autonomy", Some(repo)).map_err(|e| e.to_string())?;
    let levels: Vec<Autonomy> = [tiers.repo, tiers.org, tiers.global]
        .into_iter()
        .flatten()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect();
    Ok((rules, ceiling(&levels)))
}

fn parse_since(since: Option<&str>) -> Result<Option<i64>, String> {
    since
        .map(|s| {
            parse_iso8601_utc_epoch(s)
                .ok_or_else(|| format!("--since must look like 2026-11-01T00:00:00Z, got {s}"))
        })
        .transpose()
}

/// Where a source starts. With no stored cursor and no `--since`, a source
/// starts now so that turning watch mode on never replays old history.
fn start_cursor(stored: Option<Cursor>, since: Option<i64>, now: i64) -> (Cursor, bool) {
    match (since, stored) {
        (Some(t), _) => (
            Cursor {
                since: t,
                ..Cursor::default()
            },
            true,
        ),
        (None, Some(c)) => (c, true),
        (None, None) => (
            Cursor {
                since: now,
                ..Cursor::default()
            },
            false,
        ),
    }
}

fn poll_error(source: &str, err: SourceError, out: &mut String) -> Result<(), String> {
    match err {
        SourceError::Unauthorized => Err(err.to_string()),
        other => {
            out.push_str(&format!("{source}: {other}\n"));
            Ok(())
        }
    }
}

/// `playbook watch once`: one poll of every repo, then the approved items.
pub fn once<W: GithubWrite, C: Clock, H: Http>(
    ctx: &Ctx<'_, W, C>,
    since: Option<&str>,
    mut make_http: impl FnMut() -> H,
) -> Result<String, String> {
    let settings = Settings::load(ctx.home)?;
    if !settings.enabled {
        return Ok(OFF_MESSAGE.to_string());
    }
    let since = parse_since(since)?;
    let Acquire::Held(_lock) = acquire(&ctx.data.join("watch.lock")).map_err(|e| e.to_string())?
    else {
        return Ok("watch already running\n".to_string());
    };
    let mut store = Store::open(&ctx.data.join("watch.db")).map_err(|e| e.to_string())?;
    let audit = AuditLog::new(&ctx.data.join("watch"));
    let mut out = String::new();
    let mut events = Vec::new();
    for repo in &settings.repos {
        let (rules, level) = repo_rules(ctx.home, repo)?;
        for endpoint in ENDPOINTS {
            let mut poller = GithubPoller::new(
                make_http(),
                ctx.clock,
                repo,
                endpoint,
                settings.poll_seconds,
            );
            let source = format!("{repo}/{}", poller.name());
            let now = ctx.clock.now();
            let stored = store.cursor(&source).map_err(|e| e.to_string())?;
            let (cursor, started) = start_cursor(stored, since, now);
            if !started {
                store
                    .transaction(|tx| tx.set_cursor(&source, &cursor))
                    .map_err(|e| e.to_string())?;
                out.push_str(&format!("{source}: watching from now\n"));
                continue;
            }
            match poller.poll(&cursor, &mut events) {
                Ok(Poll::Events { next, .. }) => {
                    let mut engine = Engine {
                        store: &mut store,
                        write: ctx.write,
                        clock: ctx.clock,
                        audit: &audit,
                        me: ctx.me,
                    };
                    let ids = engine
                        .ingest(
                            &source,
                            &next,
                            &events,
                            &rules,
                            level,
                            settings.max_catchup_seconds,
                        )
                        .map_err(|e| e.to_string())?;
                    for id in ids {
                        if let Some(row) = store.work(id).map_err(|e| e.to_string())? {
                            out.push_str(&format!(
                                "proposal {id}: rule {} on {}#{}\n",
                                row.rule, row.repo, row.object
                            ));
                        }
                    }
                }
                Ok(Poll::Unchanged { .. }) => {}
                Err(err) => poll_error(&source, err, &mut out)?,
            }
        }
        let mut engine = Engine {
            store: &mut store,
            write: ctx.write,
            clock: ctx.clock,
            audit: &audit,
            me: ctx.me,
        };
        let ran = engine
            .run_approved(repo, &rules)
            .map_err(|e| e.to_string())?;
        if ran > 0 {
            out.push_str(&format!("{repo}: ran {ran} approved\n"));
        }
    }
    Ok(out)
}

/// `playbook watch dry-run`: what would be proposed. Reads GitHub and the
/// stored cursors, and changes nothing.
pub fn dry_run<W: GithubWrite, C: Clock, H: Http>(
    ctx: &Ctx<'_, W, C>,
    since: Option<&str>,
    mut make_http: impl FnMut() -> H,
) -> Result<String, String> {
    let settings = Settings::load(ctx.home)?;
    let since = parse_since(since)?;
    let db = ctx.data.join("watch.db");
    let store = db
        .exists()
        .then(|| Store::open(&db))
        .transpose()
        .map_err(|e| e.to_string())?;
    let mut out = String::new();
    let mut events = Vec::new();
    for repo in &settings.repos {
        let (rules, level) = repo_rules(ctx.home, repo)?;
        for (id, rule) in &rules {
            out.push_str(&format!(
                "{repo}: rule {id}: {:?} -> {:?} at {:?}\n",
                rule.on,
                rule.action,
                effective(rule.autonomy, level)
            ));
        }
        for endpoint in ENDPOINTS {
            let mut poller = GithubPoller::new(
                make_http(),
                ctx.clock,
                repo,
                endpoint,
                settings.poll_seconds,
            );
            let source = format!("{repo}/{}", poller.name());
            let stored = match &store {
                Some(s) => s.cursor(&source).map_err(|e| e.to_string())?,
                None => None,
            };
            let (cursor, started) = start_cursor(stored, since, ctx.clock.now());
            if !started {
                continue;
            }
            match poller.poll(&cursor, &mut events) {
                Ok(_) => {
                    for event in &events {
                        for (id, rule) in &rules {
                            if effective(rule.autonomy, level) != Autonomy::Off
                                && matches(rule, event, ctx.me)
                            {
                                out.push_str(&format!(
                                    "would propose: rule {id} for {} on {}#{}\n",
                                    event.key, event.repo, event.number
                                ));
                            }
                        }
                    }
                }
                Err(err) => poll_error(&source, err, &mut out)?,
            }
        }
    }
    Ok(out)
}

/// `playbook watch approve <id>`. With another process running watch, the
/// item is only marked and that process carries it out.
pub fn approve<W: GithubWrite, C: Clock>(ctx: &Ctx<'_, W, C>, id: i64) -> Result<String, String> {
    let mut store = Store::open(&ctx.data.join("watch.db")).map_err(|e| e.to_string())?;
    let row = store
        .work(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("item {id} does not exist"))?;
    let (rules, _) = repo_rules(ctx.home, &row.repo)?;
    let lock = acquire(&ctx.data.join("watch.lock")).map_err(|e| e.to_string())?;
    let audit = AuditLog::new(&ctx.data.join("watch"));
    let mut engine = Engine {
        store: &mut store,
        write: ctx.write,
        clock: ctx.clock,
        audit: &audit,
        me: ctx.me,
    };
    let outcome = engine
        .approve(id, &rules, matches!(lock, Acquire::Held(_)))
        .map_err(|e| e.to_string())?;
    Ok(match outcome {
        ApproveOutcome::Handed => {
            format!("item {id} approved: the running watch will carry it out\n")
        }
        ApproveOutcome::Done => format!("item {id} done\n"),
        ApproveOutcome::AlreadyDone => format!("item {id} already done\n"),
        ApproveOutcome::Skipped(why) => format!("item {id} skipped: {why}\n"),
    })
}

/// `playbook watch reject <id>`.
pub fn reject<W: GithubWrite, C: Clock>(ctx: &Ctx<'_, W, C>, id: i64) -> Result<String, String> {
    let mut store = Store::open(&ctx.data.join("watch.db")).map_err(|e| e.to_string())?;
    let audit = AuditLog::new(&ctx.data.join("watch"));
    let mut engine = Engine {
        store: &mut store,
        write: ctx.write,
        clock: ctx.clock,
        audit: &audit,
        me: ctx.me,
    };
    engine.reject(id).map_err(|e| e.to_string())?;
    Ok(format!("item {id} rejected\n"))
}

/// Builds the real GitHub clients and runs `cmd`.
pub fn run(cmd: &WatchCommand) -> Result<String, String> {
    let home = crate::common::session::home_dir();
    let data = playbook_data::data_dir_from(&home);
    let settings = Settings::load(&home)?;
    if matches!(cmd, WatchCommand::Once { .. }) && !settings.enabled {
        return Ok(OFF_MESSAGE.to_string());
    }
    let token = if matches!(cmd, WatchCommand::Reject { .. }) {
        String::new()
    } else {
        std::env::var(&settings.token_env).map_err(|_| {
            format!(
                "set the {} environment variable to a GitHub token",
                settings.token_env
            )
        })?
    };
    let write = gh::GhWrite::new(token.clone());
    let clock = SystemClock;
    let me = match cmd {
        WatchCommand::Once { .. } | WatchCommand::DryRun { .. } => write.login()?,
        WatchCommand::Approve { .. } | WatchCommand::Reject { .. } => String::new(),
    };
    let ctx = Ctx {
        home: &home,
        data: &data,
        write: &write,
        clock: &clock,
        me: &me,
    };
    match cmd {
        WatchCommand::Once { since } => {
            once(&ctx, since.as_deref(), || http::UreqHttp::new(&token))
        }
        WatchCommand::DryRun { since } => {
            dry_run(&ctx, since.as_deref(), || http::UreqHttp::new(&token))
        }
        WatchCommand::Approve { id } => approve(&ctx, *id),
        WatchCommand::Reject { id } => reject(&ctx, *id),
    }
}

#[cfg(test)]
mod tests;
