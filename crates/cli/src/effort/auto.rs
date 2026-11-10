// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Stage 3 of ADR-0022: an optional choice of effort for one dispatch.
//!
//! The config key `effort.auto` is `off` (the default), `suggest` or `on`.
//! `off` does nothing at all. `suggest` works out the choice and logs it, and
//! the dispatch still uses the default. `on` also uses the choice.
//!
//! A choice is a level, which the orchestrator passes as the `effort`
//! parameter of the `Agent` call on the base agent. The candidates are the
//! levels at or under every ceiling in play (Claude Code's, playbook's, the
//! component key and the model cap), so a choice can never exceed a ceiling. A level below the default must also be
//! at or above the agent's quality floor (`floor.rs`), and an agent marked
//! costly to miss is never lowered, only raised.
//!
//! Every failure, a bad diff shape, an unreadable store, a panic, gives the
//! default. Each automatic choice is appended to `effort/auto.log` under the
//! playbook root. Nothing leaves the machine and nothing outside playbook's
//! own files is written.

use super::component::Resolution;
use super::floor::floor_of;
use super::{rank, LEVELS};
use crate::common::atomic::atomic_append;
use crate::common::time::iso_utc;
use crate::config;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The config key.
pub const KEY: &str = "effort.auto";

/// Past this many changed lines, files or bytes a diff is large enough to hide
/// a miss, so effort is raised. The byte limit is `MAX_DIFF_BYTES` in
/// `src/pr/triage.rs`.
pub const BIG_LINES: u64 = 1500;
pub const BIG_FILES: u64 = 40;
pub const BIG_BYTES: u64 = 60 * 1024;
/// At most this many lines and files is a small diff (the `dedup` trigger in
/// `/playbook:review --deep` is 300 lines).
pub const SMALL_LINES: u64 = 300;
pub const SMALL_FILES: u64 = 10;
/// The level a raise aims for, capped by what is allowed.
const RAISE_TO: &str = "high";
/// The level a lowering aims for, held at the floor.
const LOWER_TO: &str = "low";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Off,
    Suggest,
    On,
}

impl Mode {
    pub fn parse(text: &str) -> Option<Mode> {
        match text {
            "off" => Some(Mode::Off),
            "suggest" => Some(Mode::Suggest),
            "on" => Some(Mode::On),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Suggest => "suggest",
            Mode::On => "on",
        }
    }
}

/// The configured mode. Anything unreadable or unknown is `off`.
pub fn mode(home: &Path) -> Mode {
    match config::resolve_valid(KEY, home, None) {
        Ok((Value::String(s), _, _)) => Mode::parse(&s).unwrap_or(Mode::Off),
        _ => Mode::Off,
    }
}

/// What the diff looks like.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Shape {
    pub lines: u64,
    pub files: u64,
    pub bytes: Option<u64>,
    pub paths: Vec<String>,
}

impl Shape {
    /// Parse `git diff --numstat` output: `added<TAB>deleted<TAB>path`, with
    /// `-` counts for a binary file. A malformed row is an error, so a
    /// garbled input falls back to the default instead of a guess.
    pub fn from_numstat(text: &str) -> Result<Shape, String> {
        let mut shape = Shape::default();
        for row in text.lines().filter(|l| !l.trim().is_empty()) {
            let mut cols = row.splitn(3, '\t');
            let (Some(add), Some(del), Some(path)) = (cols.next(), cols.next(), cols.next()) else {
                return Err(format!("malformed numstat row: {row}"));
            };
            let count = |c: &str| -> Result<u64, String> {
                if c == "-" {
                    Ok(0)
                } else {
                    c.parse()
                        .map_err(|_| format!("malformed numstat row: {row}"))
                }
            };
            shape.lines = shape
                .lines
                .saturating_add(count(add)?)
                .saturating_add(count(del)?);
            shape.files += 1;
            shape.paths.push(path.to_string());
        }
        Ok(shape)
    }

