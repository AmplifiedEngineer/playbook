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

/// Rows whose text carries a measured size that moves between two runs: the
/// data folder and gc rows (file sizes), and the free disk space row (other
/// processes on the machine change it, which flaked as 85.3 GiB vs 85.2 GiB).
/// The free disk row has a low-space variant, so the label stops before the value.
const VOLATILE_LABELS: [&str; 3] = ["data folder", "gc:", "free disk space"];

/// Keeps every row, its level and its label, and masks only what follows the
/// label. A row that vanishes or changes level still fails the comparison.
fn stable(out: &[u8]) -> String {
    String::from_utf8_lossy(out)
        .lines()
        .map(|line| {
            VOLATILE_LABELS
                .iter()
                .find_map(|label| line.find(label).map(|at| (at, label.len())))
                .map_or_else(
                    || line.to_string(),
                    |(at, len)| format!("{}<volatile>", &line[..at + len]),
                )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn stable_masks_the_measured_value_and_keeps_the_row() {
    let a = b"OK   free disk space: 85.3 GiB\nWARN free disk space is low: 1.0 GiB of 9.0 GiB\nOK   other 1";
    let b = b"OK   free disk space: 85.2 GiB\nWARN free disk space is low: 2.0 GiB of 9.0 GiB\nOK   other 1";
    assert_eq!(stable(a), stable(b));
    // A different level, a missing row or a changed stable row still differs.
    assert_ne!(
        stable(a),
        stable(b"WARN free disk space: 85.3 GiB\nOK   other 1")
    );
    assert_ne!(stable(a), stable(b"OK   free disk space: 85.3 GiB"));
    assert_ne!(stable(b"OK   other 1"), stable(b"OK   other 2"));
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
