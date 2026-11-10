// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook adr next`, `learn preflight` and `pr comments`: the helpers that
//! replaced bash blocks in `adr.md`, `learn-project.md` and
//! `pr-comments.md`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

struct Sandbox {
    root: PathBuf,
    repo: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "pb-helpers-{tag}-{}-{n}",
            playbook::testing::run_id()
        ));
        let _ = fs::remove_dir_all(&root);
        let repo = root.join("repo");
        for d in ["repo", "bin", "home"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        let git = |args: &[&str]| {
            let o = Command::new("git")
                .args(args)
                .current_dir(&repo)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(o.status.success(), "{args:?}");
        };
        git(&["init", "-q"]);
        git(&[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widgets.git",
        ]);
        git(&["commit", "-q", "--allow-empty", "-m", "ABC-1 first"]);
        git(&[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "ABC-2 second and XYZ-9",
        ]);
        Sandbox { root, repo }
    }

    /// A fake `gh` that runs `script`.
    fn gh(&self, script: &str) {
        let f = self.root.join("bin/gh");
        fs::write(&f, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(args)
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
            .current_dir(&self.repo)
            .output()
            .unwrap()
    }
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn adr_next_starts_at_one_then_follows_the_highest_number() {
    let s = Sandbox::new("adr");
    let first = text(&s.run(&["adr", "next"]));
    assert!(first.starts_with("Next number: 0001   Date: 20"), "{first}");
    let dir = s.repo.join("docs/adr");
    assert!(dir.is_dir());
    fs::write(dir.join("0004-x.md"), "").unwrap();
    assert!(text(&s.run(&["adr", "next"])).starts_with("Next number: 0005"));
}

#[test]
fn adr_next_outside_a_repo_fails() {
    let s = Sandbox::new("adr-norepo");
    let o = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["adr", "next"])
        .env("HOME", s.root.join("home"))
        .env("GIT_CEILING_DIRECTORIES", &s.root)
        .current_dir(s.root.join("home"))
        .output()
        .unwrap();
    assert!(!o.status.success());
}

#[test]
fn learn_preflight_names_the_repo_from_the_origin_when_gh_is_unavailable() {
    let s = Sandbox::new("learn");
    s.gh("exit 1");
    let t = text(&s.run(&["learn", "preflight"]));
    assert!(t.contains("Repo:    acme/widgets"), "{t}");
    assert!(t.contains("Commits: 2"), "{t}");
    assert!(t.contains("gh:   UNAVAILABLE (PRs skipped)"), "{t}");
    assert!(t.contains("acli: absent"), "{t}");
    assert!(t.contains("/.config/playbook/memory/acme/widgets"), "{t}");
    assert!(
        t.contains("      2 ABC") && t.contains("      1 XYZ"),
        "{t}"
    );
}

#[test]
fn learn_preflight_prefers_the_name_gh_reports() {
    let s = Sandbox::new("learn-gh");
    s.gh(r#"case "$*" in "repo view"*) echo real/name;; esac"#);
    let t = text(&s.run(&["learn", "preflight"]));
    assert!(t.contains("Repo:    real/name"), "{t}");
    assert!(t.contains("gh:   ok"), "{t}");
}

#[test]
fn pr_comments_resolves_the_pr_and_writes_both_files() {
    let s = Sandbox::new("comments");
    s.gh(r#"case "$*" in
  "repo view"*) echo o/r;;
  "pr view 12 --json headRefOid"*) echo abc123;;
  "api /user"*) echo me;;
  "api graphql"*) echo '{"data":{}}';;
  "api /repos/o/r/issues/12/comments"*) echo '[]';;
esac"#);
    let o = s.run(&["pr", "comments", "#12 --bots -y"]);
    let t = text(&o);
    assert!(
        o.status.success(),
        "{t}{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(
        t.contains("PR: o/r#12") && t.contains("Head SHA: abc123") && t.contains("Me: me"),
        "{t}"
    );
    assert!(
        t.contains("Flags: bots=true dry-run=false auto-commit=true"),
        "{t}"
    );
    assert_eq!(
        fs::read_to_string("/tmp/pr-comments-12-threads.json")
            .unwrap()
            .trim(),
        r#"{"data":{}}"#
    );
    assert_eq!(
        fs::read_to_string("/tmp/pr-comments-12-issues.json")
            .unwrap()
            .trim(),
        "[]"
    );
}

#[test]
fn pr_comments_without_a_pr_uses_the_current_branch_or_fails() {
    let s = Sandbox::new("comments-none");
    s.gh("exit 1");
    let o = s.run(&["pr", "comments"]);
    assert!(!o.status.success());
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("no PR for current branch"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}