    /// The languages of the touched files, by extension, sorted.
    pub fn languages(&self) -> Vec<String> {
        let set: BTreeSet<String> = self
            .paths
            .iter()
            .filter_map(|p| language(p))
            .map(str::to_string)
            .collect();
        set.into_iter().collect()
    }
}

fn language(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => "rust",
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "go" => "go",
        "java" | "kt" => "jvm",
        "sh" | "bash" | "zsh" => "shell",
        "sql" => "sql",
        "yml" | "yaml" | "toml" | "json" => "config",
        "md" => "docs",
        _ => return None,
    })
}

/// Path words that mark a security sensitive area.
const RISK_WORDS: [&str; 10] = [
    "auth",
    "secret",
    "crypto",
    "token",
    "password",
    "credential",
    "security",
    "permission",
    "migration",
    ".env",
];
/// Words in a memory fact's name or description that mark a risky area.
const FACT_WORDS: [&str; 8] = [
    "security",
    "secret",
    "hot path",
    "hot file",
    "flaky",
    "incident",
    "sensitive",
    "regression",
];

/// The risk class of a diff and the reasons for it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Risk {
    pub elevated: bool,
    pub reasons: Vec<String>,
}

fn path_risk(path: &str) -> Option<String> {
    let lower = path.to_ascii_lowercase();
    if lower.starts_with(".github/workflows/") {
        return Some(format!("{path} is a CI workflow"));
    }
    if lower.ends_with(".sql") || lower.ends_with("dockerfile") {
        return Some(format!("{path} changes data or build infrastructure"));
    }
    RISK_WORDS
        .iter()
        .find(|w| lower.contains(*w))
        .map(|w| format!("{path} looks security sensitive ({w})"))
}

/// Facts in the memory graph that are anchored to a touched path and whose
/// name or description names a risk. A missing or unreadable graph gives
/// none, since a missing fact says nothing.
pub fn memory_risks(graph: &Value, paths: &[String]) -> Vec<String> {
    let nodes = graph.get("nodes").and_then(Value::as_array);
    let edges = graph.get("edges").and_then(Value::as_array);
    let (Some(nodes), Some(edges)) = (nodes, edges) else {
        return Vec::new();
    };
    let text_of = |id: &str| -> Option<(String, String)> {
        let n = nodes
            .iter()
            .find(|n| n.get("id").and_then(Value::as_str) == Some(id))?;
        let name = n.get("name").and_then(Value::as_str).unwrap_or(id);
        let desc = n.get("description").and_then(Value::as_str).unwrap_or("");
        Some((
            name.to_string(),
            format!("{name} {desc}").to_ascii_lowercase(),
        ))
    };
    let file_of = |id: &str| -> Option<String> {
        let n = nodes
            .iter()
            .find(|n| n.get("id").and_then(Value::as_str) == Some(id))?;
        let f = n.get("file").and_then(Value::as_str)?;
        Some(f.split('#').next().unwrap_or(f).to_string())
    };
    let mut out = BTreeSet::new();
    for e in edges
        .iter()
        .filter(|e| e.get("relation").and_then(Value::as_str) == Some("anchors"))
    {
        let (Some(from), Some(to)) = (
            e.get("from").and_then(Value::as_str),
            e.get("to").and_then(Value::as_str),
        ) else {
            continue;
        };
        let Some(anchor) = file_of(to).filter(|a| !a.is_empty()) else {
            continue;
        };
        let hit = paths
            .iter()
            .any(|p| p == &anchor || p.starts_with(&format!("{}/", anchor.trim_end_matches('/'))));
        if !hit {
            continue;
        }
        if let Some((name, text)) = text_of(from) {
            if let Some(w) = FACT_WORDS.iter().find(|w| text.contains(*w)) {
                out.insert(format!(
                    "memory fact {name} anchors a touched path and names {w}"
                ));
            }
        }
    }
    out.into_iter().collect()
}

