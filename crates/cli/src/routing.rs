// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook route`: one table that says which model, effort and agent a kind
//! of task goes to, and when the user must approve first.
//!
//! The values come from the Haiku evaluation of 2026-10-09 (1,600 calls):
//! Haiku at medium was the best value for narrow checks, Sonnet needs at most
//! medium for routine coding, and the top model is kept for design and review.
//! The user's effort ceiling always wins over the table.

use crate::config;
use crate::effort;
use serde_json::{json, Value};
use std::path::Path;

/// The task kinds, in the order `playbook route --help` lists them.
const KINDS: [&str; 7] = [
    "mechanical",
    "classify",
    "check",
    "review",
    "write",
    "implement",
    "design",
];

/// The config key that says what happens when a route needs approval.
const ESCALATE_KEY: &str = "routing.escalate";

/// One row of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    pub model: &'static str,
    pub effort: &'static str,
    pub agent: &'static str,
    pub reason: &'static str,
}

/// The route for `kind` at `tier` (`low`, `medium` or `high`). Only
/// `implement` reads the tier. Unknown kinds return `None`.
pub fn table(kind: &str, tier: &str) -> Option<Route> {
    Some(match (kind, tier) {
        ("mechanical", _) => Route {
            model: "haiku",
            effort: "low",
            agent: "collector",
            reason: "an exact, already decided step needs no judgment",
        },
        ("classify", _) => Route {
            model: "haiku",
            effort: "medium",
            agent: "review-triage",
            reason: "Haiku at medium scored best on classification, at the lowest cost",
        },
        ("check", _) => Route {
            model: "haiku",
            effort: "medium",
            agent: "cheap-checker",
            reason: "a narrow concern with a safe fallback",
        },
        ("review", _) => Route {
            model: "opus",
            effort: "medium",
            agent: "reviewer",
            reason: "Opus at medium matched high on the reviewer bench at lower cost",
        },
        ("write", _) => Route {
            model: "haiku",
            effort: "medium",
            agent: "writer",
            reason: "turning decided facts into short text; the orchestrator checks it and fixes it by hand after one resend",
        },
        ("implement", "low") => Route {
            model: "sonnet",
            effort: "low",
            agent: "implementer",
            reason: "a well specified unit, including standard TDD work",
        },
        ("implement", "medium") => Route {
            model: "sonnet",
            effort: "medium",
            agent: "implementer",
            reason: "a cross-module change or an ambiguous spec",
        },
        ("implement", "high") => Route {
            model: "sonnet",
            effort: "high",
            agent: "implementer",
            reason: "workflow determinism, concurrency or subtle architecture",
        },
        ("design", _) => Route {
            model: "opus",
            effort: "xhigh",
            agent: "critic",
            reason: "a weak design costs far more than the extra tokens",
        },
        _ => return None,
    })
}

/// What to do when a route needs approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Escalate {
    Ask,
    Auto,
    Deny,
}

impl Escalate {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ask" => Some(Self::Ask),
            "auto" => Some(Self::Auto),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }
}

/// The configured policy, `ask` when unset or unreadable.
pub fn escalate(home: &Path) -> Escalate {
    match config::resolve(ESCALATE_KEY, home, None) {
        Ok((Value::String(s), _)) => Escalate::parse(&s).unwrap_or(Escalate::Ask),
        _ => Escalate::Ask,
    }
}

/// Why a route needs the user's approval, or `None`. Three triggers: the top
/// model, the high implement tier, and a task that already failed twice.
fn approval_reason(kind: &str, route: &Route, failures: u32) -> Option<&'static str> {
    if route.model == "opus" {
        return Some("runs on the top model");
    }
    if kind == "implement" && route.effort == "high" {
        return Some("uses the high tier");
    }
    if failures >= 2 {
        return Some("has already failed twice");
    }
    None
}

/// The route with the next cheaper choice, used when escalation is denied.
fn downgrade(kind: &str, route: Route) -> Route {
    if route.model == "opus" {
        return Route {
            model: "sonnet",
            effort: "high",
            ..route
        };
    }
    if kind == "implement" && route.effort == "high" {
        return Route {
            effort: "medium",
            ..route
        };
    }
    route
}

/// The decision for one dispatch, as the orchestrator reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub kind: String,
    pub tier: String,
    pub route: Route,
    /// `proceed`, `ask` or `downgraded`.
    pub action: &'static str,
    pub approval_reason: Option<&'static str>,
    pub capped_from: Option<&'static str>,
}

