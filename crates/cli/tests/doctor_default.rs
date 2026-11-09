// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A bare `playbook doctor` runs `playbook doctor check`: same output, same
//! exit code. `--fix` keeps its own path.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

fn scratch(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "pb-doctor-default-{tag}-{}",
        playbook::testing::run_id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("home/.claude")).unwrap();
    root
}

fn run(root: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(args)
        .env("HOME", root.join("home"))
        .env("SHELL", "/bin/zsh")
        .env("PATH", "/usr/bin:/bin")
        .env_remove("CLAUDE_PLUGIN_ROOT")
        .env_remove("PLAYBOOK_IGNORE_PLUGIN_DISABLED")
        .env("NO_COLOR", "1")
        .current_dir(root)
        .output()
        .expect("spawn playbook")
}

/// The data folder row shows a disk size that moves between runs.
fn stable(out: &[u8]) -> String {
    String::from_utf8_lossy(out)
        .lines()
        .filter(|l| !l.contains("data folder"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_same_as_check(root: &PathBuf) {
    // The first run creates the data folder; warm it so both runs see the same state.
    run(root, &["doctor", "check"]);
    let bare = run(root, &["doctor"]);
    let check = run(root, &["doctor", "check"]);
    assert_eq!(bare.status.code(), check.status.code());
    assert_eq!(stable(&bare.stdout), stable(&check.stdout));
    assert!(!bare.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&bare.stderr).contains("pass a subcommand"));
}

#[test]
fn bare_doctor_matches_check_on_a_plain_home() {
    let root = scratch("plain");
    fs::write(root.join("home/.claude/settings.json"), "{}").unwrap();
    assert_same_as_check(&root);
}

#[test]
fn bare_doctor_matches_check_when_a_row_fails() {
    let root = scratch("fail");
    let check = run(&root, &["doctor", "check"]);
    assert!(String::from_utf8_lossy(&check.stdout).contains("FAIL"));
    assert_same_as_check(&root);
}

#[test]
fn doctor_fix_still_takes_the_fix_path() {
    let root = scratch("fix");
    let out = run(&root, &["doctor", "--fix"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("nothing to fix"));
}
