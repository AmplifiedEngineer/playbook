// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook effort suggest`: stage 1 of ADR-0022. A read only report of the
//! agents where a change to effort looks safe or needed, with the evidence.
//!
//! Permission to lower effort comes from the quality floor table alone
//! (`floor.rs`). Usage evidence decides whether a permitted change is worth
//! showing and whether to hold it back. It never grants permission, and it
//! never writes: the store is opened read only, and nothing here touches
//! memory, config or Claude Code settings.
//!
//! A usage row does not say which agent produced it, so cost is shown per
//! model family and effort bucket, shared with the main session and with every
//! other agent on that pair. Dispatches come from the `Agent` tool calls.

use super::component::{components, resolve, Kind, Resolution};
use super::floor::{floor_of, Floor};
use super::rank;
use crate::usage::aggregate::SECONDS_PER_DAY;
use crate::usage::query::{self, Range};
use crate::usage::{db, ToolInvocationEvent, ToolKind, UsageEvent};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

/// Dispatches of an agent in the window before a proposal is worth showing.
pub const MIN_DISPATCHES: usize = 20;
/// Distinct sessions those dispatches must span, so one busy review does not count.
pub const MIN_SESSIONS: usize = 5;
/// Days of history in the store before anything is proposed (cold start).
pub const MIN_HISTORY_DAYS: i64 = 7;
/// Share of dispatches that are reruns, or overrides upward, that holds a change back.
pub const FLAG_SHARE: f64 = 0.25;
/// Messages a cost bucket needs before its mean is shown as a comparison.
pub const MIN_BUCKET_MESSAGES: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Nothing to change.
    NoChange,
    /// Lowering is permitted by the floor and the evidence does not hold it back.
    Lower,
    /// Something needs a look: a ceiling below the floor, or many escalations.
    Needed,
    /// Not enough history or dispatches to say.
    Insufficient,
    /// Not dispatched in the window.
    Unused,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::NoChange => "no-change",
            Status::Lower => "lower",
            Status::Needed => "needed",
            Status::Insufficient => "insufficient-data",
            Status::Unused => "unused",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    /// `needed` changes the status, `info` only adds context.
    pub level: &'static str,
    pub kind: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Bucket {
    pub effort: String,
    pub messages: usize,
    pub cost_usd: f64,
}