/// The risk of `shape`, from path patterns and, when given, memory facts.
pub fn risk(shape: &Shape, graph: Option<&Value>) -> Risk {
    let mut reasons: Vec<String> = shape.paths.iter().filter_map(|p| path_risk(p)).collect();
    reasons.truncate(3);
    if let Some(g) = graph {
        reasons.extend(memory_risks(g, &shape.paths).into_iter().take(3));
    }
    Risk {
        elevated: !reasons.is_empty(),
        reasons,
    }
}

/// What the stage 1 report says about an agent: whether it holds a lowering
/// back. `None` stands for evidence that could not be read, and then nothing
/// is lowered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub hold_lowering: bool,
    pub note: String,
}

/// Read the stage 1 evidence for `agent` from a report. Lowering is held back
/// unless the report has enough history for the agent and shows no escalation
/// or repeat signal. An agent missing from the report is an error.
pub fn evidence_from(report: &super::suggest::Report, agent: &str) -> Result<Evidence, String> {
    use super::suggest::Status;
    let a = report
        .agents
        .iter()
        .find(|a| a.name == agent)
        .ok_or_else(|| format!("{agent} is not in the usage report"))?;
    let flagged = a
        .findings
        .iter()
        .find(|f| f.kind == "escalated" || f.kind == "reruns");
    let (hold, note) = match (a.status, flagged) {
        (_, Some(f)) => (true, f.kind.to_string()),
        (Status::Insufficient | Status::Unused | Status::Needed, _) => {
            (true, format!("status {}", a.status.as_str()))
        }
        _ => (false, String::new()),
    };
    Ok(Evidence {
        hold_lowering: hold,
        note,
    })
}

/// The levels the agent may run at: every level up to the lowest ceiling.
fn candidates(res: &Resolution) -> Vec<String> {
    let limit = res.ceiling.as_deref().and_then(rank);
    LEVELS
        .iter()
        .skip(1)
        .filter(|l| limit.is_none_or(|m| rank(l).is_some_and(|r| r <= m)))
        .map(|l| (*l).to_string())
        .collect()
}

/// The decision for one dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub level: String,
    pub reason: String,
}

/// Pick a level for one dispatch. Pure. The default is what `resolve`
/// returns. The candidates are only the allowed files, so a ceiling is never
/// passed, a lower one is never below the floor, and a costly to miss agent
/// is never lowered.
pub fn decide(
    res: &Resolution,
    shape: &Shape,
    risk: &Risk,
    lens: Option<&str>,
    evidence: Option<&Evidence>,
) -> Decision {
    let default_level = default_of(res);
    let keep = |why: &str| Decision {
        level: default_level.clone(),
        reason: why.to_string(),
    };
    let Some(default_rank) = rank(&default_level) else {
        return keep("no default level to compare with, so the default stands");
    };
    let cands = candidates(res);
    let pick = |want: &dyn Fn(usize) -> bool, highest: bool| -> Option<&String> {
        let mut found: Vec<&String> = cands.iter().filter(|c| rank(c).is_some_and(want)).collect();
        found.sort_by_key(|c| rank(c));
        if highest {
            found.last().copied()
        } else {
            found.first().copied()
        }
    };
    let bytes = shape.bytes.unwrap_or(0);
    let big = shape.lines > BIG_LINES || shape.files > BIG_FILES || bytes > BIG_BYTES;
    let security_lens = lens == Some("security");
    if risk.elevated || big || security_lens {
        let aim = rank(RAISE_TO).unwrap_or(default_rank);
        let why = if risk.elevated {
            risk.reasons.first().cloned().unwrap_or_default()
        } else if security_lens {
            "the review lens is security".to_string()
        } else {
            format!(
                "a large diff ({} changed lines in {} files)",
                shape.lines, shape.files
            )
        };
        // Highest allowed level up to the aim, never below the default.
        return match pick(&|r| r >= default_rank && r <= aim, true) {
            Some(c) if rank(c) > Some(default_rank) => Decision {
                level: c.clone(),
                reason: format!("raised from {default_level} to {c}: {why}"),
            },
            _ => keep(&format!(
                "{why}, but no higher level is allowed under the ceilings, so the default stands"
            )),
        };
    }
    let small = shape.lines <= SMALL_LINES && shape.files <= SMALL_FILES;
    if !small {
        return keep("a medium diff, so the default stands");
    }
    let Some(floor) = floor_of(&res.name) else {
        return keep("no row in the quality floor table, so nothing is lowered");
    };
    if floor.costly_to_miss {
        return keep("a missed finding is costly here, so it is raised for risk and never lowered");
    }
    let Some(ev) = evidence else {
        return keep("the usage evidence could not be read, so nothing is lowered");
    };
    if ev.hold_lowering {
        return keep(&format!("held back by usage evidence: {}", ev.note));
    }
    let lowest = rank(LOWER_TO)
        .unwrap_or(0)
        .max(rank(floor.floor).unwrap_or(default_rank));
    match pick(&|r| r >= lowest && r < default_rank, false) {
        Some(c) => Decision {
            level: c.clone(),
            reason: format!(
                "lowered from {default_level} to {c}: a small diff ({} changed lines in {} files) with no risk signal, and {c} is not below the floor {}",
                shape.lines, shape.files, floor.floor
            ),
        },
        None => keep("a small diff, but no lower level is allowed at or above the floor"),
    }
}

