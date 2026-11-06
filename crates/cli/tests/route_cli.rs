// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook route` end to end: the table, the approval gate, the
//! `routing.escalate` policy and the effort ceiling.

use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "playbook-route-{}-{n}",
            playbook::testing::run_id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Home(dir)
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(args)
            .current_dir(&self.0)
            .env("HOME", &self.0)
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CLAUDE_PLUGIN_ROOT")
            .output()
            .expect("playbook should spawn")
    }

    fn route(&self, args: &[&str]) -> Value {
        let mut a = vec!["route"];
        a.extend_from_slice(args);
        a.push("--json");
        let out = self.run(&a);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn routine_implementation_proceeds_on_sonnet_at_low() {
    let h = Home::new();
    let v = h.route(&["implement"]);
    assert_eq!(v["model"], "sonnet");
    assert_eq!(v["effort"], "low");
    assert_eq!(v["action"], "proceed");
    assert_eq!(v["approvalNeeded"], false);
}

#[test]
fn design_proceeds_by_default() {
    let h = Home::new();
    let v = h.route(&["design"]);
    assert_eq!(v["model"], "opus");
    assert_eq!(v["action"], "proceed");
    assert_eq!(v["approvalNeeded"], false);
    assert_eq!(v["approvalReason"], "runs on the top model");
}

#[test]
fn escalate_ask_asks_first_for_the_top_model() {
    let h = Home::new();
    assert!(h
        .run(&["config", "set", "--global", "routing.escalate", "ask"])
        .status
        .success());
    let v = h.route(&["design"]);
    assert_eq!(v["action"], "ask");
    assert_eq!(v["approvalNeeded"], true);
}

#[test]
fn escalate_auto_proceeds_and_deny_returns_the_cheaper_route() {
    let h = Home::new();
    assert!(h
        .run(&["config", "set", "--global", "routing.escalate", "auto"])
        .status
        .success());
    assert_eq!(h.route(&["design"])["action"], "proceed");
    assert!(h
        .run(&["config", "set", "--global", "routing.escalate", "deny"])
        .status
        .success());
    let v = h.route(&["design"]);
    assert_eq!(v["action"], "downgraded");
    assert_eq!(v["model"], "sonnet");
}

#[test]
fn a_second_failure_needs_approval_and_the_ceiling_wins() {
    let h = Home::new();
    assert!(h
        .run(&["config", "set", "--global", "routing.escalate", "ask"])
        .status
        .success());
    assert_eq!(h.route(&["check", "--failures", "2"])["action"], "ask");
    assert!(h.run(&["effort", "low"]).status.success());
    let v = h.route(&["check"]);
    assert_eq!(v["effort"], "low");
    assert_eq!(v["cappedFrom"], "medium");
}

#[test]
fn an_unknown_kind_or_tier_fails_with_a_hint() {
    let h = Home::new();
    let out = h.run(&["route", "nope"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("mechanical"));
    assert_eq!(
        h.run(&["route", "implement", "--tier", "x"]).status.code(),
        Some(2)
    );
}
