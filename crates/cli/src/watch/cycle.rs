// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! One pass over every watched repo, and the loop that repeats it. `once` is
//! one pass, so cron, tests and the foreground loop run the same code.

use super::audit::AuditLog;
use super::backoff::{delay, Jitter};
use super::clock::Clock;
use super::engine::GithubWrite;
use super::github::{GithubPoller, Http};
use super::journal::{recover, RecoverOps};
use super::lock::{acquire, Acquire};
use super::source::{EventSource, Poll, SourceError};
use super::store::{Cursor, Store};
use super::{poll_error, repo_rules, start_cursor, Ctx, Settings, ENDPOINTS, OFF_MESSAGE};

/// What one pass did and how long to wait before the next.
#[derive(Debug, Default)]
pub struct Cycle {
    pub out: String,
    /// Seconds to wait: the longest of the poll setting, `X-Poll-Interval`
    /// and any rate limit.
    pub wait: i64,
    /// A source failed in a way that a later try may fix.
    pub transient: bool,
}

/// Polls every repo once, queues what matched, and runs the approved items.
/// `since` replays from that time instead of starting a new source from now.
pub fn cycle<W: GithubWrite + RecoverOps, C: Clock, H: Http>(
    ctx: &Ctx<'_, W, C>,
    settings: &Settings,
    since: Option<i64>,
    store: &mut Store,
    audit: &AuditLog,
    make_http: &mut impl FnMut() -> H,
) -> Result<Cycle, String> {
    let mut report = Cycle {
        wait: settings.poll_seconds,
        ..Cycle::default()
    };
    // The lock is held, so an item still `running` belongs to a killed run.
    for (id, how) in recover(store, ctx.write).map_err(|e| e.to_string())? {
        report
            .out
            .push_str(&format!("item {id} recovered: {how:?}\n"));
    }
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
            let stored = store.cursor(&source).map_err(|e| e.to_string())?;
            let (cursor, started) = start_cursor(stored, since, ctx.clock.now());
            if !started {
                store
                    .transaction(|tx| tx.set_cursor(&source, &cursor))
                    .map_err(|e| e.to_string())?;
                report
                    .out
                    .push_str(&format!("{source}: watching from now\n"));
                continue;
            }
            // A source answers at most once per its wait, whichever process asks.
            let now = ctx.clock.now();
            if since.is_none() && cursor.next_poll_at > now {
                report.wait = report.wait.max(cursor.next_poll_at - now);
                continue;
            }
            match poller.poll(&cursor, &mut events) {
                Ok(Poll::Events { mut next, next_in }) => {
                    report.wait = report.wait.max(next_in);
                    next.next_poll_at = now + next_in;
                    let mut engine = ctx.engine(store, audit, settings);
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
                            report.out.push_str(&format!(
                                "proposal {id}: rule {} on {}#{}\n",
                                row.rule, row.repo, row.object
                            ));
                        }
                    }
                }
                Ok(Poll::Unchanged { next_in }) => {
                    report.wait = report.wait.max(next_in);
                    let later = Cursor {
                        next_poll_at: now + next_in,
                        ..cursor
                    };
                    store
                        .transaction(|tx| tx.set_cursor(&source, &later))
                        .map_err(|e| e.to_string())?;
                }
                Err(err) => {
                    poll_error(&source, &err, &mut report.out)?;
                    match err {
                        SourceError::RateLimited { retry_at } => {
                            report.wait = report.wait.max(retry_at - ctx.clock.now());
                        }
                        _ => report.transient = true,
                    }
                }
            }
        }
        let mut engine = ctx.engine(store, audit, settings);
        engine
            .redecide(repo, &rules, level)
            .map_err(|e| e.to_string())?;
        let ran = engine
            .run_approved(repo, &rules, level)
            .map_err(|e| e.to_string())?;
        if ran > 0 {
            report
                .out
                .push_str(&format!("{repo}: ran {ran} approved\n"));
        }
    }
    Ok(report)
}

/// The seams of the loop.
pub struct LoopEnv<'a> {
    /// Waits this many seconds.
    pub sleep: &'a mut dyn FnMut(i64),
    /// Whether to stop before the next pass.
    pub stop: &'a dyn Fn() -> bool,
    /// Prints a pass's output.
    pub print: &'a mut dyn FnMut(&str),
    pub jitter: &'a dyn Jitter,
}

/// `playbook watch` with no verb: passes until stopped. The instance lock is
/// held for the whole run. A transient failure backs off and goes on; a
/// refused token stops the loop.
pub fn run_loop<W: GithubWrite + RecoverOps, C: Clock, H: Http>(
    ctx: &Ctx<'_, W, C>,
    mut make_http: impl FnMut() -> H,
    env: &mut LoopEnv<'_>,
) -> Result<(), String> {
    let Acquire::Held(_lock) = acquire(&ctx.data.join("watch.lock")).map_err(|e| e.to_string())?
    else {
        return Err("watch already running".to_string());
    };
    let mut store = Store::open(&ctx.data.join("watch.db")).map_err(|e| e.to_string())?;
    let audit = AuditLog::new(&ctx.data.join("watch"));
    let mut failures: u32 = 0;
    let mut said_off = false;
    while !(env.stop)() {
        // Read each pass, so a change of setting applies without a restart.
        let settings = Settings::load(ctx.home)?;
        let wait = if settings.enabled {
            said_off = false;
            let pass = cycle(ctx, &settings, None, &mut store, &audit, &mut make_http)?;
            (env.print)(&pass.out);
            if pass.transient {
                failures += 1;
                pass.wait.max(delay(failures - 1, env.jitter))
            } else {
                failures = 0;
                pass.wait
            }
        } else {
            if !said_off {
                (env.print)(OFF_MESSAGE);
                said_off = true;
            }
            settings.poll_seconds
        };
        (env.sleep)(wait);
    }
    Ok(())
}