/// The result of a `choose` call.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub agent: String,
    pub mode: Mode,
    pub default_level: String,
    pub chosen_level: String,
    /// The `subagent_type` to spawn: always the base agent.
    pub dispatch: String,
    /// The level to pass as the `Agent` tool `effort` parameter: the choice
    /// when `on`, else the default. `None` when the agent has no known effort.
    pub effort: Option<String>,
    pub deviates: bool,
    pub applied: bool,
    pub reason: String,
    pub note: Option<String>,
    pub inputs: Value,
}

impl Choice {
    pub fn to_json(&self) -> Value {
        json!({
            "agent": self.agent,
            "mode": self.mode.as_str(),
            "default": { "level": self.default_level },
            "chosen": { "level": self.chosen_level },
            "dispatch": self.dispatch,
            "effort": self.effort,
            "deviates": self.deviates,
            "applied": self.applied,
            "reason": self.reason,
            "note": self.note,
            "inputs": self.inputs,
        })
    }
}

/// The level without a choice: the shipped effort lowered by every ceiling.
fn default_of(res: &Resolution) -> String {
    res.effective.clone().unwrap_or_default()
}

/// The `subagent_type` to spawn: always the base agent.
fn base_type(res: &Resolution) -> String {
    format!("playbook:{}", res.name)
}

/// The append only log of automatic choices.
pub fn log_path(home: &Path) -> PathBuf {
    super::patch::dir(home).join("auto.log")
}

