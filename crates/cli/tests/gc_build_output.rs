// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook gc --build-output`, spawning the real binary.

use std::fs;
use std::process::{Command, Stdio};

#[test]
fn build_output_without_terminal_is_rejected() {
    // Arrange
    let home = std::env::temp_dir().join(format!(
        "playbook-gc-build-output-{}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&home).unwrap();

    // Act: the test harness gives the binary no terminal on stdin.
    let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["gc", "--build-output", "--yes"])
        .env("HOME", &home)
        .stdin(Stdio::null())
        .output()
        .expect("playbook should spawn");

    // Assert
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("needs a terminal"));
}
