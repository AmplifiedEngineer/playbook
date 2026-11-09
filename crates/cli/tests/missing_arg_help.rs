// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A missing argument prints that command's help and one error line, from the
//! real binary.

#[path = "support/isolated.rs"]
mod isolated;

use std::process::Output;

fn assert_names_missing(stderr: &str, name: &str) {
    let line = stderr.trim_end().lines().last().expect("a last line");
    assert!(
        line.starts_with("error: the following required arguments were not provided:"),
        "{line}"
    );
    assert!(line.contains(name), "{line}");
}

fn run(args: &[&str]) -> Output {
    isolated::playbook()
        .args(args)
        .env("HTTPS_PROXY", "127.0.0.1:9")
        .env("HTTP_PROXY", "127.0.0.1:9")
        .output()
        .expect("spawn playbook")
}

#[test]
fn a_missing_positional_prints_long_help_then_one_error_line() {
    // Arrange
    let args = ["trust"];

    // Act
    let out = run(&args);

    // Assert
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr.contains("Usage: playbook trust <PATH>"), "{stderr}");
    assert!(stderr.contains("Absolute path"), "{stderr}");
    assert_eq!(
        stderr.trim_end().lines().last(),
        Some("error: the following required arguments were not provided: <PATH>")
    );
}

#[test]
fn a_flag_before_the_missing_positional_does_not_hide_the_command() {
    // Arrange
    let args = ["route", "--tier", "high"];

    // Act
    let out = run(&args);

    // Assert
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr.contains("Usage: playbook route"), "{stderr}");
    assert!(stderr.contains("mechanical, classify"), "{stderr}");
    assert_names_missing(&stderr, "<KIND>");
}

#[test]
fn a_missing_nested_positional_or_flag_names_the_missing_argument() {
    // Arrange
    let rows: [(&[&str], &str); 3] = [
        (&["config", "get"], "KEY"),
        (&["config", "set", "KEY"], "VALUE"),
        (&["pr", "reply"], "--pr <PR>"),
    ];
    for (args, missing) in rows {
        // Act
        let out = run(args);

        // Assert
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert_names_missing(&stderr, missing);
    }
}
