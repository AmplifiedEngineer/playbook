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
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("session folders") && text.contains("migration leftovers"));
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

/// Epoch seconds the binary reads as "now" in debug builds.
const NOW: u64 = 1_800_000_000;
const DAY: u64 = 86_400;

fn gc_at_fixed_now(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_playbook"))
        .arg("gc")
        .args(args)
        .env("HOME", home)
        .env("PLAYBOOK_TEST_NOW", NOW.to_string())
        .output()
        .expect("playbook should spawn")
}

fn backdate(path: &Path, age_days: u64) {
    let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(NOW - age_days * DAY);
    fs::File::open(path)
        .and_then(|file| file.set_modified(when))
        .expect("mtime should be settable");
}

#[test]
fn restore_flag_round_trips_end_to_end() {
    // Arrange
    let home = scratch_home("round-trip");
    let session = home.join(".config/playbook/runtime/sess-old");
    fs::create_dir_all(&session).unwrap();
    fs::write(session.join("state"), "x").unwrap();
    backdate(&session.join("state"), 15);
    backdate(&session, 15);

    // Act
    let applied = gc_at_fixed_now(&home, &["--yes"]);
    let gone = !session.exists();
    let index = fs::read_to_string(home.join(".config/playbook/gc/index.jsonl")).unwrap();
    let id = index
        .lines()
        .next()
        .and_then(|line| line.split("\"id\":\"").nth(1))
        .and_then(|rest| rest.split('"').next())
        .expect("the index should name the tombstone")
        .to_string();
    let restored = gc_at_fixed_now(&home, &["--restore", &id]);

    // Assert
    assert!(applied.status.success(), "{applied:?}");
    assert!(gone, "the idle session folder should be tombstoned");
    assert!(restored.status.success(), "{restored:?}");
    assert_eq!(fs::read_to_string(session.join("state")).unwrap(), "x");
}

#[test]
fn report_only_data_survives_yes() {
    // Arrange
    let home = scratch_home("report-only");
    let root = home.join(".config/playbook");
    let kept = [
        root.join("memory/fact.md"),
        root.join("plans/plan.md"),
        root.join("data/usage.db"),
        home.join(".cache/playbook/blob"),
    ];
    for path in &kept {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "x").unwrap();
        backdate(path, 400);
    }

    // Act
    let out = gc_at_fixed_now(&home, &["--yes"]);

    // Assert
    assert!(out.status.success(), "{out:?}");
    assert!(kept.iter().all(|path| path.exists()));
}
