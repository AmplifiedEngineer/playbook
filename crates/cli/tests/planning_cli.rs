// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook plan checkpoint` and `glossary add`: the helpers
//! that replaced bash blocks in `plan.md`, `implement.md` and `deep-pass.md`.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

struct Sandbox {
    root: PathBuf,
    repo: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("pb-plan-{tag}-{}-{n}", playbook::testing::run_id()));
        let _ = fs::remove_dir_all(&root);
        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        for args in [
            vec!["init", "-q"],
            vec![
                "remote",
                "add",
                "origin",
                "https://github.com/acme/widgets.git",
            ],
        ] {
            let o = Command::new("git")
                .args(&args)
                .current_dir(&repo)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(o.status.success());
        }
        Sandbox { root, repo }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_playbook"));
        c.args(args)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("home/.config"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .current_dir(&self.repo);
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn run_stdin(&self, args: &[&str], input: &str) -> Output {
        let mut child = self
            .cmd(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn a_checkpoint_is_not_found_until_it_is_written_then_found_with_its_content() {
    let s = Sandbox::new("cp");
    let first = out(&s.run(&["plan", "checkpoint", "proj-123"]));
    let mut lines = first.lines();
    let path = PathBuf::from(lines.next().unwrap());
    assert!(path.ends_with("plans/proj-123.checkpoint.md"), "{first}");
    assert_eq!(lines.next(), Some("not-found"));

    let w = s.run_stdin(
        &["plan", "checkpoint", "proj-123", "--write"],
        "Goal: ship it\n",
    );
    assert!(w.status.success());
    assert_eq!(out(&w).trim(), path.display().to_string());
    assert_eq!(fs::read_to_string(&path).unwrap(), "Goal: ship it\n");

    let again = out(&s.run(&["plan", "checkpoint", "proj-123"]));
    assert!(
        again.ends_with("found\n") && !again.ends_with("not-found\n"),
        "{again}"
    );
}

#[test]
fn a_second_write_replaces_the_first() {
    let s = Sandbox::new("cp2");
    s.run_stdin(&["plan", "checkpoint", "t", "--write"], "one");
    let w = s.run_stdin(&["plan", "checkpoint", "t", "--write"], "two");
    assert_eq!(fs::read_to_string(out(&w).trim()).unwrap(), "two");
}

#[test]
fn a_slug_that_escapes_the_plans_folder_is_refused() {
    let s = Sandbox::new("cp3");
    let o = s.run(&["plan", "checkpoint", "../evil"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("invalid topic slug"));
}

#[test]
fn glossary_add_creates_the_file_then_appends() {
    let s = Sandbox::new("gl");
    s.run(&["glossary", "add", "- **Seat**: one licence"]);
    let o = s.run(&["glossary", "add", "- **Tenant**: one customer"]);
    assert!(o.status.success());
    let text = fs::read_to_string(s.repo.join("GLOSSARY.md")).unwrap();
    assert_eq!(
        text,
        "- **Seat**: one licence\n- **Tenant**: one customer\n"
    );
}

fn plans_dir(s: &Sandbox) -> PathBuf {
    let o = s.run(&["path", "plans"]);
    PathBuf::from(out(&o).trim())
}

fn seed_plans(s: &Sandbox) -> PathBuf {
    let d = plans_dir(s);
    fs::create_dir_all(&d).unwrap();
    for n in [
        "demo.md",
        "demo-quality.md",
        "demo.checkpoint.md",
        "demo.gate-source.md",
        "demo.gate-source.test-review.md",
    ] {
        fs::write(d.join(n), "x").unwrap();
    }
    fs::create_dir(d.join("demo.checkpoint.md.lock")).unwrap();
    d
}

#[test]
fn plan_cleanup_removes_scratch_only_and_is_idempotent() {
    let s = Sandbox::new("clean");
    let d = seed_plans(&s);
    let dry = s.run(&["plan", "cleanup", "demo", "--dry-run"]);
    assert!(dry.status.success());
    assert!(d.join("demo.checkpoint.md").is_file());
    let o = s.run(&["plan", "cleanup", "demo"]);
    assert!(o.status.success());
    assert_eq!(out(&o).lines().count(), 4, "{}", out(&o));
    assert!(d.join("demo.md").is_file() && d.join("demo-quality.md").is_file());
    assert!(!d.join("demo.checkpoint.md.lock").exists());
    let again = s.run(&["plan", "cleanup", "demo", "--json"]);
    assert!(again.status.success());
    assert!(out(&again).contains("\"files\":[]"), "{}", out(&again));
}

#[test]
fn plan_cleanup_rejects_bad_slugs() {
    let s = Sandbox::new("cleanbad");
    seed_plans(&s);
    for slug in ["../demo", "a/b", ".hidden", ".."] {
        let o = s.run(&["plan", "cleanup", slug]);
        assert_eq!(o.status.code(), Some(1), "{slug}");
    }
}

#[test]
fn plan_cleanup_stale_needs_yes() {
    let s = Sandbox::new("cleanstale");
    let d = seed_plans(&s);
    fs::write(d.join("orphan.checkpoint.md"), "x").unwrap();
    let o = s.run(&["plan", "cleanup", "--stale"]);
    assert!(o.status.success());
    assert!(d.join("demo.checkpoint.md").is_file());
    let o = s.run(&["plan", "cleanup", "--stale", "--yes", "--older-than", "0"]);
    assert!(o.status.success());
    assert!(!d.join("demo.checkpoint.md").exists());
    assert!(d.join("orphan.checkpoint.md").is_file());
}

#[test]
fn plans_picker_ignores_scratch_files() {
    let s = Sandbox::new("picker");
    seed_plans(&s);
    let o = out(&s.run(&["plans"]));
    assert!(o.contains("demo.md"), "{o}");
    assert!(
        !o.contains("checkpoint") && !o.contains("gate-source") && !o.contains("quality"),
        "{o}"
    );
}
