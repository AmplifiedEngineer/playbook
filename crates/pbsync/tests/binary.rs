// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The built `pbsync` binary, run with a temporary HOME so no real home is touched.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const SENTINEL: &str = "sentinel-token-77c1";

fn home() -> PathBuf {
    let home =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("home-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    home
}

fn pbsync(home: &PathBuf, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pbsync"))
        .args(args)
        .env("HOME", home)
        .env("PBSYNC_TOKEN", SENTINEL)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn enable_status_and_disable_run_end_to_end_without_leaking_the_token() {
    // Arrange
    let home = home();
    let file = home.join(".config/playbook/data/sync.json");

    // Act
    let declined = pbsync(&home, &["enable", "--url", "https://db.turso.io"], "no\n");
    let enabled = pbsync(
        &home,
        &["enable", "--url", "https://db.turso.io", "--account", "a"],
        "yes\n",
    );
    let status = pbsync(&home, &["status"], "");
    let disabled = pbsync(&home, &["disable"], "");

    // Assert
    assert_eq!(declined.status.code(), Some(1));
    assert_eq!(enabled.status.code(), Some(0));
    let status_text = String::from_utf8(status.stdout.clone()).unwrap();
    assert!(status_text.contains("sync: enabled") && status_text.contains("token: set"));
    assert_eq!(disabled.status.code(), Some(0));
    assert!(std::fs::read_to_string(&file)
        .unwrap()
        .contains("\"enabled\": false"));
    for output in [&declined, &enabled, &status, &disabled] {
        let text = [output.stdout.clone(), output.stderr.clone()].concat();
        assert!(!String::from_utf8_lossy(&text).contains(SENTINEL));
    }
    assert!(!std::fs::read_to_string(&file).unwrap().contains(SENTINEL));
}

#[test]
fn version_and_unknown_commands_keep_their_exit_codes() {
    // Arrange
    let home = home();

    // Act
    let version = pbsync(&home, &["--version"], "");
    let unknown = pbsync(&home, &["bogus"], "");

    // Assert
    assert_eq!(version.status.code(), Some(0));
    assert_eq!(unknown.status.code(), Some(2));
}
