// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr land` through the binary with a fake `gh`. Only the verdict
//! and the exit code are asserted. `PB_LAND_MAX_POLLS` ends the wait after a
//! few polls instead of a wall clock timeout.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fake_gh(tag: &str, view_state: &str, check_bucket: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pb-land-{tag}-{}", playbook::testing::run_id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let script = format!(
        r#"#!/bin/bash
echo "$*" >> "{log}"
case "$1 $2" in
  "pr view") printf '{{"state":"{state}","isDraft":false,"headRefOid":"abc","baseRefName":"main","mergeStateStatus":"CLEAN","reviewDecision":"","autoMergeRequest":null}}';;
  "pr checks") printf '[{{"name":"t","workflow":"ci","bucket":"{bucket}","startedAt":"2026-01-01T00:00:01Z"}}]';;
  "pr list") printf '[]';;
  "pr merge") echo armed;;
  *) echo "unexpected gh $*" >&2; exit 1;;
esac
"#,
        log = dir.join("log").display(),
        state = view_state,
        bucket = check_bucket
    );
    let gh = dir.join("gh");
    fs::write(&gh, script).unwrap();
    Command::new("chmod").arg("+x").arg(&gh).status().unwrap();
    dir
}

fn land(dir: &Path) -> (i32, String) {
    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["pr", "land", "7", "--settle", "0", "--interval", "0"])
        .env("PATH", path)
        .env("HOME", dir)
        .env("PB_LAND_MAX_POLLS", "3")
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn a_merged_pr_exits_zero() {
    let dir = fake_gh("merged", "MERGED", "pass");
    let (code, out) = land(&dir);
    assert_eq!(code, 0, "{out}");
    assert!(out.starts_with("LAND=MERGED"), "{out}");
}

#[test]
fn an_open_pr_that_never_merges_is_still_waiting_with_exit_7() {
    let dir = fake_gh("waiting", "OPEN", "pass");
    let (code, out) = land(&dir);
    assert_eq!(code, 7, "{out}");
    assert!(out.starts_with("LAND=STILL_WAITING"), "{out}");
    let log = fs::read_to_string(dir.join("log")).unwrap();
    assert!(!log.contains("--admin") && !log.contains("--delete-branch"));
    assert!(log.contains("--auto --match-head-commit abc"));
}

#[test]
fn a_failing_check_exits_6() {
    let dir = fake_gh("fail", "OPEN", "fail");
    let (code, out) = land(&dir);
    assert_eq!(code, 6, "{out}");
    assert!(out.starts_with("LAND=FAIL"), "{out}");
}