/// Choose an effort for one dispatch of `res`. With `off` nothing is read or
/// written. On any failure, including a panic in the decision, the default is
/// returned and the failure is the reason. `shape` is the result of reading
/// the diff, so a read error is handled here. `evidence` is read only when a
/// lowering is on the table.
#[allow(clippy::too_many_arguments)]
pub fn choose(
    home: &Path,
    res: &Resolution,
    shape: Result<Shape, String>,
    graph: Option<&Value>,
    lens: Option<&str>,
    evidence: &dyn Fn() -> Result<Evidence, String>,
    now: i64,
) -> Choice {
    let mode = mode(home);
    let default_level = default_of(res);
    let base = |reason: String, inputs: Value| Choice {
        agent: res.name.clone(),
        mode,
        default_level: default_level.clone(),
        chosen_level: default_level.clone(),
        dispatch: base_type(res),
        effort: Some(default_level.clone()).filter(|l| !l.is_empty()),
        deviates: false,
        applied: false,
        reason,
        note: None,
        inputs,
    };
    if mode == Mode::Off {
        return base(
            "effort.auto is off, so the default stands".to_string(),
            Value::Null,
        );
    }
    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || -> Result<Choice, String> {
            let shape = shape?;
            let r = risk(&shape, graph);
            // The usage store is read only when a lowering is on the table.
            let lowering = !r.elevated
                && lens != Some("security")
                && shape.lines <= SMALL_LINES
                && shape.files <= SMALL_FILES
                && shape.bytes.unwrap_or(0) <= BIG_BYTES;
            let ev = if lowering {
                evidence()
            } else {
                Err("not needed for this diff".to_string())
            };
            let (ev_ref, ev_note) = match &ev {
                Ok(e) => (
                    Some(e),
                    if e.hold_lowering {
                        "holds lowering"
                    } else {
                        "ok"
                    }
                    .to_string(),
                ),
                Err(e) => (None, format!("not used: {e}")),
            };
            let d = decide(res, &shape, &r, lens, ev_ref);
            let inputs = json!({
                "lines": shape.lines,
                "files": shape.files,
                "bytes": shape.bytes,
                "languages": shape.languages(),
                "risk": r.reasons,
                "lens": lens,
                "evidence": ev_note,
            });
            let deviates = d.level != default_level;
            let mut c = base(d.reason.clone(), inputs);
            c.chosen_level = d.level.clone();
            c.deviates = deviates;
            c.applied = mode == Mode::On && deviates;
            if mode == Mode::On {
                c.effort = Some(d.level.clone());
            }
            if deviates {
                c.note = Some(if mode == Mode::On {
                    format!(
                    "playbook effort auto: {} at {} instead of {} ({}). See `playbook effort log`.",
                    res.name, d.level, default_level, d.reason
                )
                } else {
                    format!(
                    "playbook effort auto (suggest only): {} would run at {} instead of {} ({}). Not applied.",
                    res.name, d.level, default_level, d.reason
                )
                });
            }
            Ok(c)
        },
    ));
    let choice = match attempt {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => base(format!("fell back to the default: {e}"), Value::Null),
        Err(_) => base(
            "fell back to the default: the choice failed".to_string(),
            Value::Null,
        ),
    };
    log_choice(home, &choice, now);
    choice
}

fn log_choice(home: &Path, c: &Choice, now: i64) {
    let line = json!({
        "ts": iso_utc(now),
        "component": c.agent,
        "mode": c.mode.as_str(),
        "default": c.default_level,
        "chosen": c.chosen_level,
        "applied": c.applied,
        "inputs": c.inputs,
        "reason": c.reason,
    });
    atomic_append(&log_path(home).to_string_lossy(), &line.to_string());
}

/// The last `limit` log rows, oldest first. A damaged row is skipped.
pub fn read_log(home: &Path, limit: usize) -> Vec<Value> {
    let text = std::fs::read_to_string(log_path(home)).unwrap_or_default();
    let rows: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let skip = rows.len().saturating_sub(limit);
    rows.into_iter().skip(skip).collect()
}

