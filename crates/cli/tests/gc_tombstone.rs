// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook gc` against a tombstone index planted by hand, spawning the real
//! binary with `$HOME` pinned to a scratch directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "playbook-gc-tombstone-{}-{tag}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&dir).expect("scratch home should be creatable");
    dir
}

fn gc_dir(home: &Path) -> PathBuf {
    home.join(".config/playbook/gc")
}

fn gc(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_playbook"))
        .arg("gc")
        .args(args)
        .env("HOME", home)
        .output()
        .expect("playbook should spawn")
}

/// Writes one index record in the given state whose data is not on disk.
fn plant_record(home: &Path, id: &str, state: &str) -> PathBuf {
    let gc_dir = gc_dir(home);
    fs::create_dir_all(&gc_dir).unwrap();
    let line = format!(
        r#"{{"id":"{id}","state":"{state}","at":1,"paths":["/nowhere/{id}"],"dest":"{dest}","size":1,"reason":"idle","tombstoned_at":1,"retain_days":7,"mode":"rename","source_mtime":1}}"#,
        dest = gc_dir.join("tombstones/2027-01-01").join(id).display()
    );
    let index = gc_dir.join("index.jsonl");
    fs::write(&index, format!("{line}\n")).unwrap();
    index
}

#[test]
fn dry_run_reports_an_anomaly_and_changes_nothing() {
    let home = scratch_home("dry");
    let index = plant_record(&home, "a1", "done");
    let before = fs::read(&index).unwrap();

    let out = gc(&home, &[]);

    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("a1: mark lost"));
    assert_eq!(fs::read(&index).unwrap(), before);
    assert!(!gc_dir(&home).join(".lock").exists());
}

#[test]
fn yes_repairs_the_anomaly_the_dry_run_reported() {
    let home = scratch_home("repair");
    let index = plant_record(&home, "a1", "done");

    let out = gc(&home, &["--yes"]);

    assert!(out.status.success(), "{out:?}");
    let text = fs::read_to_string(&index).unwrap();
    assert!(text.contains(r#""state":"lost""#), "{text}");
}

#[test]
fn restore_refuses_an_unknown_id() {
    let home = scratch_home("unknown");

    let out = gc(&home, &["--restore", "nope"]);

    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("no tombstone with id nope"));
}

#[test]
fn restore_reports_busy_while_another_run_holds_the_lock() {
    let home = scratch_home("busy");
    let lock = gc_dir(&home).join(".lock");
    fs::create_dir_all(&lock).unwrap();
    fs::write(lock.join("owner"), "1 live").unwrap();

    let out = gc(&home, &["--restore", "nope"]);

    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("busy"));
}
