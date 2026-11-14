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

/// A row reads `<LEVEL>  <message>`. Keeps every row, its level and its label,
/// and masks only what follows a volatile label at the start of the message.
/// A missing data folder is a state, not a size, so that row stays whole.
fn stable(out: &[u8]) -> String {
    String::from_utf8_lossy(out)
        .lines()
        .map(|line| {
            let Some((level, rest)) = line.split_once(char::is_whitespace) else {
                return line.to_string();
            };
            let message = rest.trim_start();
            if message.ends_with("is empty or missing") {
                return line.to_string();
            }
            match VOLATILE_LABELS.iter().find(|l| message.starts_with(**l)) {
                Some(label) => format!("{level} {label}<volatile>"),
                None => line.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn stable_masks_the_measured_value_and_keeps_the_row() {
    let a = b"PASS  free disk space: 85.3 GiB\nWARN  free disk space is low: 1.0 GiB of 9.0 GiB\nINFO  other 1";
    let b = b"PASS  free disk space: 85.2 GiB\nWARN  free disk space is low: 2.0 GiB of 9.0 GiB\nINFO  other 1";
    assert_eq!(stable(a), stable(b));
    assert_eq!(
        stable(b"PASS  free disk space: 85.3 GiB"),
        "PASS free disk space<volatile>"
    );
    // The gc and data folder sizes are masked the same way.
    assert_eq!(
        stable(b"INFO  gc: 1 KB in 2 items across 4 stores, 0 tombstones kept"),
        stable(b"INFO  gc: 9 KB in 3 items across 4 stores, 0 tombstones kept")
    );
    assert_eq!(
        stable(b"INFO  data folder /x: 3 files, 4 KB"),
        stable(b"INFO  data folder /x: 5 files, 9 KB")
    );
    // A different level, a missing row or a changed stable row still differs.
    assert_ne!(
        stable(a),
        stable(b"FAIL  free disk space: 85.3 GiB\nWARN  free disk space is low: 1.0 GiB of 9.0 GiB\nINFO  other 1")
    );
    assert_ne!(stable(a), stable(b"PASS  free disk space: 85.3 GiB"));
    assert_ne!(stable(b"INFO  other 1"), stable(b"INFO  other 2"));
    // A message that only mentions a label later on is not masked.
    assert_ne!(
        stable(b"WARN  fix: check the data folder 1"),
        stable(b"WARN  fix: check the data folder 2")
    );
    // An empty data folder is not the same state as a filled one.
    assert_ne!(
        stable(b"INFO  data folder /x is empty or missing"),
        stable(b"INFO  data folder /x: 3 files, 4 KB")
    );
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
