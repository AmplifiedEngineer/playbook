// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for `playbook worktree review setup` and the setup then
//! teardown round trip, ported from the retired shell test.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "playbook-review-setup-{tag}-{}-{n}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&dir).expect("scratch dir");
    fs::canonicalize(&dir).expect("canonicalize")
}

fn git(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git")
}

fn git_ok(dir: &Path, args: &[&str]) -> String {
    let out = git(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A working repo, a bare clone standing in for GitHub, and the seed sha.
struct Fixture {
    repo: PathBuf,
    bare: PathBuf,
    sha: String,
}

impl Fixture {
    fn new() -> Self {
        let repo = scratch("repo");
        git_ok(&repo, &["init", "-q", "-b", "main"]);
        git_ok(&repo, &["config", "user.email", "t@t"]);
        git_ok(&repo, &["config", "user.name", "T"]);
        fs::write(repo.join("README"), "init\n").unwrap();
        git_ok(&repo, &["add", "."]);
        git_ok(&repo, &["commit", "-q", "-m", "initial"]);
        let sha = git_ok(&repo, &["rev-parse", "HEAD"]);
        let bare = scratch("bare");
        git_ok(
            &repo,
            &["clone", "--bare", "-q", ".", bare.to_str().unwrap()],
        );
        let fixture = Self { repo, bare, sha };
        fixture.set_pr_ref(7, &fixture.sha.clone());
        fixture
    }

    fn set_pr_ref(&self, pr: u32, sha: &str) {
        git_ok(
            &self.bare,
            &["update-ref", &format!("refs/pull/{pr}/head"), sha],
        );
    }

    /// Commit a change in the working repo, push the object to the bare, and
    /// return the new sha.
    fn new_commit(&self, line: &str) -> String {
        fs::write(self.repo.join("README"), format!("{line}\n")).unwrap();
        git_ok(&self.repo, &["commit", "-q", "-am", line]);
        let sha = git_ok(&self.repo, &["rev-parse", "HEAD"]);
        git_ok(
            &self.repo,
            &[
                "push",
                "-q",
                self.bare.to_str().unwrap(),
                "HEAD:refs/heads/side",
            ],
        );
        sha
    }

    fn playbook(&self, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_playbook"));
        cmd.args(["worktree", "review"])
            .args(args)
            .current_dir(&self.repo)
            .env("HOME", &self.repo)
            .env("GH_FETCH_URL", format!("file://{}", self.bare.display()))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null");
        for (k, v) in envs {
            cmd.env(k, v);
        }
        cmd.output().expect("playbook")
    }

    fn setup(&self, pr: &str, sha: &str) -> Output {
        self.playbook(&["setup", pr, sha], &[])
    }

    fn listed(&self, path: &str) -> bool {
        git_ok(&self.repo, &["worktree", "list", "--porcelain"])
            .lines()
            .any(|l| l == format!("worktree {path}"))
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn setup_creates_a_locked_detached_worktree_at_the_requested_head() {
    let f = Fixture::new();

    let out = f.setup("7", &f.sha);

    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let wt = stdout(&out);
    assert!(
        Path::new(&wt).is_absolute() && Path::new(&wt).is_dir(),
        "{wt}"
    );
    assert!(f.listed(&wt));
    let porcelain = git_ok(&f.repo, &["worktree", "list", "--porcelain"]);
    let block: Vec<&str> = porcelain
        .split("\n\n")
        .filter(|b| b.contains(&format!("worktree {wt}")))
        .collect();
    assert!(block[0].contains("\nlocked"), "not locked: {}", block[0]);
    assert_eq!(git_ok(Path::new(&wt), &["rev-parse", "HEAD"]), f.sha);
}

#[test]
fn setup_then_teardown_removes_the_worktree_and_teardown_is_idempotent() {
    let f = Fixture::new();
    let wt = stdout(&f.setup("7", &f.sha));

    let first = f.playbook(&["teardown", &wt], &[]);
    let second = f.playbook(&["teardown", &wt], &[]);

    assert!(first.status.success() && second.status.success());
    assert!(!Path::new(&wt).exists(), "directory still exists");
    assert!(!f.listed(&wt), "worktree still listed");
}

#[test]
fn teardown_of_a_path_that_never_existed_succeeds() {
    let f = Fixture::new();
    let ghost = f.repo.join("nonexistent-worktree-path");

    let out = f.playbook(&["teardown", ghost.to_str().unwrap()], &[]);

    assert!(out.status.success());
}

#[test]
fn a_second_setup_while_live_locked_is_refused() {
    let f = Fixture::new();
    assert!(f.setup("7", &f.sha).status.success());

    let out = f.setup("7", &f.sha);

    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("already in progress"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn setup_never_sweeps_an_unlocked_worktree_outside_review_worktrees() {
    let f = Fixture::new();
    let other = scratch("other").join("wu");
    git_ok(
        &f.repo,
        &[
            "worktree",
            "add",
            "--detach",
            "-q",
            other.to_str().unwrap(),
            &f.sha,
        ],
    );

    let out = f.setup("7", &f.sha);

    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(
        f.listed(other.to_str().unwrap()),
        "unrelated worktree was swept"
    );
}

#[test]
fn teardown_leaves_a_worktree_outside_review_worktrees_alone() {
    let f = Fixture::new();
    let other = scratch("other2").join("wu");
    git_ok(
        &f.repo,
        &[
            "worktree",
            "add",
            "--detach",
            "-q",
            other.to_str().unwrap(),
            &f.sha,
        ],
    );
    fs::write(other.join("dirty.txt"), "work\n").unwrap();

    let out = f.playbook(&["teardown", other.to_str().unwrap()], &[]);

    assert!(out.status.success());
    assert!(
        other.join("dirty.txt").exists(),
        "unrelated worktree was removed"
    );
}

#[test]
fn a_fresh_locked_worktree_for_another_pr_is_not_swept() {
    let f = Fixture::new();
    let sha8 = f.new_commit("pr8");
    f.set_pr_ref(8, &sha8);
    let pr7 = stdout(&f.setup("7", &f.sha));

    let out = f.setup("8", &sha8);

    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(f.listed(&pr7), "fresh PR 7 worktree was swept");
}

#[test]
fn setup_with_a_bogus_sha_fails() {
    let f = Fixture::new();

    let out = f.setup("7", "deadbeef000000000000000000000000deadbeef");

    assert!(!out.status.success());
}

#[test]
fn setup_works_from_another_cwd_via_git_dir_and_prints_an_absolute_path() {
    let f = Fixture::new();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_playbook"));
    cmd.args(["worktree", "review", "setup", "7", &f.sha])
        .current_dir(std::env::temp_dir())
        .env("GIT_DIR", f.repo.join(".git"))
        .env("HOME", &f.repo)
        .env("GH_FETCH_URL", format!("file://{}", f.bare.display()))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null");

    let out = cmd.output().unwrap();

    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let wt = stdout(&out);
    assert!(
        Path::new(&wt).is_absolute() && Path::new(&wt).is_dir(),
        "{wt}"
    );
}

#[test]
fn setup_warns_when_origin_moved_but_still_checks_out_the_requested_sha() {
    let f = Fixture::new();
    let new_sha = f.new_commit("v2");
    f.set_pr_ref(7, &new_sha);

    let out = f.setup("7", &f.sha);

    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("has moved"), "{}", stderr(&out));
    let wt = stdout(&out);
    assert_eq!(git_ok(Path::new(&wt), &["rev-parse", "HEAD"]), f.sha);
}

/// A directory holding an executable named `name` that runs `script`.
fn fake_bin(name: &str, script: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("fakebin");
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

#[test]
fn a_hung_gh_repo_view_times_out_instead_of_hanging() {
    let f = Fixture::new();
    let bin = fake_bin("gh", "sleep 60");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let started = std::time::Instant::now();

    let out = f.playbook(
        &["setup", "7", &f.sha],
        &[
            ("GH_FETCH_URL", ""),
            ("PATH", &path),
            ("REVIEW_NET_TIMEOUT_SECONDS", "1"),
        ],
    );

    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("timed out"),
        "stderr: {}",
        stderr(&out)
    );
    let waited = started.elapsed();
    assert!(
        waited >= std::time::Duration::from_secs(1),
        "the deadline was reached: {waited:?}"
    );
    assert!(waited < std::time::Duration::from_secs(20));
}

#[test]
fn a_hung_git_fetch_times_out_instead_of_hanging() {
    let f = Fixture::new();
    let real = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let real = String::from_utf8_lossy(&real.stdout).trim().to_string();
    let bin = fake_bin(
        "git",
        &format!("case \"$*\" in *fetch*) sleep 60;; esac\nexec {real} \"$@\""),
    );
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let started = std::time::Instant::now();

    let out = f.playbook(
        &["setup", "7", &f.sha],
        &[("PATH", &path), ("REVIEW_NET_TIMEOUT_SECONDS", "1")],
    );

    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("timed out or could not start"),
        "stderr: {}",
        stderr(&out)
    );
    let waited = started.elapsed();
    assert!(
        waited >= std::time::Duration::from_secs(1),
        "the deadline was reached: {waited:?}"
    );
    assert!(waited < std::time::Duration::from_secs(20));
}

#[test]
fn the_worktree_is_locked_with_the_pid_when_it_is_created() {
    let f = Fixture::new();

    let out = f.setup("7", &f.sha);

    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let wt = stdout(&out);
    let admin = git_ok(Path::new(&wt), &["rev-parse", "--git-dir"]);
    let reason = fs::read_to_string(Path::new(&admin).join("locked")).unwrap();
    assert!(reason.contains("review pr=7 pid="), "{reason}");
}

/// The time the binary reads as "now" in these tests, in epoch seconds.
const NOW: u64 = 1_800_000_000;
const HOUR: u64 = 3_600;
/// A pid that is never alive: the setup process that wrote a lock has exited.
const DEAD_PID: u32 = 999_999;

impl Fixture {
    /// A review worktree `<pr>-<sha7>` under the review folder, unlocked.
    fn review_worktree(&self, name: &str) -> PathBuf {
        let root = git_ok(
            &self.repo,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        );
        let dir = Path::new(&root).join("review-worktrees").join(name);
        fs::create_dir_all(dir.parent().unwrap()).unwrap();
        git_ok(
            &self.repo,
            &[
                "worktree",
                "add",
                "--detach",
                "-q",
                dir.to_str().unwrap(),
                &self.sha,
            ],
        );
        dir
    }

    fn lock(&self, worktree: &Path, reason: &str) {
        git_ok(
            &self.repo,
            &[
                "worktree",
                "lock",
                "--reason",
                reason,
                worktree.to_str().unwrap(),
            ],
        );
    }

    /// Sets the creation time the sweep reads: the modified time of the
    /// worktree's `.git` file.
    fn created_at(&self, worktree: &Path, epoch: u64) {
        let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(epoch);
        fs::File::options()
            .write(true)
            .open(worktree.join(".git"))
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    /// Runs `setup` for PR 7 with the binary's clock at `now`.
    fn setup_at(&self, now: u64) -> Output {
        self.playbook(
            &["setup", "7", &self.sha],
            &[("PLAYBOOK_TEST_NOW", &now.to_string())],
        )
    }
}

#[test]
fn a_stale_locked_review_worktree_is_swept_during_setup() {
    let f = Fixture::new();
    let stale = f.review_worktree("3-aaaaaaa");
    f.lock(
        &stale,
        &format!("review pr=3 pid={DEAD_PID} ts={}", NOW - 25 * HOUR),
    );

    let out = f.setup_at(NOW);

    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(Path::new(&stdout(&out)).is_absolute());
    assert!(
        !f.listed(stale.to_str().unwrap()),
        "stale worktree survived"
    );
}

#[test]
fn an_unlocked_leftover_under_review_worktrees_is_swept() {
    let f = Fixture::new();
    let leftover = f.review_worktree("3-abcdef1");
    f.created_at(&leftover, NOW - HOUR);

    let out = f.setup_at(NOW);

    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(!f.listed(leftover.to_str().unwrap()), "leftover survived");
}

#[test]
fn review_unlocked_boundary_60s() {
    let f = Fixture::new();
    let leftover = f.review_worktree("3-abcdef1");
    f.created_at(&leftover, NOW - 60);

    f.setup_at(NOW);

    assert!(!f.listed(leftover.to_str().unwrap()), "60 seconds goes");
}

#[test]
fn review_unlocked_one_second_under_60s_stays() {
    let f = Fixture::new();
    let leftover = f.review_worktree("3-abcdef1");
    f.created_at(&leftover, NOW - 59);

    f.setup_at(NOW);

    assert!(f.listed(leftover.to_str().unwrap()), "59 seconds stays");
}

#[test]
fn review_locked_dead_pid_boundary_24h() {
    let f = Fixture::new();
    let old = f.review_worktree("3-aaaaaaa");
    f.lock(
        &old,
        &format!("review pid={DEAD_PID} ts={}", NOW - 24 * HOUR),
    );

    f.setup_at(NOW);

    assert!(!f.listed(old.to_str().unwrap()), "24 hours goes");
}

#[test]
fn review_locked_one_second_under_24h_stays() {
    let f = Fixture::new();
    let young = f.review_worktree("3-aaaaaaa");
    f.lock(
        &young,
        &format!("review pid={DEAD_PID} ts={}", NOW - 24 * HOUR + 1),
    );

    f.setup_at(NOW);

    assert!(
        f.listed(young.to_str().unwrap()),
        "one second under 24 hours stays"
    );
}

#[test]
fn review_live_pid_stays() {
    let f = Fixture::new();
    let live = f.review_worktree("3-aaaaaaa");
    f.lock(
        &live,
        &format!("review pid={} ts={}", std::process::id(), NOW - 25 * HOUR),
    );

    f.setup_at(NOW);

    assert!(f.listed(live.to_str().unwrap()), "a live pid keeps it");
}

#[test]
fn review_dirty_tree_stays() {
    let f = Fixture::new();
    let dirty = f.review_worktree("3-aaaaaaa");
    fs::write(dirty.join("notes.txt"), "work\n").unwrap();
    f.created_at(&dirty, NOW - HOUR);

    f.setup_at(NOW);

    assert!(
        f.listed(dirty.to_str().unwrap()),
        "uncommitted work keeps it"
    );
}

#[test]
fn review_ts_beats_creation_time() {
    let f = Fixture::new();
    let old = f.review_worktree("3-aaaaaaa");
    f.created_at(&old, NOW - 10);
    f.lock(
        &old,
        &format!("review pid={DEAD_PID} ts={}", NOW - 25 * HOUR),
    );

    f.setup_at(NOW);

    assert!(
        !f.listed(old.to_str().unwrap()),
        "ts decides, not the young folder"
    );
}

#[test]
fn review_malformed_ts_falls_back() {
    let f = Fixture::new();
    let old = f.review_worktree("3-aaaaaaa");
    f.created_at(&old, NOW - 25 * HOUR);
    f.lock(&old, &format!("review pid={DEAD_PID} ts=soon"));

    f.setup_at(NOW);

    assert!(!f.listed(old.to_str().unwrap()), "creation time decides");
}

#[test]
fn review_age_falls_back_to_mtime() {
    assert_eq!(
        playbook::worktree::review_age_secs(None, Some(100), 150),
        50
    );
    assert_eq!(
        playbook::worktree::review_age_secs(Some(120), Some(100), 150),
        30
    );
    assert_eq!(playbook::worktree::review_age_secs(None, None, 150), 0);
}

#[test]
fn review_setup_sweep_ignores_other_worktrees() {
    let f = Fixture::new();
    let stale_review = f.review_worktree("3-aaaaaaa");
    f.created_at(&stale_review, NOW - 25 * HOUR);
    let other = scratch("other3").join("wu");
    git_ok(
        &f.repo,
        &[
            "worktree",
            "add",
            "--detach",
            "-q",
            other.to_str().unwrap(),
            &f.sha,
        ],
    );

    let out = f.setup_at(NOW);

    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(!f.listed(stale_review.to_str().unwrap()));
    assert!(
        f.listed(other.to_str().unwrap()),
        "only review worktrees go"
    );
    assert!(
        f.listed(f.repo.to_str().unwrap()),
        "the primary checkout stays"
    );
}

#[test]
fn setup_skips_sweep_when_locked() {
    // Arrange
    let f = Fixture::new();
    let leftover = f.review_worktree("3-abcdef1");
    f.created_at(&leftover, 1_000);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    fs::create_dir_all(f.repo.join(".config/playbook/gc/.lock")).unwrap();

    // Act
    let out = f.setup_at(now);

    // Assert
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(
        Path::new(&stdout(&out)).is_dir(),
        "the worktree is still made"
    );
    assert!(
        f.listed(leftover.to_str().unwrap()),
        "a busy gc run skips the sweep"
    );
}
