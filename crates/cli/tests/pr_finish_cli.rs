// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr finish context` against a fake `gh` on PATH and a scratch HOME.

use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

const HEAD_OID: &str = "0123456789abcdef0123456789abcdef01234567";

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("pb-fc-{tag}-{}-{n}", playbook::testing::run_id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        Sandbox { root }
    }

    fn gh(&self, script: &str) {
        let f = self.root.join("bin/gh");
        fs::write(&f, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn gh_draft_pr(&self) {
        self.gh(&format!(
            r#"case "$2" in
view) echo '{{"number":7,"url":"https://github.com/o/r/pull/7","isDraft":true,"headRefOid":"{HEAD_OID}","baseRefName":"main"}}';;
list) echo '[]';;
esac"#
        ));
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(args)
            .env("HOME", self.root.join("home"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
            .env_remove("PLAYBOOK_MODE")
            .current_dir(&self.root)
            .output()
            .unwrap()
    }

    fn config(&self, key: &str, value: &str) {
        let o = self.run(&["config", "set", key, value, "--global"]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
}

fn json_of(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout).expect("stdout is one JSON object")
}

#[test]
fn quick_type_on_a_draft_pr_reports_the_pr_and_the_review() {
    let s = Sandbox::new("quick");
    s.gh_draft_pr();
    s.config("autoReview.type", "quick");
    let o = s.run(&["pr", "finish", "context", "7", "--json"]);
    assert_eq!(o.status.code(), Some(0));
    let v = json_of(&o);
    assert_eq!(v["ok"], true);
    assert_eq!(v["code"], 0);
    assert_eq!(v["review"], "quick");
    assert_eq!(v["pr"]["isDraft"], true);
    assert_eq!(v["pr"]["head"], HEAD_OID);
    assert_eq!(v["settings"]["autoReview.type"]["source"], "global");
}

#[test]
fn the_flag_sets_the_mode_and_its_source() {
    let s = Sandbox::new("flag");
    s.gh_draft_pr();
    let o = s.run(&["pr", "finish", "context", "7", "--json", "--flag", "auto"]);
    let v = json_of(&o);
    assert_eq!(v["mode"]["mode"], "auto");
    assert_eq!(v["mode"]["source"], "flag");
}

#[test]
fn a_failed_pr_view_exits_zero_with_code_five() {
    let s = Sandbox::new("fail");
    s.gh("echo 'no pull requests found' >&2; exit 1");
    let o = s.run(&["pr", "finish", "context", "7", "--json"]);
    assert_eq!(o.status.code(), Some(0));
    let v = json_of(&o);
    assert_eq!(v["ok"], false);
    assert_eq!(v["code"], 5);
    assert!(v["error"].as_str().unwrap().contains("no pull requests"));
}

#[test]
fn a_missing_dir_exits_zero_with_the_error_in_the_envelope() {
    let s = Sandbox::new("dir");
    s.gh_draft_pr();
    let o = s.run(&[
        "pr",
        "finish",
        "context",
        "7",
        "--json",
        "--dir",
        "/missing/dir",
    ]);
    assert_eq!(o.status.code(), Some(0));
    let v = json_of(&o);
    assert_eq!(v["ok"], false);
    assert!(v["error"].as_str().unwrap().contains("/missing/dir"));
}

#[test]
fn plain_mode_keeps_the_failure_exit_code() {
    let s = Sandbox::new("plain");
    s.gh("exit 1");
    let o = s.run(&["pr", "finish", "context", "7"]);
    assert_eq!(o.status.code(), Some(5));
}