/// `playbook effort log` as text.
pub fn log_text(rows: &[Value]) -> String {
    if rows.is_empty() {
        return "No automatic effort choices logged yet.".to_string();
    }
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("-").to_string();
    rows.iter()
        .map(|r| {
            format!(
                "{} {} {} -> {} ({}{}) {}\n  inputs: {}",
                s(r, "ts"),
                s(r, "component"),
                s(r, "default"),
                s(r, "chosen"),
                s(r, "mode"),
                if r.get("applied") == Some(&Value::Bool(true)) {
                    ", applied"
                } else {
                    ""
                },
                s(r, "reason"),
                r.get("inputs").map_or("-".to_string(), Value::to_string),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;
    use crate::effort::component::Kind;

    fn res(name: &str, shipped: &str, ceiling: Option<&str>) -> Resolution {
        let effective = crate::effort::lower(Some(shipped), ceiling).map(str::to_string);
        Resolution {
            kind: Kind::Agent,
            name: name.to_string(),
            known: true,
            shipped: Some(shipped.to_string()),
            model: Some("sonnet".into()),
            model_cap: None,
            configured: "auto".into(),
            global: "auto".into(),
            claude: None,
            ceiling: ceiling.map(str::to_string),
            effective,
            subagent_type: Some(format!("playbook:{name}")),
        }
    }

    fn small() -> Shape {
        Shape::from_numstat("10\t2\tsrc/a.rs\n3\t0\tsrc/b.rs\n").unwrap()
    }

    fn ok_ev() -> Result<Evidence, String> {
        Ok(Evidence {
            hold_lowering: false,
            note: String::new(),
        })
    }

    fn home(tag: &str) -> PathBuf {
        let h = scratch_dir(tag);
        std::fs::create_dir_all(&h).unwrap();
        h
    }

    fn set_mode(h: &Path, v: &str) {
        crate::config::write::set(crate::config::write::Tier::Global, KEY, &json!(v), h, None)
            .unwrap();
    }

    #[test]
    fn evidence_holds_lowering_without_history_or_with_escalations() {
        use crate::effort::suggest::{AgentReport, Finding, Report, Status};
        let mk = |status, kind: Option<&'static str>| Report {
            range: "30d",
            history_days: 30,
            cold_start: false,
            other_dispatches: 0,
            agents: vec![AgentReport {
                name: "collector".into(),
                model: "haiku".into(),
                shipped: "medium".into(),
                effective: "medium".into(),
                configured: "auto".into(),
                floor: floor_of("collector"),
                dispatches: 30,
                sessions: 8,
                by_level: Default::default(),
                reruns: 0,
                overrides_down: 0,
                overrides_up: 0,
                buckets: vec![],
                status,
                findings: kind
                    .map(|k| Finding {
                        level: "info",
                        kind: k,
                        message: String::new(),
                    })
                    .into_iter()
                    .collect(),
            }],
        };
        assert!(
            !evidence_from(&mk(Status::NoChange, None), "collector")
                .unwrap()
                .hold_lowering
        );
        assert!(
            !evidence_from(&mk(Status::Lower, None), "collector")
                .unwrap()
                .hold_lowering
        );
        assert!(
            evidence_from(&mk(Status::Insufficient, None), "collector")
                .unwrap()
                .hold_lowering
        );
        assert!(
            evidence_from(&mk(Status::NoChange, Some("escalated")), "collector")
                .unwrap()
                .hold_lowering
        );
        assert!(evidence_from(&mk(Status::NoChange, None), "other").is_err());
    }

    #[test]
    fn numstat_gives_size_files_and_languages() {
        let s = small();
        assert_eq!((s.lines, s.files), (15, 2));
        assert_eq!(s.languages(), vec!["rust".to_string()]);
        let b = Shape::from_numstat("-\t-\timg.png\n").unwrap();
        assert_eq!((b.lines, b.files), (0, 1));
    }

    #[test]
    fn bad_numstat_is_an_error() {
        assert!(Shape::from_numstat("not a row").is_err());
        assert!(Shape::from_numstat("x\ty\tfile").is_err());
    }

    #[test]
    fn paths_and_memory_facts_raise_the_risk_class() {
        assert!(
            risk(
                &Shape::from_numstat("1\t1\tsrc/auth/login.rs").unwrap(),
                None
            )
            .elevated
        );
        assert!(
            risk(
                &Shape::from_numstat("1\t1\t.github/workflows/ci.yml").unwrap(),
                None
            )
            .elevated
        );
        assert!(!risk(&small(), None).elevated);
        let graph = json!({
            "nodes": [
                {"id": "g/hot", "name": "hot-path-billing", "description": "a hot path, be careful"},
                {"id": "g/calm", "name": "style-note", "description": "naming"},
                {"id": "code:src/a.rs", "file": "src/a.rs"},
                {"id": "code:src/b.rs", "file": "src/b.rs#L3"},
            ],
            "edges": [
                {"from": "g/hot", "to": "code:src/a.rs", "relation": "anchors"},
                {"from": "g/calm", "to": "code:src/b.rs", "relation": "anchors"},
            ],
        });
        let r = risk(&small(), Some(&graph));
        assert!(r.elevated);
        assert!(r.reasons[0].contains("hot-path-billing"), "{:?}", r.reasons);
        assert_eq!(r.reasons.len(), 1);
        assert!(risk(&small(), Some(&json!("garbage"))).reasons.is_empty());
    }

    #[test]
    fn a_risky_diff_is_raised_but_never_above_the_ceiling() {
        let r = Risk {
            elevated: true,
            reasons: vec!["src/auth.rs looks security sensitive (auth)".into()],
        };
        let open = res("reviewer", "medium", None);
        let d = decide(&open, &small(), &r, None, None);
        assert_eq!(d.level, "high");
        let capped = res("reviewer", "medium", Some("high"));
        let d = decide(&capped, &small(), &r, None, None);
        assert_eq!(d.level, "high", "{}", d.reason);
        let at_default = res("reviewer", "medium", Some("medium"));
        let d = decide(&at_default, &small(), &r, None, None);
        assert_eq!(d.level, "medium");
        assert!(d.reason.contains("no higher level"), "{}", d.reason);
    }

    #[test]
    fn large_diffs_and_the_security_lens_raise() {
        let r = res("reviewer", "medium", None);
        let big = Shape {
            lines: 2000,
            files: 3,
            ..Shape::default()
        };
        assert_eq!(decide(&r, &big, &Risk::default(), None, None).level, "high");
        let heavy = Shape {
            lines: 5,
            files: 1,
            bytes: Some(70_000),
            paths: vec![],
        };
        assert_eq!(
            decide(&r, &heavy, &Risk::default(), None, None).level,
            "high"
        );
        assert_eq!(
            decide(&r, &small(), &Risk::default(), Some("security"), None).level,
            "high"
        );
    }

    #[test]
    fn small_clean_diffs_are_lowered_only_to_the_floor() {
        // analyst: floor medium, so a small diff stays at medium.
        let a = res("analyst", "high", None);
        let ev = ok_ev().unwrap();
        let d = decide(&a, &small(), &Risk::default(), None, Some(&ev));
        assert_eq!(d.level, "medium", "{}", d.reason);
        // cheap-checker: floor low and already low, nothing below.
        let c = res("cheap-checker", "low", None);
        assert_eq!(
            decide(&c, &small(), &Risk::default(), None, Some(&ev)).level,
            "low"
        );
        // collector: floor low, shipped medium, may go to low.
        let col = res("collector", "medium", None);
        assert_eq!(
            decide(&col, &small(), &Risk::default(), None, Some(&ev)).level,
            "low"
        );
    }

    #[test]
    fn a_costly_to_miss_agent_is_never_lowered() {
        let ev = ok_ev().unwrap();
        for name in ["reviewer", "auditor"] {
            let r = res(name, "high", None);
            let d = decide(&r, &small(), &Risk::default(), None, Some(&ev));
            assert_eq!(d.level, "high", "{name}: {}", d.reason);
        }
    }

    #[test]
    fn missing_or_holding_evidence_stops_a_lowering() {
        let col = res("collector", "medium", None);
        assert_eq!(
            decide(&col, &small(), &Risk::default(), None, None).level,
            "medium"
        );
        let hold = Evidence {
            hold_lowering: true,
            note: "escalated often".into(),
        };
        let d = decide(&col, &small(), &Risk::default(), None, Some(&hold));
        assert_eq!(d.level, "medium");
        assert!(d.reason.contains("escalated often"));
    }

    #[test]
    fn an_agent_without_a_floor_row_is_not_lowered() {
        let r = res("mystery", "high", None);
        let ev = ok_ev().unwrap();
        assert_eq!(
            decide(&r, &small(), &Risk::default(), None, Some(&ev)).level,
            "high"
        );
    }

    #[test]
    fn auto_is_off_by_default_and_logs_nothing() {
        let h = home("auto-off");
        assert_eq!(mode(&h), Mode::Off);
        let r = res("reviewer", "medium", None);
        let c = choose(&h, &r, Ok(small()), None, Some("security"), &ok_ev, 100);
        assert!(!c.deviates && !c.applied);
        assert_eq!(c.dispatch, "playbook:reviewer");
        assert!(!log_path(&h).exists());
    }

    #[test]
    fn on_uses_the_choice_logs_it_and_says_so() {
        let h = home("auto-on");
        set_mode(&h, "on");
        let r = res("reviewer", "medium", None);
        let c = choose(
            &h,
            &r,
            Ok(small()),
            None,
            Some("security"),
            &ok_ev,
            1_791_676_800,
        );
        assert!(c.deviates && c.applied);
        assert_eq!(c.dispatch, "playbook:reviewer");
        assert_eq!(c.effort.as_deref(), Some("high"));
        assert!(c
            .note
            .as_deref()
            .unwrap()
            .contains("reviewer at high instead of medium"));
        let rows = read_log(&h, 10);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row["ts"], "2026-10-11T00:00:00Z");
        assert_eq!(row["component"], "reviewer");
        assert_eq!(row["chosen"], "high");
        assert_eq!(row["inputs"]["lines"], 15);
        assert!(row["reason"].as_str().unwrap().contains("security"));
        assert!(log_text(&rows).contains("reviewer medium -> high"));
    }

    #[test]
    fn suggest_logs_but_dispatches_the_default() {
        let h = home("auto-suggest");
        set_mode(&h, "suggest");
        let r = res("reviewer", "medium", None);
        let c = choose(&h, &r, Ok(small()), None, Some("security"), &ok_ev, 100);
        assert!(c.deviates && !c.applied);
        assert_eq!(c.dispatch, "playbook:reviewer");
        assert_eq!(c.chosen_level, "high");
        assert!(c.note.as_deref().unwrap().contains("Not applied"));
        assert_eq!(read_log(&h, 10).len(), 1);
    }

    #[test]
    fn bad_input_falls_back_to_the_default_and_is_logged() {
        let h = home("auto-bad");
        set_mode(&h, "on");
        let r = res("reviewer", "medium", None);
        let c = choose(&h, &r, Err("git failed".into()), None, None, &ok_ev, 100);
        assert!(!c.deviates && !c.applied);
        assert_eq!(c.dispatch, "playbook:reviewer");
        assert!(c.reason.contains("git failed"));
        assert!(read_log(&h, 10)[0]["reason"]
            .as_str()
            .unwrap()
            .contains("git failed"));
    }

    #[test]
    fn a_panic_in_the_decision_falls_back_to_the_default() {
        let h = home("auto-panic");
        set_mode(&h, "on");
        let r = res("collector", "medium", None);
        let boom = || -> Result<Evidence, String> { panic!("store exploded") };
        let c = choose(&h, &r, Ok(small()), None, None, &boom, 100);
        assert_eq!(c.dispatch, "playbook:collector");
        assert!(!c.deviates);
        assert!(c.reason.contains("fell back"));
    }

    #[test]
    fn an_unknown_mode_value_reads_as_off() {
        let h = home("auto-unknown");
        assert!(Mode::parse("always").is_none());
        assert_eq!(Mode::parse("on"), Some(Mode::On));
        assert_eq!(mode(&h), Mode::Off);
    }

    #[test]
    fn a_damaged_log_row_is_skipped_when_reading() {
        let h = home("auto-badlog");
        std::fs::create_dir_all(log_path(&h).parent().unwrap()).unwrap();
        std::fs::write(log_path(&h), "garbage\n{\"component\":\"x\"}\n").unwrap();
        assert_eq!(read_log(&h, 10).len(), 1);
        assert_eq!(read_log(&h, 0).len(), 0);
    }
}
