// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook effort suggest` end to end on a synthetic usage store (ADR-0022,
//! stage 1). Every id, session and repo name below is made up.

use playbook::usage::db::{insert_tool_event, open_db, upsert_usage_event};
use playbook::usage::{ToolInvocationEvent, ToolKind, UsageEvent};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);
const DAY: i64 = 86_400;

struct Env {
    home: PathBuf,
    plugin: PathBuf,
}

impl Env {
    fn new(tag: &str, agents: &[(&str, &str, &str)]) -> Env {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!(
            "playbook-effort-suggest-{}-{tag}-{n}",
            playbook::testing::run_id()
        ));
        let _ = fs::remove_dir_all(&base);
        let home = base.join("home");
        let plugin = base.join("plugin");
        fs::create_dir_all(home.join(".config/playbook/data")).unwrap();
        fs::create_dir_all(plugin.join("agents")).unwrap();
        for (name, model, effort) in agents {
            fs::write(
                plugin.join(format!("agents/{name}.md")),
                format!("---\nname: {name}\ndescription: d\nmodel: {model}\neffort: {effort}\n---\nBody\n"),
            )
            .unwrap();
        }
        Env { home, plugin }
    }

    fn db(&self) -> PathBuf {
        self.home.join(".config/playbook/data/usage.db")
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(args)
            .current_dir(&self.home)
            .env("HOME", &self.home)
            .env("CLAUDE_PLUGIN_ROOT", &self.plugin)
            .env_remove("CLAUDE_CONFIG_DIR")
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// `count` messages of `model` at `effort`, `days_ago` days back.
fn messages(env: &Env, tag: &str, model: &str, effort: &str, count: usize, days_ago: i64) {
    let conn = open_db(&env.db()).unwrap();
    for i in 0..count {
        let mut e = UsageEvent {
            event_id: format!("{tag}-{i}"),
            timestamp: now() - days_ago * DAY + i as i64,
            session_id: format!("synthetic-session-{}", i % 7),
            account: "acct".into(),
            agent: "claude-code".into(),
            model: model.into(),
            effort: effort.into(),
            repo: "example/repo".into(),
            branch: "main".into(),
            input_tokens: 1000,
            output_tokens: 500,
            ..UsageEvent::default()
        };
        e.apply_pricing();
        upsert_usage_event(&conn, &e).unwrap();
    }
}

/// `count` dispatches of `name`, spread over `sessions` sessions.
fn dispatches(env: &Env, name: &str, count: usize, sessions: usize) {
    let conn = open_db(&env.db()).unwrap();
    for i in 0..count {
        insert_tool_event(
            &conn,
            &ToolInvocationEvent {
                event_id: format!("t-{name}-{i}"),
                timestamp: now() - 2 * DAY + i as i64,
                session_id: format!("synthetic-session-{}", i % sessions),
                account: "acct".into(),
                kind: ToolKind::Agent,
                name: name.into(),
            },
        )
        .unwrap();
    }
}

fn agent<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == name)
        .unwrap_or_else(|| panic!("no agent {name}"))
}

