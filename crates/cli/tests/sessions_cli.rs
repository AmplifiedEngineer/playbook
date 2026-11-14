// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Binary-level tests for `playbook sessions`, run against a scratch `$HOME`.
//! The fixture history is older than the default window, so it lists nothing.

mod common;

use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALLS: AtomicUsize = AtomicUsize::new(0);

fn run(args: &[&str]) -> (bool, String) {
    // One home per call: tests run in parallel and each call deletes its home.
    let n = CALLS.fetch_add(1, Ordering::Relaxed);
    let home = std::env::temp_dir().join(format!(
        "playbook-sessions-{}-{n}",
        playbook::testing::run_id()
    ));
    let project = home.join(".claude/projects/proj-one");
    fs::create_dir_all(&project).unwrap();
    let fixtures = std::path::Path::new(env!("PLAYBOOK_REPO_ROOT"))
        .join("crates/telemetry/tests/fixtures/usage");
    fs::copy(
        fixtures.join("usage/proj-one/s1.jsonl"),
        project.join("s1.jsonl"),
    )
    .unwrap();
    fs::copy(
        fixtures.join("account/claude.json"),
        home.join(".claude.json"),
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(args)
        .env("HOME", home.canonicalize().unwrap())
        .env("PATH", common::path_with_failing_gh(&home))
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&home);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn history_older_than_the_window_lists_no_sessions() {
    let (ok, text) = run(&["sessions"]);
    assert!(ok);
    assert_eq!(text, "No recent sessions.\n");
}

#[test]
fn json_has_a_generation_time_and_an_empty_session_list() {
    let (ok, text) = run(&["sessions", "--json"]);
    assert!(ok);
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(doc["generated_at"].as_i64().unwrap() > 0);
    assert_eq!(doc["sessions"].as_array().unwrap().len(), 0);
}

#[test]
fn the_list_subcommand_is_the_same_as_the_bare_command() {
    assert_eq!(run(&["sessions", "list"]), run(&["sessions"]));
}

#[test]
fn the_sweep_dry_run_reports_and_names_nothing_to_close() {
    let (ok, text) = run(&["sessions", "sweep", "--dry-run"]);
    assert!(ok);
    assert_eq!(
        text,
        "Dry run: nothing is removed, stopped or closed.\nNo finished or stale sessions.\n"
    );
}

#[test]
fn the_sweep_refuses_to_run_without_the_dry_run_flag() {
    let (ok, text) = run(&["sessions", "sweep"]);
    assert!(!ok);
    assert!(text.is_empty());
}
