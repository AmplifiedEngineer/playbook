// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The verbs that look at or steer a running watch: `status`, `pause`,
//! `resume`, `retry` and `log`.

use serde_json::{json, Value};

use super::audit::AuditLog;
use super::backoff::StdJitter;
use super::clock::Clock;
use super::engine::{Engine, GithubWrite};
use super::limits::day_of;
use super::lock::{acquire, Acquire};
use super::rule::Do;
use super::store::Store;
use super::{Ctx, Settings};

const QUEUE_STATES: [&str; 5] = ["queued", "proposed", "approved", "running", "dead"];

fn open(data: &std::path::Path) -> Result<Store, String> {
    Store::open(&data.join("watch.db")).map_err(|e| e.to_string())
}

fn with_engine<W: GithubWrite, C: Clock, T>(
    ctx: &Ctx<'_, W, C>,
    body: impl FnOnce(&mut Engine<'_, W, C>) -> Result<T, String>,
) -> Result<T, String> {
    let settings = Settings::load(ctx.home)?;
    let mut store = open(ctx.data)?;
    let audit = AuditLog::new(&ctx.data.join("watch"));
    let mut engine = Engine {
        store: &mut store,
        write: ctx.write,
        clock: ctx.clock,
        audit: &audit,
        jitter: &StdJitter,
        caps: settings.caps,
        me: ctx.me,
    };
    body(&mut engine)
}

/// `playbook watch pause` and `resume`. A paused watch starts no new action.
/// One already running finishes.
pub fn set_paused<W: GithubWrite, C: Clock>(
    ctx: &Ctx<'_, W, C>,
    paused: bool,
) -> Result<String, String> {
    with_engine(ctx, |engine| {
        engine.set_paused(paused).map_err(|e| e.to_string())
    })?;
    Ok(if paused {
        "watch paused: no new action starts\n"
    } else {
        "watch resumed\n"
    }
    .to_string())
}

/// `playbook watch retry <id>`: puts a dead item back in the queue.
pub fn retry<W: GithubWrite, C: Clock>(ctx: &Ctx<'_, W, C>, id: i64) -> Result<String, String> {
    with_engine(ctx, |engine| engine.retry(id).map_err(|e| e.to_string()))?;
    Ok(format!("item {id} queued again\n"))
}

/// What `status` reports, as JSON.
pub fn status_json<W: GithubWrite, C: Clock>(ctx: &Ctx<'_, W, C>) -> Result<Value, String> {
    let settings = Settings::load(ctx.home)?;
    let store = open(ctx.data)?;
    let running = matches!(
        acquire(&ctx.data.join("watch.lock")).map_err(|e| e.to_string())?,
        Acquire::Busy
    );
    let sql = |e: super::store::StoreError| e.to_string();
    let mut queue = serde_json::Map::new();
    for state in QUEUE_STATES {
        let count = store.ids_in_state(state).map_err(sql)?.len();
        queue.insert(state.to_string(), json!(count));
    }
    let mut dead = Vec::new();
    for id in store.ids_in_state("dead").map_err(sql)? {
        if let Some(row) = store.work(id).map_err(sql)? {
            dead.push(json!({
                "id": id, "rule": row.rule, "repo": row.repo,
                "object": row.object, "reason": row.reason,
            }));
        }
    }
    let (actions, usd) = store.counter(day_of(ctx.clock.now()), "*").map_err(sql)?;
    let mut ladder = serde_json::Map::new();
    for action in Do::ALL {
        let history = store.approvals(action.as_str()).map_err(sql)?;
        let failed = history.iter().filter(|a| a.failed).count();
        ladder.insert(
            action.as_str().to_string(),
            json!({ "approved": history.len(), "failed": failed }),
        );
    }
    Ok(json!({
        "enabled": settings.enabled,
        "paused": store.control("paused").map_err(sql)?.as_deref() == Some("1"),
        "running": running,
        "queue": queue,
        "dead": dead,
        "today": {
            "actions": actions, "actions_limit": settings.caps.actions_per_day,
            "usd": usd, "usd_limit": settings.caps.usd_per_day,
        },
        "ladder": ladder,
    }))
}

/// `playbook watch status`, as text or JSON.
pub fn status<W: GithubWrite, C: Clock>(ctx: &Ctx<'_, W, C>, json: bool) -> Result<String, String> {
    let s = status_json(ctx)?;
    if json {
        return Ok(format!("{s}\n"));
    }
    let flag = |key: &str| s[key].as_bool().unwrap_or(false);
    let mut out = format!(
        "enabled: {}\npaused: {}\nrunning: {}\n",
        flag("enabled"),
        flag("paused"),
        flag("running")
    );
    for state in QUEUE_STATES {
        out.push_str(&format!("{state}: {}\n", s["queue"][state]));
    }
    for item in s["dead"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "dead {}: rule {} on {}#{}: {}\n",
            item["id"],
            item["rule"].as_str().unwrap_or(""),
            item["repo"].as_str().unwrap_or(""),
            item["object"],
            item["reason"].as_str().unwrap_or("no reason"),
        ));
    }
    let today = &s["today"];
    out.push_str(&format!(
        "today: {} of {} actions, {} of {} dollars\n",
        today["actions"], today["actions_limit"], today["usd"], today["usd_limit"]
    ));
    for action in Do::ALL {
        let class = &s["ladder"][action.as_str()];
        out.push_str(&format!(
            "{}: {} approved, {} failed\n",
            action.as_str(),
            class["approved"],
            class["failed"]
        ));
    }
    Ok(out)
}

/// `playbook watch log`: the audit records, newest last, filtered by rule and
/// outcome. Control characters are removed again on the way out.
pub fn log<W: GithubWrite, C: Clock>(
    ctx: &Ctx<'_, W, C>,
    rule: Option<&str>,
    outcome: Option<&str>,
    json: bool,
) -> Result<String, String> {
    let records = AuditLog::new(&ctx.data.join("watch"))
        .records()
        .map_err(|e| e.to_string())?;
    let wanted = |record: &Value, field: &str, want: Option<&str>| {
        want.is_none_or(|w| record[field].as_str() == Some(w))
    };
    let mut out = String::new();
    for record in records
        .iter()
        .filter(|r| wanted(r, "rule", rule) && wanted(r, "outcome", outcome))
    {
        if json {
            out.push_str(&format!("{record}\n"));
        } else {
            let field = |name: &str| record[name].as_str().unwrap_or("-").to_string();
            out.push_str(&format!(
                "{} {} {} {}\n",
                record["time"],
                field("rule"),
                field("key"),
                field("outcome")
            ));
        }
    }
    Ok(out)
}