fn kinds(a: &Value) -> Vec<String> {
    a["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["kind"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_component_above_its_floor_with_enough_evidence_gets_a_lower_proposal_with_costs() {
    // critic ships at high here, so its floor (medium) is below it.
    let env = Env::new(
        "lower",
        &[("critic", "sonnet", "high"), ("reviewer", "opus", "high")],
    );
    messages(&env, "old", "claude-sonnet-5", "high", 60, 20);
    messages(&env, "med", "claude-sonnet-5", "medium", 60, 10);
    dispatches(&env, "playbook:critic", 25, 25);
    dispatches(&env, "reviewer", 30, 10);

    let report = env.json(&["effort", "suggest", "--json"]);
    let critic = agent(&report, "critic");
    assert_eq!(critic["status"], "lower");
    assert_eq!(critic["floor"], "medium");
    assert_eq!(critic["dispatches"], 25);
    let text = critic["findings"][0]["message"].as_str().unwrap();
    assert!(text.contains("effort.agents.critic medium"), "{text}");
    assert!(text.contains("Mean cost per message"), "{text}");
    assert!(critic["buckets"].as_array().unwrap().len() >= 2);

    // The reviewer is costly to miss: the floor table never lets it be lowered.
    let reviewer = agent(&report, "reviewer");
    assert!(
        !kinds(reviewer).contains(&"lower".to_string()),
        "{reviewer}"
    );
    assert_eq!(reviewer["costlyToMiss"], true);
}

#[test]
fn a_ceiling_below_the_floor_is_needed_and_a_cold_start_proposes_nothing() {
    let env = Env::new("ceiling", &[("critic", "sonnet", "medium")]);
    messages(&env, "recent", "claude-sonnet-5", "medium", 10, 1);
    dispatches(&env, "playbook:critic", 40, 10);
    let set = env.run(&["config", "set", "--global", "effort.agents.critic", "low"]);
    assert!(
        set.status.success(),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );

    let report = env.json(&["effort", "suggest", "--json"]);
    assert_eq!(report["coldStart"], true);
    let critic = agent(&report, "critic");
    assert_eq!(critic["status"], "needed");
    assert_eq!(critic["effective"], "low");
    assert!(kinds(critic).contains(&"ceiling-below-floor".to_string()));
    assert!(!kinds(critic).contains(&"lower".to_string()));
}

#[test]
fn upward_overrides_hold_a_lowering_back_and_are_reported_as_needed() {
    let env = Env::new("escalated", &[("critic", "sonnet", "high")]);
    messages(&env, "old", "claude-sonnet-5", "high", 5, 40);
    dispatches(&env, "playbook:critic", 12, 6);
    dispatches(&env, "critic-xhigh", 12, 6);

    let critic_report = env.json(&["effort", "suggest", "--json", "--range", "all"]);
    let critic = agent(&critic_report, "critic");
    assert_eq!(critic["overridesUp"], 12);
    assert_eq!(critic["status"], "needed");
    assert!(kinds(critic).contains(&"escalated".to_string()));
    assert!(!kinds(critic).contains(&"lower".to_string()));
}

#[test]
fn many_repeats_inside_a_session_hold_a_lowering_back_without_proposing_a_change() {
    let env = Env::new("reruns", &[("critic", "sonnet", "high")]);
    messages(&env, "old", "claude-sonnet-5", "high", 5, 40);
    dispatches(&env, "playbook:critic", 25, 6);

    let report = env.json(&["effort", "suggest", "--json"]);
    let critic = agent(&report, "critic");
    assert_eq!(critic["reruns"], 19);
    assert_eq!(critic["status"], "no-change");
    assert_eq!(kinds(critic), ["reruns", "lower-held"]);
}

#[test]
fn too_few_dispatches_is_insufficient_and_no_dispatch_is_unused() {
    let env = Env::new(
        "few",
        &[("critic", "sonnet", "high"), ("analyst", "haiku", "medium")],
    );
    messages(&env, "old", "claude-sonnet-5", "high", 5, 40);
    dispatches(&env, "playbook:critic", 5, 5);
    let report = env.json(&["effort", "suggest", "--json"]);
    assert_eq!(agent(&report, "critic")["status"], "insufficient-data");
    assert_eq!(agent(&report, "analyst")["status"], "unused");
}

#[test]
fn it_reads_only_and_a_missing_store_is_an_error_that_creates_nothing() {
    let env = Env::new("readonly", &[("critic", "sonnet", "high")]);
    let out = env.run(&["effort", "suggest"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no usage store"));
    assert!(!env.db().exists(), "a missing store must not be created");

    messages(&env, "old", "claude-sonnet-5", "high", 5, 40);
    dispatches(&env, "playbook:critic", 25, 8);
    let before = fs::read(env.db()).unwrap();
    let out = env.run(&["effort", "suggest"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("read only"));
    assert!(text.contains("Nothing was written"));
    assert_eq!(fs::read(env.db()).unwrap(), before, "the store changed");
    for untouched in [
        ".claude",
        ".config/playbook/memory",
        ".config/playbook/config.json",
    ] {
        assert!(
            !env.home.join(untouched).exists(),
            "{untouched} was created"
        );
    }
}

#[test]
fn an_unknown_range_fails_with_the_valid_values() {
    let env = Env::new("range", &[("critic", "sonnet", "high")]);
    let out = env.run(&["effort", "suggest", "--range", "7d"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr)
        .contains("today, week, last-week, month, last-month, 30d, 60d, 90d or all"));
}