impl Bucket {
    fn mean(&self) -> f64 {
        self.cost_usd / self.messages.max(1) as f64
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentReport {
    pub name: String,
    pub model: String,
    pub shipped: String,
    pub effective: String,
    pub configured: String,
    pub floor: Option<&'static Floor>,
    pub dispatches: usize,
    pub sessions: usize,
    pub by_level: BTreeMap<String, usize>,
    pub reruns: usize,
    pub overrides_down: usize,
    pub overrides_up: usize,
    pub buckets: Vec<Bucket>,
    pub status: Status,
    pub findings: Vec<Finding>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub range: &'static str,
    pub history_days: i64,
    pub cold_start: bool,
    pub other_dispatches: usize,
    pub agents: Vec<AgentReport>,
}

/// The effort tiers an older release could name in a variant agent
/// (`reviewer-low`). New dispatches name the base agent, so these only matter
/// for dispatches already in the usage history.
const TIERS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// The agent a dispatch name refers to and the effort tier it names, if any.
/// `playbook:reviewer` and `reviewer` are the base, `reviewer-low` and
/// `playbook-variants:reviewer-xhigh` are variants. Any other name is `None`.
pub fn parse_dispatch<'a>(
    name: &str,
    bases: &[&'a str],
) -> Option<(&'a str, Option<&'static str>)> {
    let name = name.rsplit(':').next().unwrap_or(name);
    if let Some(base) = bases.iter().find(|b| **b == name) {
        return Some((base, None));
    }
    for tier in TIERS {
        if let Some(stem) = name.strip_suffix(&format!("-{tier}")) {
            if let Some(base) = bases.iter().find(|b| **b == stem) {
                return Some((base, Some(tier)));
            }
        }
    }
    None
}

fn family(model: &str) -> Option<&'static str> {
    ["haiku", "sonnet", "opus"]
        .into_iter()
        .find(|f| model.contains(f))
}

fn bucket(usage: &[UsageEvent], fam: &str, effort: &str) -> Bucket {
    let rows: Vec<&UsageEvent> = usage
        .iter()
        .filter(|e| e.model.contains(fam) && e.effort == effort)
        .collect();
    Bucket {
        effort: effort.to_string(),
        messages: rows.len(),
        cost_usd: rows.iter().map(|e| e.cost_usd).sum(),
    }
}

fn rank_of(level: &str) -> usize {
    rank(level).unwrap_or(usize::MAX)
}

/// Builds the report from the resolved agents, the usage rows and the tool
/// events of the window, and the timestamp of the first event in the whole
/// store (for the cold start check). Pure: `now` is the clock.
pub fn analyze(
    agents: &[Resolution],
    usage: &[UsageEvent],
    tools: &[ToolInvocationEvent],
    first_event: Option<i64>,
    now: i64,
    range: Range,
) -> Report {
    let window = range.days(now);
    let bases: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
    let history_days = first_event.map_or(0, |first| (now - first).div_euclid(SECONDS_PER_DAY));
    let cold_start = history_days < MIN_HISTORY_DAYS;

    // agent -> (level -> count), agent -> (session -> count)
    let mut levels: HashMap<&str, BTreeMap<String, usize>> = HashMap::new();
    let mut sessions: HashMap<&str, HashMap<&str, usize>> = HashMap::new();
    let mut other = 0;
    for t in tools
        .iter()
        .filter(|t| t.kind == ToolKind::Agent && query::in_window(window, t.timestamp))
    {
        let Some((base, tier)) = parse_dispatch(&t.name, &bases) else {
            other += 1;
            continue;
        };
        let shipped = agents
            .iter()
            .find(|a| a.name == base)
            .and_then(|a| a.shipped.as_deref())
            .unwrap_or("");
        let level = tier.unwrap_or(shipped).to_string();
        *levels.entry(base).or_default().entry(level).or_default() += 1;
        *sessions
            .entry(base)
            .or_default()
            .entry(t.session_id.as_str())
            .or_default() += 1;
    }

    let mut out = Vec::new();
    for a in agents {
        let shipped = a.shipped.clone().unwrap_or_default();
        let effective = a.effective.clone().unwrap_or_else(|| shipped.clone());
        let floor = floor_of(&a.name);
        let by_level = levels.remove(a.name.as_str()).unwrap_or_default();
        let per_session = sessions.remove(a.name.as_str()).unwrap_or_default();
        let dispatches: usize = by_level.values().sum();
        let fan_out = floor.is_none_or(|f| f.fan_out);
        let reruns = if fan_out {
            0
        } else {
            per_session.values().map(|n| n - 1).sum()
        };
        let overrides_down = by_level
            .iter()
            .filter(|(l, _)| rank_of(l) < rank_of(&shipped))
            .map(|(_, n)| n)
            .sum();
        let overrides_up = by_level
            .iter()
            .filter(|(l, _)| rank_of(l) > rank_of(&shipped) && rank(l).is_some())
            .map(|(_, n)| n)
            .sum();

        let fam = a.model.as_deref().and_then(family);
        let mut wanted: BTreeSet<String> = by_level.keys().cloned().collect();
        wanted.insert(shipped.clone());
        if let Some(f) = floor {
            wanted.insert(f.floor.to_string());
        }
        // Rows with no recorded effort are their own bucket.
        wanted.insert(String::new());
        let mut buckets: Vec<Bucket> = fam
            .map(|f| wanted.iter().map(|e| bucket(usage, f, e)).collect())
            .unwrap_or_default();
        buckets.retain(|b| b.messages > 0 || !b.effort.is_empty());
        buckets.sort_by_key(|b| rank_of(&b.effort));

        let mut report = AgentReport {
            name: a.name.clone(),
            model: a.model.clone().unwrap_or_default(),
            shipped,
            effective,
            configured: a.configured.clone(),
            floor,
            dispatches,
            sessions: per_session.len(),
            by_level,
            reruns,
            overrides_down,
            overrides_up,
            buckets,
            status: Status::NoChange,
            findings: Vec::new(),
        };
        judge(&mut report, cold_start);
        out.push(report);
    }
    out.sort_by_key(|r| {
        (
            match r.status {
                Status::Needed => 0,
                Status::Lower => 1,
                Status::NoChange => 2,
                Status::Insufficient => 3,
                Status::Unused => 4,
            },
            r.name.clone(),
        )
    });
    Report {
        range: range.key(),
        history_days,
        cold_start,
        other_dispatches: other,
        agents: out,
    }
}

/// The cost comparison sentence for a lowering: the mean cost per message in
/// the shared bucket at the effective level and at the floor, or a note that
/// the store has too few messages to compare. Starts with `. `.
pub(crate) fn cost_note(r: &AgentReport) -> String {
    let Some(floor) = r.floor else {
        return String::new();
    };
    let cost = |level: &str| {
        r.buckets
            .iter()
            .find(|b| b.effort == level && b.messages >= MIN_BUCKET_MESSAGES)
    };
    if let (Some(hi), Some(lo)) = (cost(&r.effective), cost(floor.floor)) {
        format!(
            ". Mean cost per message in the shared {} bucket: ${:.4} at {}, ${:.4} at {}",
            r.model,
            hi.mean(),
            hi.effort,
            lo.mean(),
            lo.effort
        )
    } else {
        ". The store has too few messages at one of the two levels to compare cost".to_string()
    }
}

fn judge(r: &mut AgentReport, cold_start: bool) {
    let Some(floor) = r.floor else {
        r.findings.push(Finding {
            level: "info",
            kind: "no-floor",
            message: "no row in the quality floor table, so nothing is ever proposed".into(),
        });
        return;
    };
    // A ceiling below the floor depends on config only, so it is reported
    // whatever the history says.
    let below = rank_of(&r.effective) < rank_of(floor.floor);
    if below {
        r.findings.push(Finding {
            level: "needed",
            kind: "ceiling-below-floor",
            message: format!(
                "a ceiling holds it at {} and its quality floor is {}{}; raise or remove the ceiling (effort.agents.{} is {}, see `playbook effort list`)",
                r.effective,
                floor.floor,
                if floor.costly_to_miss { ", and a missed finding is costly here" } else { "" },
                r.name,
                r.configured,
            ),
        });
    }
    if r.dispatches > 0 && r.overrides_down > 0 && floor.costly_to_miss {
        r.findings.push(Finding {
            level: "info",
            kind: "lower-variant-on-costly",
            message: format!(
                "{} of {} dispatches used a lower variant than the shipped {}; the shipped routing allows this for small diffs, so it is not counted as a saving",
                r.overrides_down, r.dispatches, r.shipped
            ),
        });
    }
    let enough = !cold_start && r.dispatches >= MIN_DISPATCHES && r.sessions >= MIN_SESSIONS;
    if r.dispatches == 0 {
        r.status = if below {
            Status::Needed
        } else {
            Status::Unused
        };
        return;
    }
    if !enough {
        r.findings.push(Finding {
            level: "info",
            kind: "insufficient-data",
            message: if cold_start {
                format!("less than {MIN_HISTORY_DAYS} days of history in the store, so the defaults stand")
            } else {
                format!(
                    "{} dispatches over {} sessions; a proposal needs {MIN_DISPATCHES} over {MIN_SESSIONS} sessions",
                    r.dispatches, r.sessions
                )
            },
        });
        r.status = if below {
            Status::Needed
        } else {
            Status::Insufficient
        };
        return;
    }
    let share = |n: usize| n as f64 / r.dispatches as f64;
    let mut held = false;
    if share(r.overrides_up) >= FLAG_SHARE {
        held = true;
        r.findings.push(Finding {
            level: "needed",
            kind: "escalated",
            message: format!(
                "{} of {} dispatches used a higher variant than the shipped {}; if that suits your work, the shipped effort may be too low, so check it with `playbook eval bench` before any change",
                r.overrides_up, r.dispatches, r.shipped
            ),
        });
    }
    if share(r.reruns) >= FLAG_SHARE {
        held = true;
        r.findings.push(Finding {
            level: "info",
            kind: "reruns",
            message: format!(
                "{} of {} dispatches were repeats inside a session. A deliberate repeat (a second gate, another review) looks the same as a retry, so this holds a lowering back and never proposes a change on its own",
                r.reruns, r.dispatches
            ),
        });
    }
    let permitted = !floor.costly_to_miss
        && rank_of(floor.floor) < rank_of(&r.shipped)
        && rank_of(&r.effective) > rank_of(floor.floor);
    if permitted && !held {
        let mut text = format!(
            "may run at its floor {} instead of {} ({}). To apply: playbook config set --global effort.agents.{} {} (applies to every project)",
            floor.floor, r.effective, floor.source, r.name, floor.floor
        );
        text.push_str(&cost_note(r));
        r.findings.push(Finding {
            level: "lower",
            kind: "lower",
            message: text,
        });
    } else if permitted {
        r.findings.push(Finding {
            level: "info",
            kind: "lower-held",
            message: format!(
                "the floor permits {}, held back by the signals above",
                floor.floor
            ),
        });
    }
    r.status = if r.findings.iter().any(|f| f.level == "needed") {
        Status::Needed
    } else if r.findings.iter().any(|f| f.level == "lower") {
        Status::Lower
    } else {
        Status::NoChange
    };
}

fn finding_json(f: &Finding) -> Value {
    json!({ "level": f.level, "kind": f.kind, "message": f.message })
}

impl Report {
    pub fn to_json(&self) -> Value {
        json!({
            "range": self.range,
            "historyDays": self.history_days,
            "coldStart": self.cold_start,
            "thresholds": {
                "minDispatches": MIN_DISPATCHES,
                "minSessions": MIN_SESSIONS,
                "minHistoryDays": MIN_HISTORY_DAYS,
                "flagShare": FLAG_SHARE,
                "minBucketMessages": MIN_BUCKET_MESSAGES,
            },
            "floorTable": super::floor::DOC_PATH,
            "otherDispatches": self.other_dispatches,
            "costNote": "cost is per model family and effort bucket, shared with the main session and other agents",
            "agents": self.agents.iter().map(|a| json!({
                "name": a.name,
                "model": a.model,
                "shipped": a.shipped,
                "effective": a.effective,
                "configured": a.configured,
                "floor": a.floor.map(|f| f.floor),
                "costlyToMiss": a.floor.map(|f| f.costly_to_miss),
                "floorSource": a.floor.map(|f| f.source),
                "status": a.status.as_str(),
                "dispatches": a.dispatches,
                "sessions": a.sessions,
                "dispatchesByLevel": a.by_level,
                "reruns": a.reruns,
                "overridesDown": a.overrides_down,
                "overridesUp": a.overrides_up,
                "buckets": a.buckets.iter().map(|b| json!({
                    "effort": if b.effort.is_empty() { "unknown" } else { b.effort.as_str() },
                    "messages": b.messages,
                    "costUsd": (b.cost_usd * 10000.0).round() / 10000.0,
                    "meanCostUsd": if b.messages == 0 { Value::Null } else { json!((b.mean() * 1e6).round() / 1e6) },
                })).collect::<Vec<_>>(),
                "findings": a.findings.iter().map(finding_json).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })
    }

    pub fn to_text(&self) -> String {
        let mut out = format!(
            "playbook effort suggest: range {}, {} days of history, read only\n",
            self.range, self.history_days
        );
        out.push_str("agent           model   shipped  effective  floor    dispatches  status\n");
        for a in &self.agents {
            out.push_str(&format!(
                "{:<15} {:<7} {:<8} {:<10} {:<8} {:<11} {}\n",
                a.name,
                a.model,
                a.shipped,
                a.effective,
                a.floor.map_or("-", |f| f.floor),
                a.dispatches,
                a.status.as_str()
            ));
        }
        for a in self.agents.iter().filter(|a| !a.findings.is_empty()) {
            out.push_str(&format!("\n{}\n", a.name));
            for f in &a.findings {
                out.push_str(&format!("  {}: {}\n", f.kind, f.message));
            }
            let levels: Vec<String> = a
                .by_level
                .iter()
                .map(|(l, n)| format!("{l} x{n}"))
                .collect();
            if !levels.is_empty() {
                out.push_str(&format!("  dispatched at: {}\n", levels.join(", ")));
            }
            for b in a.buckets.iter().filter(|b| b.messages > 0) {
                out.push_str(&format!(
                    "  {} bucket at {}: {} messages, ${:.2} total, ${:.4} per message\n",
                    a.model,
                    if b.effort.is_empty() {
                        "unknown"
                    } else {
                        &b.effort
                    },
                    b.messages,
                    b.cost_usd,
                    b.mean()
                ));
            }
        }
        if self.cold_start {
            out.push_str(&format!(
                "\nCold start: less than {MIN_HISTORY_DAYS} days of history, so no change is proposed.\n"
            ));
        }
        out.push_str("\nCost buckets are shared with the main session and other agents. Floors come from the benchmark (src/effort/floor.rs). Nothing was written.");
        out
    }
}

/// The reason no report can be made, or the loaded inputs.
pub fn run(
    home: &Path,
    claude: Option<&str>,
    root: Option<&Path>,
    db_path: &Path,
    range: Range,
    now: i64,
) -> Result<Report, String> {
    let root =
        root.ok_or("no plugin root found: run `playbook path plugin-root` to see what was tried")?;
    if !db_path.is_file() {
        return Err(format!(
            "no usage store at {}: run `playbook usage --summary` once to create it",
            db_path.display()
        ));
    }
    let conn = playbook_store::Connection::open_with_flags(
        db_path,
        playbook_store::OpenFlags::SQLITE_OPEN_READ_ONLY
            | playbook_store::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("failed to open the usage store read only: {e}"))?;
    conn.busy_timeout(std::time::Duration::from_millis(2000))
        .map_err(|e| format!("failed to set busy_timeout: {e}"))?;
    let usage = query::load_window(&conn, range, now)?;
    let tools = db::load_tool_events(&conn)?;
    let first_event: Option<i64> = conn
        .query_row("SELECT MIN(timestamp) FROM usage_events", [], |r| r.get(0))
        .map_err(|e| format!("failed to read the usage store: {e}"))?;
    let agents: Vec<Resolution> = components(root)
        .into_iter()
        .filter(|(k, _)| *k == Kind::Agent)
        .map(|(k, name)| resolve(k, &name, home, claude, Some(root)))
        .collect();
    Ok(analyze(&agents, &usage, &tools, first_event, now, range))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASES: [&str; 3] = ["reviewer", "test-reviewer", "git"];

    #[test]
    fn dispatch_names_map_to_an_agent_and_a_tier() {
        assert_eq!(
            parse_dispatch("playbook:reviewer", &BASES),
            Some(("reviewer", None))
        );
        assert_eq!(parse_dispatch("reviewer", &BASES), Some(("reviewer", None)));
        assert_eq!(
            parse_dispatch("playbook-variants:reviewer-xhigh", &BASES),
            Some(("reviewer", Some("xhigh")))
        );
        assert_eq!(
            parse_dispatch("git-low", &BASES),
            Some(("git", Some("low")))
        );
        // A longer agent name is not mistaken for a variant of a shorter one.
        assert_eq!(
            parse_dispatch("test-reviewer-low", &BASES),
            Some(("test-reviewer", Some("low")))
        );
    }

    #[test]
    fn other_names_map_to_nothing() {
        assert_eq!(parse_dispatch("general-purpose", &BASES), None);
        assert_eq!(parse_dispatch("Draft WU-1 brief file", &BASES), None);
        assert_eq!(parse_dispatch("reviewer-ultra", &BASES), None);
    }

    #[test]
    fn model_names_map_to_a_family() {
        assert_eq!(family("claude-opus-5-5"), Some("opus"));
        assert_eq!(family("claude-haiku-4-5-20251001"), Some("haiku"));
        assert_eq!(family("gpt-x"), None);
    }
}
