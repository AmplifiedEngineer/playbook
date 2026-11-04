// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Regression tests for #631: the sweep removed brand new worktrees. A new
//! branch sits at a commit that is already an ancestor of the default branch,
//! so it read as "merged" before any work existed.

use playbook::worktree::PrState;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    container: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git should spawn")
}

fn git_ok(repo: &Path, args: &[&str]) {
    let out = git(repo, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn fixture(tag: &str) -> Fixture {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let container = std::env::temp_dir().join(format!(
        "playbook-wt-keep-{}-{tag}-{n}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&container).unwrap();
    let container = container.canonicalize().unwrap();
    let repo = container.join("repo");
    fs::create_dir_all(&repo).unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "T"],
        vec![
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widgets.git",
        ],
    ] {
        git_ok(&repo, &args);
    }
    fs::write(repo.join("README.md"), "seed\n").unwrap();
    git_ok(&repo, &["add", "."]);
    git_ok(&repo, &["commit", "-q", "-m", "seed"]);
    let head = String::from_utf8_lossy(&git(&repo, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    git_ok(&repo, &["update-ref", "refs/remotes/origin/main", &head]);
    let home = container.join("home");
    fs::create_dir_all(&home).unwrap();
    Fixture {
        container,
        repo: repo.canonicalize().unwrap(),
        home: home.canonicalize().unwrap(),
    }
}

/// A worktree in the agent-tool location on a new branch from `main`.
fn agent_worktree(f: &Fixture, name: &str) -> PathBuf {
    let dest = f.repo.join(".claude").join("worktrees").join(name);
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    git_ok(
        &f.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            name,
            dest.to_str().unwrap(),
            "main",
        ],
    );
    dest.canonicalize().unwrap()
}

fn age(path: &Path, days: u64) {
    let when = SystemTime::now() - Duration::from_secs(days * 86_400);
    fs::File::open(path.join(".git"))
        .unwrap()
        .set_modified(when)
        .unwrap();
}

fn sweep(f: &Fixture) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["worktree", "sweep"])
        .current_dir(&f.repo)
        .env("HOME", &f.home)
        .output()
        .expect("playbook should spawn");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn commit_in(path: &Path, file: &str) {
    fs::write(path.join(file), "work\n").unwrap();
    git_ok(path, &["add", "."]);
    git_ok(path, &["commit", "-q", "-m", "work"]);
}

fn cleanup(f: &Fixture) {
    let _ = fs::remove_dir_all(&f.container);
}

#[test]
fn a_brand_new_clean_worktree_is_kept() {
    let f = fixture("new-clean");
    let wt = agent_worktree(&f, "agent-new");
    let out = sweep(&f);
    assert!(wt.is_dir(), "a new worktree must survive the sweep: {out}");
    cleanup(&f);
}

#[test]
fn a_new_worktree_with_uncommitted_work_is_kept() {
    let f = fixture("new-dirty");
    let wt = agent_worktree(&f, "agent-dirty");
    fs::write(wt.join("work.txt"), "in progress\n").unwrap();
    let out = sweep(&f);
    assert!(wt.join("work.txt").is_file(), "{out}");
    cleanup(&f);
}

#[test]
fn an_old_worktree_with_uncommitted_work_is_kept() {
    let f = fixture("old-dirty");
    let wt = agent_worktree(&f, "agent-old-dirty");
    age(&wt, 90);
    fs::write(wt.join("work.txt"), "in progress\n").unwrap();
    let out = sweep(&f);
    assert!(wt.join("work.txt").is_file(), "{out}");
    assert!(out.contains("uncommitted"), "{out}");
    cleanup(&f);
}

#[test]
fn a_clean_branch_with_no_commits_is_kept_until_it_has_idled_past_the_stale_window() {
    let f = fixture("idle");
    let young = agent_worktree(&f, "agent-young");
    age(&young, 10);
    let old = agent_worktree(&f, "agent-old");
    age(&old, 40);
    let out = sweep(&f);
    assert!(
        young.is_dir(),
        "10 days idle is under the 30 day window: {out}"
    );
    assert!(
        !old.exists(),
        "40 days idle, clean, no commits: reaped: {out}"
    );
    cleanup(&f);
}

#[test]
fn a_branch_with_commits_not_merged_is_kept() {
    let f = fixture("unmerged");
    let wt = agent_worktree(&f, "agent-unmerged");
    commit_in(&wt, "feature.txt");
    age(&wt, 5);
    sweep(&f);
    assert!(wt.is_dir());
    cleanup(&f);
}

#[test]
fn a_merged_branch_with_its_own_commits_is_removed_and_logged() {
    let f = fixture("merged");
    let wt = agent_worktree(&f, "agent-merged");
    commit_in(&wt, "feature.txt");
    // Land it: main and origin/main move to the branch tip.
    git_ok(&f.repo, &["merge", "-q", "--ff-only", "agent-merged"]);
    let head = String::from_utf8_lossy(&git(&f.repo, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    git_ok(&f.repo, &["update-ref", "refs/remotes/origin/main", &head]);
    age(&wt, 1);
    let out = sweep(&f);
    assert!(!wt.exists(), "merged and idle over an hour: reaped: {out}");
    let log = fs::read_to_string(f.home.join(".config/playbook/worktree-sweep.log")).unwrap();
    assert!(
        log.contains("agent-merged") && log.contains("removed"),
        "{log}"
    );
    cleanup(&f);
}

#[test]
fn removing_a_worktree_removes_its_databases_from_the_data_folder() {
    let f = fixture("data-files");
    let wt = agent_worktree(&f, "agent-data");
    commit_in(&wt, "feature.txt");
    git_ok(&f.repo, &["merge", "-q", "--ff-only", "agent-data"]);
    let head = String::from_utf8_lossy(&git(&f.repo, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    git_ok(&f.repo, &["update-ref", "refs/remotes/origin/main", &head]);
    age(&wt, 1);
    let id: String = wt
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let data = f.home.join(".config/playbook/data");
    let db = |kind: &str, name: &str| {
        data.join(kind)
            .join("acme/widgets")
            .join(format!("{name}.db"))
    };
    for p in [
        db("gate", &id),
        db("code", &id),
        db("gate", "other-worktree"),
    ] {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "x").unwrap();
    }

    let out = sweep(&f);

    assert!(!wt.exists(), "{out}");
    assert!(!db("gate", &id).exists() && !db("code", &id).exists());
    assert!(
        db("gate", "other-worktree").exists(),
        "other worktrees keep theirs"
    );
    cleanup(&f);
}

#[test]
fn worktree_remove_takes_its_databases_and_an_id_shared_with_a_live_worktree_keeps_them() {
    let f = fixture("data-files-remove");
    let wt = agent_worktree(&f, "agent-rm");
    commit_in(&wt, "feature.txt");
    git_ok(&f.repo, &["merge", "-q", "--ff-only", "agent-rm"]);
    let head = String::from_utf8_lossy(&git(&f.repo, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    git_ok(&f.repo, &["update-ref", "refs/remotes/origin/main", &head]);
    age(&wt, 1);
    let id: String = wt
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let file = f
        .home
        .join(".config/playbook/data/gate/acme/widgets")
        .join(format!("{id}.db"));
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "x").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["worktree", "remove"])
        .arg(&wt)
        .current_dir(&f.repo)
        .env("HOME", &f.home)
        .output()
        .expect("playbook should spawn");

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!wt.exists());
    assert!(
        !file.exists(),
        "the removed worktree's database goes with it"
    );
    cleanup(&f);
}

#[test]
fn a_just_merged_branch_is_protected_while_its_worktree_is_new() {
    let f = fixture("merged-new");
    let wt = agent_worktree(&f, "agent-merged-new");
    commit_in(&wt, "feature.txt");
    git_ok(&f.repo, &["merge", "-q", "--ff-only", "agent-merged-new"]);
    let head = String::from_utf8_lossy(&git(&f.repo, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    git_ok(&f.repo, &["update-ref", "refs/remotes/origin/main", &head]);
    sweep(&f);
    assert!(
        wt.is_dir(),
        "created minutes ago: the grace period keeps it"
    );
    cleanup(&f);
}

#[test]
fn many_worktrees_are_judged_side_by_side_but_reported_in_list_order() {
    let f = fixture("order");
    // Eight worktrees: even ones have their own landed commit and are old
    // enough to be reaped, odd ones are brand new and must be kept.
    let names: Vec<String> = (0..8).map(|i| format!("agent-{i}")).collect();
    let paths: Vec<PathBuf> = names.iter().map(|n| agent_worktree(&f, n)).collect();
    for (i, wt) in paths.iter().enumerate() {
        if i % 2 == 0 {
            commit_in(wt, "own.txt");
        }
    }
    for i in (0..8).step_by(2) {
        git_ok(&f.repo, &["merge", "-q", "--no-edit", &names[i]]);
    }
    let head = String::from_utf8_lossy(&git(&f.repo, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    git_ok(&f.repo, &["update-ref", "refs/remotes/origin/main", &head]);
    for i in (0..8).step_by(2) {
        age(&paths[i], 1);
    }

    let dry = |f: &Fixture| {
        let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(["worktree", "sweep", "--dry-run"])
            .current_dir(&f.repo)
            .env("HOME", &f.home)
            .output()
            .expect("playbook should spawn");
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let first = dry(&f);
    // Every run reports the same lines in the same order.
    for _ in 0..4 {
        assert_eq!(dry(&f), first);
    }
    let lines: Vec<&str> = first.lines().collect();
    assert_eq!(lines.len(), 8, "{first}");
    for (i, line) in lines.iter().enumerate() {
        assert!(line.contains(&format!("agent-{i}")), "line {i}: {line}");
        if i % 2 == 0 {
            assert!(line.contains("would remove"), "line {i}: {line}");
        } else {
            assert!(line.contains("not landed"), "line {i}: {line}");
        }
    }
    cleanup(&f);
}

fn run_playbook(f: &Fixture, cwd: &Path, args: &[&str]) {
    let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", &f.home)
        .output()
        .expect("playbook should spawn");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn store_rows(f: &Fixture) -> Vec<String> {
    let store = f
        .home
        .join(".config/playbook/data/code/acme/widgets/store-v4.sqlite3");
    let conn = rusqlite::Connection::open(store).expect("store");
    let mut stmt = conn
        .prepare("SELECT key FROM worktrees ORDER BY key")
        .expect("prepare");
    stmt.query_map([], |r| r.get(0))
        .expect("rows")
        .map(|r| r.expect("key"))
        .collect()
}

#[test]
fn removing_a_worktree_drops_its_store_row_and_lock_and_keeps_the_sibling() {
    let f = fixture("store-row");
    let wt = agent_worktree(&f, "agent-store");
    commit_in(&wt, "feature.txt");
    git_ok(&f.repo, &["merge", "-q", "--ff-only", "agent-store"]);
    let head = String::from_utf8_lossy(&git(&f.repo, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    git_ok(&f.repo, &["update-ref", "refs/remotes/origin/main", &head]);
    run_playbook(&f, &wt, &["code", "index"]);
    run_playbook(&f, &f.repo, &["code", "index"]);
    assert_eq!(store_rows(&f).len(), 2);
    let data_dir = f.home.join(".config/playbook/data/code/acme/widgets");
    let locks_before = fs::read_dir(&data_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.ends_with(".lock") && n != "store-v4.lock"
        })
        .count();
    assert_eq!(locks_before, 2);
    age(&wt, 1);

    let out = sweep(&f);

    assert!(!wt.exists(), "{out}");
    let left = store_rows(&f);
    assert_eq!(left.len(), 1, "{left:?}");
    let dir = f.home.join(".config/playbook/data/code/acme/widgets");
    let locks: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".lock") && n != "store-v4.lock")
        .collect();
    assert_eq!(locks, vec![format!("{}.lock", left[0])]);
    cleanup(&f);
}

/// Sweeps `f` with a fake pull request lookup, so no test needs `gh`.
fn sweep_with_pr(f: &Fixture, branch: &str, state: Option<PrState>) -> String {
    let lines = playbook::worktree::sweep_with(
        &f.repo,
        &f.home,
        None,
        false,
        playbook::common::time::now_secs(),
        Some(&move |root: &Path, name: &str| {
            if root == f.repo.as_path() && name == branch {
                state
            } else {
                None
            }
        }),
    )
    .expect("sweep");
    lines.join("\n")
}

#[test]
fn a_squash_merged_branch_is_removed_once_its_pull_request_is_merged() {
    let f = fixture("squash");
    let wt = agent_worktree(&f, "agent-squash");
    commit_in(&wt, "feature.txt");
    age(&wt, 1);
    // The squash commit is not an ancestor of the branch, so git alone keeps it.
    let kept = sweep_with_pr(&f, "agent-squash", None);
    assert!(wt.is_dir(), "no PR information: kept as before: {kept}");
    let out = sweep_with_pr(&f, "agent-squash", Some(PrState::Merged));
    assert!(!wt.exists(), "merged PR: reaped: {out}");
    cleanup(&f);
}

#[test]
fn an_idle_branch_with_an_open_pull_request_is_kept() {
    let f = fixture("open-pr");
    let wt = agent_worktree(&f, "agent-open");
    age(&wt, 40);
    let out = sweep_with_pr(&f, "agent-open", Some(PrState::Open));
    assert!(wt.is_dir(), "an open PR protects an old branch: {out}");
    let out = sweep_with_pr(&f, "agent-open", None);
    assert!(
        !wt.exists(),
        "without the open PR the idle branch is reaped: {out}"
    );
    cleanup(&f);
}

#[test]
fn a_merged_pull_request_does_not_remove_a_dirty_worktree() {
    let f = fixture("merged-dirty");
    let wt = agent_worktree(&f, "agent-merged-dirty");
    commit_in(&wt, "feature.txt");
    age(&wt, 1);
    fs::write(wt.join("work.txt"), "in progress\n").unwrap();
    let out = sweep_with_pr(&f, "agent-merged-dirty", Some(PrState::Merged));
    assert!(wt.join("work.txt").is_file(), "{out}");
    assert!(out.contains("uncommitted"), "{out}");
    cleanup(&f);
}
