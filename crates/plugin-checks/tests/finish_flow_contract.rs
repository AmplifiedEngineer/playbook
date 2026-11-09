// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The finish skill calls the two deterministic commands and keeps its old
//! flow only in a reference file.

use std::fs;
use std::path::PathBuf;

fn skill_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/playbook/skills/finish-pull-request")
}

fn read(rel: &str) -> String {
    fs::read_to_string(skill_dir().join(rel)).expect(rel)
}

#[test]
fn skill_uses_finish_and_land() {
    let s = read("SKILL.md");
    assert!(s.contains("playbook pr status"));
    assert!(s.contains("playbook pr land"));
    assert!(s.lines().count() <= 60, "skill grew past 60 lines");
}

#[test]
fn skill_never_allows_admin_or_branch_delete() {
    let s = read("SKILL.md");
    assert!(s.contains("Never use `--admin` or `--delete-branch`"));
    assert!(!s.contains("gh pr merge"));
}

#[test]
fn legacy_flow_lives_in_a_reference() {
    let l = read("references/legacy-land.md");
    assert!(l.contains("pr-land-wait"));
    assert!(l.contains("Never `--admin`"));
}
