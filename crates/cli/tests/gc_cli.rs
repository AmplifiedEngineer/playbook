// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook gc`, spawning the real binary with `$HOME` pinned to a scratch
//! directory so the report never reads the developer's real state.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, Output};

fn scratch_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "playbook-gc-cli-{}-{tag}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&dir).expect("scratch home should be creatable");
    dir
}

fn gc(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_playbook"))
        .arg("gc")
        .args(args)
        .env("HOME", home)
        .output()
        .expect("playbook should spawn")
}

#[test]
fn gc_report_lists_nothing_and_creates_nothing() {
    let home = scratch_home("report");

    let out = gc(&home, &[]);

    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("No gc stores are registered."));
    assert!(!home.join(".config/playbook/gc").exists());
}

#[test]
fn gc_build_output_flag_is_refused_until_it_is_wired() {
    let home = scratch_home("flags");

    let out = gc(&home, &["--build-output"]);

    assert_eq!(out.status.code(), Some(2));
    assert!(!home.join(".config/playbook/gc").exists());
}

#[test]
fn gc_yes_and_restore_cannot_be_combined() {
    let home = scratch_home("conflict");

    let out = gc(&home, &["--yes", "--restore", "x"]);

    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn gc_yes_on_an_empty_home_succeeds_and_releases_the_lock() {
    let home = scratch_home("yes-empty");

    let out = gc(&home, &["--yes"]);

    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("0 tombstoned"));
    assert!(!home.join(".config/playbook/gc/.lock").exists());
}