/// Resolve `kind` under the policy and the effort ceiling.
pub fn decide(
    kind: &str,
    tier: &str,
    failures: u32,
    policy: Escalate,
    ceiling: Option<&str>,
) -> Option<Decision> {
    let base = table(kind, tier)?;
    let reason = approval_reason(kind, &base, failures);
    let (mut route, action) = match (reason, policy) {
        (None, _) => (base, "proceed"),
        (Some(_), Escalate::Ask) => (base, "ask"),
        (Some(_), Escalate::Auto) => (base, "proceed"),
        (Some(_), Escalate::Deny) => (downgrade(kind, base), "downgraded"),
    };
    let mut capped_from = None;
    if let Some(cap) = ceiling {
        if effort::lower(Some(route.effort), Some(cap)) != Some(route.effort) {
            capped_from = Some(route.effort);
            route.effort = LEVEL_NAMES
                .iter()
                .find(|l| **l == cap)
                .copied()
                .unwrap_or(route.effort);
        }
    }
    Some(Decision {
        kind: kind.to_string(),
        tier: tier.to_string(),
        route,
        action,
        approval_reason: reason,
        capped_from,
    })
}

const LEVEL_NAMES: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// The text or JSON the command prints.
pub fn render(d: &Decision, json: bool) -> String {
    if json {
        return json!({
            "kind": d.kind,
            "tier": d.tier,
            "model": d.route.model,
            "effort": d.route.effort,
            "agent": d.route.agent,
            "reason": d.route.reason,
            "action": d.action,
            "approvalNeeded": d.action == "ask",
            "approvalReason": d.approval_reason,
            "cappedFrom": d.capped_from,
        })
        .to_string();
    }
    let mut out = format!(
        "{}: {} at {} effort via {} ({})\naction: {}",
        d.kind, d.route.model, d.route.effort, d.route.agent, d.route.reason, d.action
    );
    if let Some(why) = d.approval_reason {
        out.push_str(&format!("\napproval: this task {why}"));
    }
    if let Some(from) = d.capped_from {
        out.push_str(&format!("\neffort capped from {from} by your ceiling"));
    }
    out
}

/// Entry point for the CLI. `Err` carries a usage message.
pub fn run(
    home: &Path,
    claude_home: &Path,
    cwd: &Path,
    kind: &str,
    tier: &str,
    failures: u32,
    json: bool,
) -> Result<String, String> {
    if !["low", "medium", "high"].contains(&tier) {
        return Err(format!("unknown tier '{tier}'. Use low, medium or high"));
    }
    let ceiling = effort::ceiling(home, claude_home, cwd).effective;
    let d = decide(kind, tier, failures, escalate(home), ceiling.as_deref()).ok_or_else(|| {
        format!(
            "unknown task kind '{kind}'. Use one of: {}",
            KINDS.join(", ")
        )
    })?;
    Ok(render(&d, json))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_a_route() {
        for k in KINDS {
            assert!(table(k, "low").is_some(), "{k}");
        }
        assert!(table("nope", "low").is_none());
    }

    #[test]
    fn writing_goes_to_the_cheap_writer_without_approval() {
        let d = decide("write", "low", 0, Escalate::Ask, None).unwrap();

        assert_eq!(d.action, "proceed");
        assert_eq!(
            (d.route.model, d.route.effort, d.route.agent),
            ("haiku", "medium", "writer")
        );
    }

    #[test]
    fn routine_work_proceeds_without_approval() {
        let d = decide("implement", "low", 0, Escalate::Ask, None).unwrap();
        assert_eq!(d.action, "proceed");
        assert_eq!(d.route.model, "sonnet");
    }

    #[test]
    fn the_top_model_the_high_tier_and_two_failures_need_approval() {
        assert_eq!(
            decide("design", "low", 0, Escalate::Ask, None)
                .unwrap()
                .action,
            "ask"
        );
        assert_eq!(
            decide("implement", "high", 0, Escalate::Ask, None)
                .unwrap()
                .action,
            "ask"
        );
        assert_eq!(
            decide("check", "low", 2, Escalate::Ask, None)
                .unwrap()
                .action,
            "ask"
        );
        assert_eq!(
            decide("check", "low", 1, Escalate::Ask, None)
                .unwrap()
                .action,
            "proceed"
        );
    }

    #[test]
    fn auto_proceeds_and_deny_downgrades() {
        let a = decide("design", "low", 0, Escalate::Auto, None).unwrap();
        assert_eq!((a.action, a.route.model), ("proceed", "opus"));
        let d = decide("design", "low", 0, Escalate::Deny, None).unwrap();
        assert_eq!(
            (d.action, d.route.model, d.route.effort),
            ("downgraded", "sonnet", "high")
        );
        let h = decide("implement", "high", 0, Escalate::Deny, None).unwrap();
        assert_eq!(h.route.effort, "medium");
    }

    #[test]
    fn the_ceiling_always_wins() {
        let d = decide("design", "low", 0, Escalate::Auto, Some("medium")).unwrap();
        assert_eq!(d.route.effort, "medium");
        assert_eq!(d.capped_from, Some("xhigh"));
        let n = decide("check", "low", 0, Escalate::Ask, Some("high")).unwrap();
        assert_eq!(n.capped_from, None);
    }
}
