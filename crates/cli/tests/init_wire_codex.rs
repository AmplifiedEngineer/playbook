// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Tests for `playbook::init::codex_hooks`, which registers the guards that
//! work in Codex in `~/.codex/hooks.json`. Every test uses a scratch directory,
//! never a real Codex home.

use playbook::init::codex_hooks::{audit, unwire, wire, CODEX_HOOK_SPECS};
use playbook::init::wire::wired_hooks;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch_hooks_path() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "playbook-codex-hooks-{}-{n}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir.join("hooks.json")
}

fn read(path: &PathBuf) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn commands(root: &Value, event: &str) -> Vec<String> {
    root["hooks"][event]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
        .filter_map(|e| e["command"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn wire_writes_only_the_codex_guards_and_no_if_field() {
    // Arrange
    let path = scratch_hooks_path();
    // Act
    let outcome = wire(&path, 100).unwrap();
    // Assert
    assert!(outcome.changed);
    let root = read(&path);
    assert_eq!(
        commands(&root, "PreToolUse"),
        [
            "playbook hook rm-workspace-guard",
            "playbook hook precommit-check"
        ]
    );
    assert!(commands(&root, "PostToolUse").is_empty());
    assert!(!root.to_string().contains("\"if\""));
    assert_eq!(root["hooks"]["PreToolUse"][0]["matcher"], "Bash");
}

#[test]
fn a_second_wire_changes_nothing_and_takes_no_backup() {
    // Arrange
    let path = scratch_hooks_path();
    wire(&path, 100).unwrap();
    let before = fs::read_to_string(&path).unwrap();
    // Act
    let outcome = wire(&path, 200).unwrap();
    // Assert
    assert!(!outcome.changed && outcome.backup_path.is_none());
    assert_eq!(fs::read_to_string(&path).unwrap(), before);
}

#[test]
fn wire_keeps_user_entries_and_backs_the_file_up() {
    // Arrange
    let path = scratch_hooks_path();
    let user = json!({
        "model": "keep",
        "hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [
            {"type": "command", "command": "my-own-hook"}
        ]}]}
    });
    fs::write(&path, user.to_string()).unwrap();
    // Act
    let outcome = wire(&path, 100).unwrap();
    // Assert
    let root = read(&path);
    assert_eq!(root["model"], "keep");
    assert!(commands(&root, "PreToolUse").contains(&"my-own-hook".to_string()));
    assert_eq!(commands(&root, "PreToolUse").len(), 3);
    let backup = outcome.backup_path.expect("backup of the existing file");
    assert_eq!(fs::read_to_string(backup).unwrap(), user.to_string());
}

#[test]
fn an_invalid_json_file_is_refused_and_left_alone() {
    // Arrange
    let path = scratch_hooks_path();
    fs::write(&path, "{ not json").unwrap();
    // Act
    let result = wire(&path, 100);
    // Assert
    assert!(result.is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "{ not json");
}

#[test]
fn unwire_removes_only_our_entries_and_keeps_other_keys() {
    // Arrange
    let path = scratch_hooks_path();
    fs::write(
        &path,
        json!({
            "statusLine": {"command": "playbook statusline"},
            "hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [
                {"type": "command", "command": "my-own-hook"}
            ]}]}
        })
        .to_string(),
    )
    .unwrap();
    wire(&path, 100).unwrap();
    // Act
    let removed = unwire(&path, 300, false).unwrap();
    // Assert
    assert_eq!(removed, 2);
    let root = read(&path);
    assert_eq!(commands(&root, "PreToolUse"), ["my-own-hook"]);
    assert_eq!(root["statusLine"]["command"], "playbook statusline");
    assert_eq!(unwire(&path, 400, false).unwrap(), 0);
}

#[test]
fn unwire_on_a_file_holding_only_our_entries_leaves_an_empty_object() {
    // Arrange
    let path = scratch_hooks_path();
    wire(&path, 100).unwrap();
    // Act
    unwire(&path, 300, false).unwrap();
    // Assert
    assert_eq!(read(&path), json!({}));
}

#[test]
fn a_dry_run_unwire_counts_and_writes_nothing() {
    // Arrange
    let path = scratch_hooks_path();
    wire(&path, 100).unwrap();
    let before = fs::read_to_string(&path).unwrap();
    // Act
    let removed = unwire(&path, 300, true).unwrap();
    // Assert
    assert_eq!(removed, 2);
    assert_eq!(fs::read_to_string(&path).unwrap(), before);
}

#[test]
fn audit_reports_missing_then_present_and_never_the_claude_only_hooks() {
    // Arrange
    let path = scratch_hooks_path();
    // Act
    let before = audit(&path);
    wire(&path, 100).unwrap();
    let after = audit(&path);
    // Assert
    assert_eq!(before.missing, ["rm-workspace-guard", "precommit-check"]);
    assert_eq!(before.present, 0);
    assert!(after.missing.is_empty() && after.stale.is_empty());
    assert_eq!(after.present, CODEX_HOOK_SPECS.len());
}

#[test]
fn the_claude_hook_list_is_unchanged_by_the_codex_set() {
    // Arrange / Act
    let claude: Vec<&str> = wired_hooks().map(|s| s.name).collect();
    // Assert
    assert!(claude.contains(&"commit-message-sanitizer"));
    assert!(wired_hooks().all(|s| !std::ptr::eq(s, &CODEX_HOOK_SPECS[0])));
}
