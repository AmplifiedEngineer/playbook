// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A plugin switched off in Claude Code pauses every hook and the status line.
//! Each case also runs the enabled control, so a guard that never blocks cannot
//! make the paused assertion pass for the wrong reason.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const DISABLED: &str = r#"{"enabledPlugins":{"playbook@amplifiedengineer":false}}"#;
const ENABLED: &str = r#"{"enabledPlugins":{"playbook@amplifiedengineer":true}}"#;

fn scratch_home(tag: &str, settings: Option<&str>) -> PathBuf {
    let home = std::env::temp_dir().join(format!(
        "playbook-plugin-disabled-{tag}-{}",
        playbook::testing::run_id()
    ));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(home.join(".claude")).unwrap();
    if let Some(text) = settings {
        fs::write(home.join(".claude/settings.json"), text).unwrap();
    }
    home
}

fn run_rm_guard(home: &PathBuf, ignore: bool) -> Output {
    let payload = serde_json::json!({ "tool_input": { "command": "rm -rf /" } });
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_playbook"));
    cmd.args(["hook", "rm-workspace-guard"])
        .env("HOME", home)
        .env("HOOK_INPUT", payload.to_string())
        .env_remove("CI")
        .env_remove("PLAYBOOK_HEADLESS")
        .env_remove("PLAYBOOK_IGNORE_PLUGIN_DISABLED");
    if ignore {
        cmd.env("PLAYBOOK_IGNORE_PLUGIN_DISABLED", "1");
    }
    cmd.output().unwrap()
}

fn run_statusline(home: &PathBuf) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .arg("statusline")
        .env("HOME", home)
        .env_remove("PLAYBOOK_IGNORE_PLUGIN_DISABLED")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"model":{"display_name":"Opus"},"workspace":{"current_dir":"/tmp"}}"#)
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn an_enabled_plugin_still_runs_the_guard() {
    let home = scratch_home("enabled", Some(ENABLED));
    let out = run_rm_guard(&home, false);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains(r#""permissionDecision":"deny""#));
}

#[test]
fn a_missing_settings_file_still_runs_the_guard() {
    let home = scratch_home("nosettings", None);
    let out = run_rm_guard(&home, false);
    assert!(String::from_utf8_lossy(&out.stdout).contains(r#""permissionDecision":"deny""#));
}

#[test]
fn a_malformed_settings_file_still_runs_the_guard() {
    let home = scratch_home(
        "malformed",
        Some(r#"{"enabledPlugins":{"playbook@amplifiedengineer":false"#),
    );
    let out = run_rm_guard(&home, false);
    assert!(String::from_utf8_lossy(&out.stdout).contains(r#""permissionDecision":"deny""#));
}

#[test]
fn a_disabled_plugin_makes_the_hook_a_silent_success() {
    let home = scratch_home("disabled", Some(DISABLED));
    let out = run_rm_guard(&home, false);
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn a_disabled_plugin_does_not_pause_the_worktree_hooks() {
    // The hook stands in for Claude Code's own worktree creation, so it must
    // still run. With no cwd in the payload it refuses and says so.
    let home = scratch_home("worktree", Some(DISABLED));
    let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["hook", "worktree-create"])
        .env("HOME", &home)
        .env("HOOK_INPUT", r#"{"name":"x"}"#)
        .env_remove("PLAYBOOK_IGNORE_PLUGIN_DISABLED")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("payload cwd is missing"));
}

#[test]
fn the_escape_variable_runs_the_hook_even_when_the_plugin_is_disabled() {
    let home = scratch_home("escape", Some(DISABLED));
    let out = run_rm_guard(&home, true);
    assert!(String::from_utf8_lossy(&out.stdout).contains(r#""permissionDecision":"deny""#));
}

#[test]
fn a_disabled_plugin_makes_the_status_line_print_nothing() {
    let home = scratch_home("sl-disabled", Some(DISABLED));
    let out = run_statusline(&home);
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
}

#[test]
fn an_enabled_plugin_still_prints_the_status_line() {
    let home = scratch_home("sl-enabled", Some(ENABLED));
    let out = run_statusline(&home);
    assert!(!out.stdout.is_empty());
}
