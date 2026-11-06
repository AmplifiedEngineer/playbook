// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `playbook` binary started under the name `pbtop` runs the standalone
//! dashboard (ADR 0041), so a Homebrew `pbtop` link to `playbook` always has
//! the version of the binary it points to.

#![cfg(unix)]

#[path = "support/isolated.rs"]
mod isolated;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const PBTOP: &str = "pbtop";

/// A scratch folder holding a link named `link_name` to the built binary.
fn linked_as(link_name: &str) -> PathBuf {
    let dir = isolated::fresh_home().join("bin");
    std::fs::create_dir_all(&dir).expect("bin dir");
    let link = dir.join(link_name);
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_playbook"), &link).expect("symlink");
    link
}

fn run(link: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(link);
    isolated::isolate(&mut cmd, &isolated::fresh_home());
    cmd.args(args).output().expect("the link runs")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_link_named_pbtop_prints_the_pbtop_version() {
    let out = run(&linked_as(PBTOP), &["--version"]);

    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        stdout(&out),
        format!("pbtop {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn a_link_named_pbtop_shows_the_pbtop_help() {
    let out = run(&linked_as(PBTOP), &["--help"]);

    let help = stdout(&out);
    assert!(out.status.success(), "{out:?}");
    assert!(help.contains("Usage: pbtop"), "{help}");
    assert!(help.contains("--summary"), "{help}");
}

#[test]
fn a_link_named_pbtop_runs_a_pbtop_command() {
    let out = run(&linked_as(PBTOP), &["--summary"]);

    assert!(out.status.success(), "{out:?}");
    assert!(
        stdout(&out).contains("No usage recorded yet"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn a_link_named_pb_still_runs_playbook() {
    let out = run(&linked_as("pb"), &["--version"]);

    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        stdout(&out),
        format!("playbook {}\n", env!("CARGO_PKG_VERSION"))
    );
}
