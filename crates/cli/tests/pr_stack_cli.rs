// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr stack` against a fake `gh` on PATH that serves the fixtures in
//! `tests/fixtures/pr_stack/`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

fn fixtures() -> PathBuf {
    PathBuf::from(env!("PLAYBOOK_REPO_ROOT")).join("crates/cli/tests/fixtures/pr_stack")
}

/// Runs `playbook pr stack <args>` with a `gh` that answers from the fixtures:
/// the GraphQL call returns `api`, a `pr view N` returns `pr-N.json`, and a
/// `pr list` returns `head-<branch>.json` or `base-<branch>.json` (else `[]`).
fn run(api: &str, args: &str) -> Output {
    let n = N.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("pb-stack-{}-{n}", playbook::testing::run_id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::create_dir_all(root.join("home")).unwrap();
    let script = format!(
        r#"#!/bin/sh
FX='{fx}'
case "$1 $2" in
  "repo view") cat "$FX/repo.json" ;;
  "api graphql") cat "$FX/{api}" ;;
  "pr view") cat "$FX/pr-$3.json" ;;
  "pr list")
    case "$5" in
      --head) f="$FX/head-$6.json" ;;
      *) f="$FX/base-$6.json" ;;
    esac
    if [ -f "$f" ]; then cat "$f"; else echo '[]'; fi ;;
  *) exit 1 ;;
esac
"#,
        fx = fixtures().display()
    );
    let gh = root.join("bin/gh");
    fs::write(&gh, script).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["pr", "stack", args])
        .env("HOME", root.join("home"))
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", root.join("bin").display()),
        )
        .current_dir(&root)
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn json_for_a_five_stack_lists_every_pr_and_asks() {
    let o = run("api-5.json", "3 --json");
    let v: serde_json::Value = serde_json::from_str(&text(&o)).expect("json");
    assert!(o.status.success());
    assert_eq!(v["source"], "api");
    assert_eq!(v["ask"], true);
    assert_eq!(v["prs"].as_array().unwrap().len(), 5);
    assert_eq!(v["prs"][0]["position"], 1);
}

#[test]
fn json_reports_a_stack_far_over_the_pr_limit() {
    // Arrange
    let args = "3 --json --max-prs 2";

    // Act
    let o = run("api-5.json", args);

    // Assert
    let v: serde_json::Value = serde_json::from_str(&text(&o)).expect("json");
    assert!(o.status.success());
    assert_eq!(v["far_over_budget"], true);
    assert_eq!(
        v["far_over_budget_reasons"],
        serde_json::json!(["5 open PRs, more than 2 times the limit of 2"])
    );
}

#[test]
fn a_branch_chain_is_found_without_the_api_field() {
    let o = run("api-none.json", "2 --json");
    let v: serde_json::Value = serde_json::from_str(&text(&o)).expect("json");
    assert_eq!(v["source"], "branch-chain");
    assert_eq!(v["open_count"], 3);
}

#[test]
fn a_pr_outside_any_stack_says_so_and_does_not_ask() {
    let o = run("api-none.json", "4 --json");
    let v: serde_json::Value = serde_json::from_str(&text(&o)).expect("json");
    assert_eq!(v["in_stack"], false);
    assert_eq!(v["ask"], false);
    assert_eq!(
        text(&run("api-none.json", "4")).trim(),
        "PR #4 is not part of a stack."
    );
}

#[test]
fn the_text_form_marks_the_current_pr() {
    let o = run("api-middle-merged.json", "3");
    let t = text(&o);
    assert!(t.contains("#3 [open]") && t.contains("<- this PR"), "{t}");
    assert!(t.contains("merged (context only): 2"), "{t}");
}

fn sandbox(gh_body: &str) -> (PathBuf, String) {
    let n = N.fetch_add(1, Ordering::Relaxed);
    let root =
        std::env::temp_dir().join(format!("pb-stack-map-{}-{n}", playbook::testing::run_id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("bin")).unwrap();
    let gh = root.join("bin/gh");
    fs::write(&gh, format!("#!/bin/sh\n{gh_body}\n")).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:/usr/bin:/bin", root.join("bin").display());
    (root, path)
}

#[test]
fn context_writes_a_diff_for_open_prs_only_and_map_posts_to_the_right_pr() {
    let (root, path) = sandbox(&format!(
        r#"FX='{fx}'
case "$1 $2" in
  "repo view") cat "$FX/repo.json" ;;
  "api graphql") cat "$FX/api-middle-merged.json" ;;
  "pr diff") printf 'diff --git a/f.rs b/f.rs\n--- a/f.rs\n+++ b/f.rs\n@@ -1,2 +1,3 @@\n a\n+b\n c\n' ;;
  "pr view")
    case "$4" in
      body,files) echo '{{"body":"why","files":[{{"path":"f.rs","additions":1,"deletions":0}}]}}' ;;
      *) echo '{{"headRefOid":"sha'"$3"'","baseRefName":"b'$(($3-1))'","state":"OPEN"}}' ;;
    esac ;;
  *) exit 1 ;;
esac"#,
        fx = fixtures().display()
    ));
    let pb = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(args)
            .env("HOME", &root)
            .env("PATH", &path)
            .current_dir(&root)
            .output()
            .unwrap()
    };

    let ctx = text(&pb(&["pr", "stack", "3 --context"]));
    let stack_file = ctx
        .lines()
        .find_map(|l| l.strip_prefix("stack_file="))
        .unwrap_or_else(|| panic!("no stack_file in {ctx}"));
    assert!(
        ctx.contains("diff_file_3=") && !ctx.contains("diff_file_1="),
        "{ctx}"
    );

    let findings = root.join("findings.json");
    fs::write(&findings, r#"[{"file":"f.rs","line":2,"body":"nit"}]"#).unwrap();
    let args = format!("map --stack {stack_file} --findings {}", findings.display());
    let o = pb(&["pr", "stack", &args]);
    let v: serde_json::Value = serde_json::from_str(&text(&o)).expect("json");
    assert_eq!(v["by_pr"]["4"]["comments"][0]["path"], "f.rs", "{v}");
}
