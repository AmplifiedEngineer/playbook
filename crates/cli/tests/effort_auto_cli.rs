// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook effort choose` and `playbook effort log` end to end: off by
//! default, a choice inside the ceiling when on, a log row for each choice,
//! and nothing written outside playbook's own files.

use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Home(PathBuf);

impl Home {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "playbook-effort-auto-{}-{tag}-{n}",
            playbook::testing::run_id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".claude")).unwrap();
        Home(dir)
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(args)
            .current_dir(&self.0)
            .env("HOME", &self.0)
            .env(
                "CLAUDE_PLUGIN_ROOT",
                concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/plugin"),
            )
            .env(
                "PLAYBOOK_AGENT_VARIANTS",
                "reviewer-low,reviewer-high,reviewer-xhigh",
            )
            .env_remove("PLAYBOOK_AGENT_VARIANTS_PLUGIN")
            .env_remove("CLAUDE_CONFIG_DIR")
            .output()
            .expect("playbook should spawn")
    }

    fn choose(&self, extra: &[&str]) -> Value {
        let mut args = vec!["effort", "choose", "reviewer", "--json"];
        args.extend_from_slice(extra);
        let out = self.run(&args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

#[test]
fn auto_is_off_by_default_and_writes_no_log() {
    let h = Home::new("off");
    let v = h.choose(&["--path", "src/auth/login.rs", "--lines", "5"]);
    assert_eq!(v["mode"], "off");
    assert_eq!(v["deviates"], false);
    assert_eq!(v["dispatch"], "playbook:reviewer");
    assert!(!h.0.join(".config/playbook/effort/auto.log").exists());
}

#[test]
fn on_raises_a_risky_diff_and_logs_it() {
    let h = Home::new("on");
    assert!(h
        .run(&["config", "set", "--global", "effort.auto", "on"])
        .status
        .success());
    let v = h.choose(&["--path", "src/auth/login.rs", "--lines", "5"]);
    assert_eq!(v["mode"], "on");
    assert_eq!(v["deviates"], true);
    assert_eq!(v["chosen"]["level"], "high");
    assert_eq!(v["dispatch"], "playbook:reviewer");
    assert_eq!(v["effort"], "high");
    assert!(v["note"].as_str().unwrap().contains("instead of"));

    let log = h.run(&["effort", "log", "--json"]);
    let rows: Value = serde_json::from_slice(&log.stdout).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["component"], "reviewer");
    assert_eq!(rows[0]["chosen"], "high");
}

#[test]
fn a_ceiling_at_the_default_holds_the_choice() {
    let h = Home::new("ceiling");
    assert!(h
        .run(&["config", "set", "--global", "effort.auto", "on"])
        .status
        .success());
    assert!(h.run(&["effort", "medium"]).status.success());
    let v = h.choose(&["--path", "src/auth/login.rs"]);
    assert_eq!(v["chosen"]["level"], "medium");
    assert_eq!(v["effort"], "medium");
    assert_eq!(v["dispatch"], "playbook:reviewer");
}

#[test]
fn a_bad_diff_base_falls_back_to_the_default() {
    let h = Home::new("badbase");
    assert!(h
        .run(&["config", "set", "--global", "effort.auto", "on"])
        .status
        .success());
    let v = h.choose(&["--base", "no-such-ref-anywhere"]);
    assert_eq!(v["deviates"], false);
    assert_eq!(v["dispatch"], "playbook:reviewer");
    assert!(v["reason"].as_str().unwrap().contains("fell back"));
}

#[test]
fn claude_code_settings_are_never_written() {
    let h = Home::new("noclaude");
    h.run(&["config", "set", "--global", "effort.auto", "on"]);
    h.choose(&["--path", "src/auth/login.rs"]);
    assert_eq!(fs::read_dir(h.0.join(".claude")).unwrap().count(), 0);
}
