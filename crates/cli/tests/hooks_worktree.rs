// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Tests for the `worktree-create` and `worktree-remove` hooks, spawning the
//! real binary against real scratch git repos. Env is set per child process,
//! never on the test process, so no env lock is needed.

#[path = "support/isolated.rs"]
mod isolated;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A scratch `<root>/proj/<repo>` checkout with a bare `origin`, plus the root.
struct Fixture {
    root: PathBuf,
    repo: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn git(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("git should spawn")
}

fn git_ok(dir: &Path, args: &[&str]) -> String {
    let out = git(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn fixture(tag: &str) -> Fixture {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!(
            "playbook-wt-hook-{}-{tag}-{n}",
            playbook::testing::run_id()
        ));
    let _ = fs::remove_dir_all(&root);
    let repo = root.join("proj").join("app");
    let origin = root.join("origin.git");
    fs::create_dir_all(&repo).unwrap();
    git_ok(
        &root,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            origin.to_str().unwrap(),
        ],
    );
    git_ok(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("README.md"), "seed\n").unwrap();
    git_ok(&repo, &["add", "."]);
    git_ok(&repo, &["commit", "-q", "-m", "seed"]);
    git_ok(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git_ok(&repo, &["push", "-q", "-u", "origin", "main"]);
    Fixture { root, repo }
}

fn run_hook(name: &str, payload: &str) -> Output {
    let mut child = isolated::playbook()
        .args(["hook", name])
        .env_remove("CI")
        .env_remove("PLAYBOOK_HEADLESS")
        .env_remove("HOOK_INPUT")
        .env_remove("WORKTREE_BASE_DIR")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("playbook should spawn");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn create(repo: &Path, name: &str) -> Output {
    let payload = format!(
        r#"{{"hook_event_name":"WorktreeCreate","cwd":"{}","worktree_name":"{name}"}}"#,
        repo.display()
    );
    run_hook("worktree-create", &payload)
}

fn remove(cwd: &Path, path: &Path) -> Output {
    let payload = format!(
        r#"{{"hook_event_name":"WorktreeRemove","cwd":"{}","worktree_path":"{}"}}"#,
        cwd.display(),
        path.display()
    );
    run_hook("worktree-remove", &payload)
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn created_path(out: &Output) -> PathBuf {
    assert!(out.status.success(), "create failed: {}", stderr(out));
    PathBuf::from(stdout(out).trim_end_matches('\n'))
}

#[test]
fn create_uses_cc_launcher_location_and_prints_only_the_path() {
    let f = fixture("create");
    let out = create(&f.repo, "probe");
    let expected = f.root.join("proj/.worktrees/app/probe");
    assert_eq!(stdout(&out), format!("{}\n", expected.display()));
    assert!(expected.join("README.md").exists());
    assert_eq!(
        git_ok(&expected, &["symbolic-ref", "--short", "HEAD"]).trim(),
        "worktree-probe"
    );
    assert!(git_ok(&f.repo, &["worktree", "list"]).contains("probe"));
}

#[test]
fn create_twice_reuses_the_registered_worktree() {
    let f = fixture("reuse");
    let first = created_path(&create(&f.repo, "probe"));
    fs::write(first.join("scratch.txt"), "keep me\n").unwrap();
    let second = created_path(&create(&f.repo, "probe"));
    assert_eq!(first, second);
    assert!(second.join("scratch.txt").exists());
}

#[test]
fn create_rejects_traversal_and_separator_names() {
    let f = fixture("sanitize");
    for bad in ["..", "../escape", "a/b", "", "-rf", "a b", "x~y"] {
        let out = create(&f.repo, bad);
        assert!(!out.status.success(), "{bad:?} should fail");
        assert!(
            stdout(&out).is_empty(),
            "stdout must stay empty for {bad:?}"
        );
    }
    assert!(!f.root.join("proj/.worktrees").exists());
    assert!(!f.repo.join(".claude").exists());
}

#[test]
fn create_falls_back_to_claude_worktrees_when_base_dir_is_a_file() {
    let f = fixture("fallback");
    fs::write(f.root.join("proj/.worktrees"), "not a dir").unwrap();
    let out = create(&f.repo, "probe");
    let path = created_path(&out);
    assert_eq!(path, f.repo.join(".claude/worktrees/probe"));
    assert!(path.join("README.md").exists());
    assert!(stderr(&out).contains("falling back"));
    assert_eq!(stdout(&out), format!("{}\n", path.display()));
}

#[test]
fn create_uses_payload_base_commit_when_given() {
    let f = fixture("basecommit");
    let seed = git_ok(&f.repo, &["rev-parse", "HEAD"]).trim().to_string();
    fs::write(f.repo.join("later.txt"), "x\n").unwrap();
    git_ok(&f.repo, &["add", "."]);
    git_ok(&f.repo, &["commit", "-q", "-m", "later"]);
    let payload = format!(
        r#"{{"cwd":"{}","worktree_name":"old","base_commit":"{seed}"}}"#,
        f.repo.display()
    );
    let path = created_path(&run_hook("worktree-create", &payload));
    assert!(!path.join("later.txt").exists());
}

#[test]
fn remove_deletes_a_clean_pushed_worktree_and_its_branch() {
    let f = fixture("rm-clean");
    let path = created_path(&create(&f.repo, "done"));
    let out = remove(&f.repo, &path);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!path.exists());
    assert!(git_ok(&f.repo, &["branch", "--list", "worktree-done"])
        .trim()
        .is_empty());
}

#[test]
fn remove_keeps_a_worktree_with_uncommitted_changes() {
    let f = fixture("rm-dirty");
    let path = created_path(&create(&f.repo, "dirty"));
    fs::write(path.join("untracked.txt"), "wip\n").unwrap();
    let out = remove(&f.repo, &path);
    assert!(out.status.success());
    assert!(path.exists());
    assert!(stderr(&out).contains("uncommitted"));
}

#[test]
fn remove_keeps_a_worktree_with_unpushed_commits() {
    let f = fixture("rm-unpushed");
    let path = created_path(&create(&f.repo, "ahead"));
    fs::write(path.join("new.txt"), "work\n").unwrap();
    git_ok(&path, &["add", "."]);
    git_ok(&path, &["commit", "-q", "-m", "local only"]);
    let out = remove(&f.repo, &path);
    assert!(out.status.success());
    assert!(path.exists());
    assert!(stderr(&out).contains("commits"));
    assert!(!git_ok(&f.repo, &["branch", "--list", "worktree-ahead"])
        .trim()
        .is_empty());
}

#[test]
fn remove_deletes_after_the_branch_is_pushed() {
    let f = fixture("rm-pushed");
    let path = created_path(&create(&f.repo, "shipped"));
    fs::write(path.join("new.txt"), "work\n").unwrap();
    git_ok(&path, &["add", "."]);
    git_ok(&path, &["commit", "-q", "-m", "work"]);
    git_ok(&path, &["push", "-q", "origin", "worktree-shipped"]);
    let out = remove(&f.repo, &path);
    assert!(out.status.success());
    assert!(!path.exists());
    assert!(git_ok(&f.repo, &["branch", "--list", "worktree-shipped"])
        .trim()
        .is_empty());
}

#[test]
fn remove_refuses_a_non_worktree_path_and_the_main_worktree() {
    let f = fixture("rm-refuse");
    let stranger = f.root.join("stranger");
    fs::create_dir_all(&stranger).unwrap();
    fs::write(stranger.join("f.txt"), "x").unwrap();

    let out = remove(&f.repo, &stranger);
    assert!(out.status.success());
    assert!(stranger.join("f.txt").exists());
    assert!(stderr(&out).contains("not a worktree"));

    let out = remove(&f.repo, &f.repo);
    assert!(out.status.success());
    assert!(f.repo.join("README.md").exists());
    assert!(stderr(&out).contains("main worktree"));
}

#[test]
fn create_recreates_a_worktree_whose_folder_was_deleted() {
    let f = fixture("stale");
    let first = created_path(&create(&f.repo, "gone"));
    fs::remove_dir_all(&first).unwrap();
    let second = created_path(&create(&f.repo, "gone"));
    assert_eq!(first, second);
    assert!(second.join("README.md").exists());
}

#[test]
fn create_reattaches_a_kept_branch_after_the_worktree_was_removed() {
    let f = fixture("reattach");
    let path = created_path(&create(&f.repo, "probe"));
    fs::write(path.join("kept.txt"), "work\n").unwrap();
    git_ok(&path, &["add", "."]);
    git_ok(&path, &["commit", "-q", "-m", "kept"]);
    git_ok(&f.repo, &["worktree", "remove", path.to_str().unwrap()]);
    let again = created_path(&create(&f.repo, "probe"));
    assert_eq!(path, again);
    assert!(again.join("kept.txt").exists());
}

#[test]
fn create_bases_on_origin_head_not_unpushed_local_commits() {
    let f = fixture("originhead");
    git_ok(&f.repo, &["remote", "set-head", "origin", "main"]);
    fs::write(f.repo.join("local-only.txt"), "x\n").unwrap();
    git_ok(&f.repo, &["add", "."]);
    git_ok(&f.repo, &["commit", "-q", "-m", "local only"]);
    let path = created_path(&create(&f.repo, "fresh"));
    assert!(!path.join("local-only.txt").exists());
}

#[test]
fn create_ignores_an_option_shaped_base_commit_and_needs_an_absolute_cwd() {
    let f = fixture("inputs");
    let payload = format!(
        r#"{{"cwd":"{}","name":"vianame","base_commit":"--orphan"}}"#,
        f.repo.display()
    );
    let path = created_path(&run_hook("worktree-create", &payload));
    assert!(path.join("README.md").exists());
    let out = run_hook("worktree-create", r#"{"cwd":"","worktree_name":"x"}"#);
    assert!(!out.status.success());
    assert!(stdout(&out).is_empty());
}

#[test]
fn remove_works_without_a_usable_cwd() {
    let f = fixture("rm-nocwd");
    let path = created_path(&create(&f.repo, "nocwd"));
    let payload = format!(r#"{{"cwd":"","worktree_path":"{}"}}"#, path.display());
    let out = run_hook("worktree-remove", &payload);
    assert!(out.status.success());
    assert!(!path.exists());
}

#[test]
fn remove_keeps_a_locked_worktree_and_leaves_branches_alone_when_detached() {
    let f = fixture("rm-locked");
    let locked = created_path(&create(&f.repo, "locked"));
    git_ok(&f.repo, &["worktree", "lock", locked.to_str().unwrap()]);
    let out = remove(&f.repo, &locked);
    assert!(out.status.success());
    assert!(locked.exists());

    let detached = f.root.join("detached");
    git_ok(
        &f.repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            detached.to_str().unwrap(),
        ],
    );
    let before = git_ok(&f.repo, &["branch", "--list"]);
    let out = remove(&f.repo, &detached);
    assert!(out.status.success());
    assert!(!detached.exists());
    assert_eq!(before, git_ok(&f.repo, &["branch", "--list"]));
}

#[test]
fn remove_never_deletes_a_branch_the_hook_did_not_create() {
    let f = fixture("rm-foreign");
    let path = f.root.join("launcher-made");
    git_ok(
        &f.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feat/mine",
            path.to_str().unwrap(),
        ],
    );
    let out = remove(&f.repo, &path);
    assert!(out.status.success());
    assert!(!path.exists());
    assert!(!git_ok(&f.repo, &["branch", "--list", "feat/mine"])
        .trim()
        .is_empty());
}

fn ignore_env_files(repo: &Path) {
    fs::write(repo.join(".gitignore"), ".env*\nnode_cache/\n").unwrap();
    git_ok(repo, &["add", ".gitignore"]);
    git_ok(repo, &["commit", "-q", "-m", "ignore"]);
}

#[test]
fn create_copies_ignored_files_named_by_worktreeinclude() {
    let f = fixture("include");
    ignore_env_files(&f.repo);
    fs::write(f.repo.join(".env"), "A=1\n").unwrap();
    fs::create_dir_all(f.repo.join("node_cache/sub")).unwrap();
    fs::write(f.repo.join("node_cache/sub/x.bin"), "bin").unwrap();
    fs::write(f.repo.join("plain.txt"), "not ignored\n").unwrap();
    fs::write(
        f.repo.join(".worktreeinclude"),
        ".env*\nnode_cache/\nplain.txt\nREADME.md\n",
    )
    .unwrap();
    let out = create(&f.repo, "inc");
    let path = created_path(&out);
    assert_eq!(fs::read_to_string(path.join(".env")).unwrap(), "A=1\n");
    assert!(path.join("node_cache/sub/x.bin").exists());
    // Not ignored by git, so never duplicated.
    assert!(!path.join("plain.txt").exists());
    assert!(stderr(&out).contains("copied 2 file(s)"));
    assert_eq!(stdout(&out), format!("{}\n", path.display()));
}

#[test]
fn create_without_worktreeinclude_copies_nothing() {
    let f = fixture("noinclude");
    ignore_env_files(&f.repo);
    fs::write(f.repo.join(".env"), "A=1\n").unwrap();
    let path = created_path(&create(&f.repo, "none"));
    assert!(!path.join(".env").exists());
}

#[test]
fn reused_worktree_keeps_its_own_included_files() {
    let f = fixture("include-reuse");
    ignore_env_files(&f.repo);
    fs::write(f.repo.join(".env"), "A=1\n").unwrap();
    fs::write(f.repo.join(".worktreeinclude"), ".env\n").unwrap();
    let first = created_path(&create(&f.repo, "again"));
    fs::write(first.join(".env"), "A=changed\n").unwrap();
    let second = created_path(&create(&f.repo, "again"));
    assert_eq!(
        fs::read_to_string(second.join(".env")).unwrap(),
        "A=changed\n"
    );
}

#[test]
fn worktreeinclude_cannot_escape_through_parent_patterns() {
    let f = fixture("include-escape");
    ignore_env_files(&f.repo);
    fs::write(f.root.join("proj/secret.env"), "S=1\n").unwrap();
    fs::write(
        f.repo.join(".worktreeinclude"),
        "../secret.env\n/../secret.env\n",
    )
    .unwrap();
    let path = created_path(&create(&f.repo, "esc"));
    assert!(!path.join("secret.env").exists());
    assert!(!f.root.join("proj/.worktrees/secret.env").exists());
}

fn push_new_commit_from_other_clone(f: &Fixture, file: &str) {
    let other = f.root.join("other");
    let origin = f.root.join("origin.git");
    git_ok(
        &f.root,
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    fs::write(other.join(file), "x\n").unwrap();
    git_ok(&other, &["add", "."]);
    git_ok(&other, &["commit", "-q", "-m", "upstream"]);
    git_ok(&other, &["push", "-q", "origin", "main"]);
}

#[test]
fn create_fetches_origin_once_a_day_and_bases_on_it() {
    let f = fixture("fetch");
    push_new_commit_from_other_clone(&f, "upstream1.txt");
    let first = created_path(&create(&f.repo, "one"));
    assert!(
        first.join("upstream1.txt").exists(),
        "stale origin should be refreshed"
    );
    // The stamp is fresh now, so a second create must not hit the network.
    push_new_commit_from_other_clone_again(&f);
    let second = created_path(&create(&f.repo, "two"));
    assert!(!second.join("upstream2.txt").exists());
    // An old stamp allows the next refresh.
    let common = git_ok(
        &f.repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    let common = PathBuf::from(common.trim());
    for name in ["playbook-origin-fetch", "FETCH_HEAD"] {
        let _ = fs::remove_file(common.join(name));
    }
    let third = created_path(&create(&f.repo, "three"));
    assert!(third.join("upstream2.txt").exists());
}

fn push_new_commit_from_other_clone_again(f: &Fixture) {
    let other = f.root.join("other");
    fs::write(other.join("upstream2.txt"), "y\n").unwrap();
    git_ok(&other, &["add", "."]);
    git_ok(&other, &["commit", "-q", "-m", "upstream2"]);
    git_ok(&other, &["push", "-q", "origin", "main"]);
}

#[test]
fn create_falls_back_to_the_cached_ref_when_origin_is_unreachable() {
    let f = fixture("offline");
    git_ok(&f.repo, &["remote", "set-head", "origin", "main"]);
    git_ok(
        &f.repo,
        &[
            "remote",
            "set-url",
            "origin",
            f.root.join("missing.git").to_str().unwrap(),
        ],
    );
    let out = create(&f.repo, "off");
    let path = created_path(&out);
    assert!(path.join("README.md").exists());
    assert!(stderr(&out).contains("using the cached ref"));
    assert_eq!(stdout(&out), format!("{}\n", path.display()));
}

#[test]
fn payload_base_commit_skips_the_origin_refresh() {
    let f = fixture("nofetch");
    push_new_commit_from_other_clone(&f, "upstream1.txt");
    let seed = git_ok(&f.repo, &["rev-parse", "HEAD"]).trim().to_string();
    let payload = format!(
        r#"{{"cwd":"{}","worktree_name":"pinned","base_commit":"{seed}"}}"#,
        f.repo.display()
    );
    let path = created_path(&run_hook("worktree-create", &payload));
    assert!(!path.join("upstream1.txt").exists());
    let common = git_ok(
        &f.repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    assert!(!PathBuf::from(common.trim())
        .join("playbook-origin-fetch")
        .exists());
}

#[test]
fn create_reuses_a_branch_checked_out_at_another_path() {
    let f = fixture("elsewhere");
    let old = f.root.join("old-place");
    git_ok(
        &f.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "worktree-moved",
            old.to_str().unwrap(),
        ],
    );
    let out = create(&f.repo, "moved");
    assert_eq!(created_path(&out), old);
}

#[test]
fn remove_treats_a_squash_merged_branch_as_published() {
    let f = fixture("rm-squash");
    let path = created_path(&create(&f.repo, "sq"));
    for n in ["a", "b"] {
        fs::write(path.join(format!("{n}.txt")), "work\n").unwrap();
        git_ok(&path, &["add", "."]);
        git_ok(&path, &["commit", "-q", "-m", n]);
    }
    git_ok(&f.repo, &["merge", "--squash", "worktree-sq"]);
    git_ok(&f.repo, &["commit", "-q", "-m", "squashed"]);
    git_ok(&f.repo, &["push", "-q", "origin", "main"]);
    let out = remove(&f.repo, &path);
    assert!(out.status.success());
    assert!(!path.exists(), "stderr: {}", stderr(&out));
    assert!(git_ok(&f.repo, &["branch", "--list", "worktree-sq"])
        .trim()
        .is_empty());
}

#[test]
fn remove_keeps_a_squash_merged_branch_that_gained_another_commit() {
    let f = fixture("rm-squash-extra");
    let path = created_path(&create(&f.repo, "sqx"));
    fs::write(path.join("a.txt"), "work\n").unwrap();
    git_ok(&path, &["add", "."]);
    git_ok(&path, &["commit", "-q", "-m", "a"]);
    git_ok(&f.repo, &["merge", "--squash", "worktree-sqx"]);
    git_ok(&f.repo, &["commit", "-q", "-m", "squashed"]);
    git_ok(&f.repo, &["push", "-q", "origin", "main"]);
    fs::write(path.join("late.txt"), "late\n").unwrap();
    git_ok(&path, &["add", "."]);
    git_ok(&path, &["commit", "-q", "-m", "late"]);
    let out = remove(&f.repo, &path);
    assert!(path.exists(), "stderr: {}", stderr(&out));
}

#[test]
fn remove_keeps_a_branch_whose_commits_cancel_out() {
    let f = fixture("rm-cancel");
    let path = created_path(&create(&f.repo, "cancel"));
    fs::write(path.join("a.txt"), "work\n").unwrap();
    git_ok(&path, &["add", "."]);
    git_ok(&path, &["commit", "-q", "-m", "add"]);
    git_ok(&path, &["rm", "-q", "a.txt"]);
    git_ok(&path, &["commit", "-q", "-m", "undo"]);
    let out = remove(&f.repo, &path);
    assert!(path.exists(), "stderr: {}", stderr(&out));
}

#[test]
fn create_prunes_a_branch_registered_at_a_deleted_other_path() {
    let f = fixture("gone-elsewhere");
    let old = f.root.join("old-place");
    git_ok(
        &f.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "worktree-lost",
            old.to_str().unwrap(),
        ],
    );
    fs::remove_dir_all(&old).unwrap();
    let out = create(&f.repo, "lost");
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(created_path(&out).is_dir());
}

#[test]
fn create_never_hands_back_the_main_checkout() {
    let f = fixture("main-branch");
    git_ok(&f.repo, &["checkout", "-q", "-b", "worktree-main-held"]);
    let out = create(&f.repo, "main-held");
    assert_ne!(stdout(&out).trim(), f.repo.to_str().unwrap());
}

#[test]
fn remove_kept_message_names_the_manual_command() {
    let f = fixture("rm-hint");
    let path = created_path(&create(&f.repo, "hint"));
    fs::write(path.join("new.txt"), "work\n").unwrap();
    git_ok(&path, &["add", "."]);
    git_ok(&path, &["commit", "-q", "-m", "local only"]);
    let out = remove(&f.repo, &path);
    assert!(path.exists());
    assert!(stderr(&out).contains(&format!("git worktree remove --force '{}'", path.display())));
}

#[test]
fn create_clears_an_empty_unregistered_leftover_directory() {
    let f = fixture("leftover");
    let first = created_path(&create(&f.repo, "crashed"));
    git_ok(&f.repo, &["worktree", "remove", first.to_str().unwrap()]);
    git_ok(&f.repo, &["branch", "-D", "worktree-crashed"]);
    fs::create_dir_all(&first).unwrap();
    let again = created_path(&create(&f.repo, "crashed"));
    assert_eq!(first, again);
    assert!(again.join("README.md").exists());
}

#[test]
fn create_keeps_a_non_empty_unregistered_directory_and_names_it() {
    let f = fixture("leftover-full");
    let first = created_path(&create(&f.repo, "full"));
    git_ok(&f.repo, &["worktree", "remove", first.to_str().unwrap()]);
    git_ok(&f.repo, &["branch", "-D", "worktree-full"]);
    fs::create_dir_all(&first).unwrap();
    fs::write(first.join("mine.txt"), "keep\n").unwrap();
    let out = create(&f.repo, "full");
    assert!(first.join("mine.txt").exists());
    assert!(out.status.success(), "fallback should still place it");
    assert!(stderr(&out).contains("not a worktree of this repo"));
}

/// Commits a `.gitignore` for `target` and fills the main checkout's
/// `target` with one build artifact.
fn warm_target(repo: &Path) {
    fs::write(repo.join(".gitignore"), "target\n").unwrap();
    git_ok(repo, &["add", "."]);
    git_ok(repo, &["commit", "-q", "-m", "ignore target"]);
    git_ok(repo, &["push", "-q"]);
    fs::create_dir_all(repo.join("target/debug")).unwrap();
    fs::write(repo.join("target/debug/dep.rlib"), "warm\n").unwrap();
}

/// Whether `dir` sits on a filesystem that supports `cp -c` clones.
fn supports_clone(dir: &Path) -> bool {
    let probe = dir.join("clone-probe");
    fs::write(&probe, "x").unwrap();
    let cloned = Command::new("cp")
        .arg("-c")
        .arg(&probe)
        .arg(dir.join("clone-probe-copy"))
        .status()
        .is_ok_and(|s| s.success());
    let _ = fs::remove_file(&probe);
    let _ = fs::remove_file(dir.join("clone-probe-copy"));
    cloned
}

macro_rules! require_clone_fs {
    ($f:expr) => {
        if !cfg!(target_os = "macos") || !supports_clone(&$f.root) {
            eprintln!("skipped: needs macOS on a filesystem with clonefile support (APFS)");
            return;
        }
    };
}

#[test]
fn create_seeds_the_new_worktree_with_a_copy_on_write_clone_of_target() {
    // Arrange: a main checkout with a warm, git-ignored target
    let f = fixture("clone");
    require_clone_fs!(f);
    warm_target(&f.repo);

    // Act
    let path = created_path(&create(&f.repo, "probe"));

    // Assert: the artifact arrived, and writing it leaves the main copy alone
    let cloned = path.join("target/debug/dep.rlib");
    assert_eq!(fs::read_to_string(&cloned).unwrap(), "warm\n");
    fs::write(&cloned, "changed\n").unwrap();
    assert_eq!(
        fs::read_to_string(f.repo.join("target/debug/dep.rlib")).unwrap(),
        "warm\n"
    );
}

#[test]
fn create_skips_the_clone_when_main_has_no_target() {
    // Arrange
    let f = fixture("clone-none");

    // Act
    let path = created_path(&create(&f.repo, "probe"));

    // Assert
    assert!(!path.join("target").exists());
}

#[test]
fn create_skips_the_clone_when_target_is_not_git_ignored() {
    // Arrange: a target that git would list as untracked
    let f = fixture("clone-unignored");
    fs::create_dir_all(f.repo.join("target")).unwrap();
    fs::write(f.repo.join("target/dep.rlib"), "warm\n").unwrap();

    // Act
    let path = created_path(&create(&f.repo, "probe"));

    // Assert: cloning it would make the fresh worktree look dirty
    assert!(!path.join("target").exists());
}

#[cfg(unix)]
#[test]
fn create_leaves_no_partial_target_when_the_clone_fails() {
    // Arrange: an unreadable artifact makes the copy fail midway
    let f = fixture("clone-fail");
    require_clone_fs!(f);
    warm_target(&f.repo);
    let locked = f.repo.join("target/debug/locked.rlib");
    fs::write(&locked, "x").unwrap();
    fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();

    // Act
    let out = create(&f.repo, "probe");

    // Assert: the hook still succeeds and the worktree starts without a target
    let path = created_path(&out);
    assert!(!path.join("target").exists());
}

#[cfg(unix)]
#[test]
fn create_skips_the_clone_when_main_target_is_a_symlink() {
    // Arrange: a target that links to a build folder elsewhere
    let f = fixture("clone-symlink");
    require_clone_fs!(f);
    warm_target(&f.repo);
    let elsewhere = f.root.join("shared-build");
    fs::rename(f.repo.join("target"), &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, f.repo.join("target")).unwrap();

    // Act
    let path = created_path(&create(&f.repo, "probe"));

    // Assert: a link would let the worktree write into the shared artifacts
    assert!(path.join("target").symlink_metadata().is_err());
}

#[test]
fn create_does_not_touch_an_existing_target_in_a_reused_worktree() {
    // Arrange: a worktree that already has its own target
    let f = fixture("clone-reuse");
    require_clone_fs!(f);
    warm_target(&f.repo);
    let first = created_path(&create(&f.repo, "probe"));
    fs::write(first.join("target/debug/dep.rlib"), "mine\n").unwrap();

    // Act
    let second = created_path(&create(&f.repo, "probe"));

    // Assert
    assert_eq!(
        fs::read_to_string(second.join("target/debug/dep.rlib")).unwrap(),
        "mine\n"
    );
}

#[test]
fn remove_deletes_the_cloned_target_with_the_worktree() {
    // Arrange
    let f = fixture("clone-remove");
    require_clone_fs!(f);
    warm_target(&f.repo);
    let path = created_path(&create(&f.repo, "probe"));

    // Act
    remove(&f.repo, &path);

    // Assert: the ignored target does not keep the worktree alive
    assert!(!path.exists());
    assert!(f.repo.join("target/debug/dep.rlib").exists());
}
