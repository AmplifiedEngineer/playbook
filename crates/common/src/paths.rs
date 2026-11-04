// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Canonical path resolution for the directories playbook itself owns:
//! memory, runtime state, config-drift baselines, per-repo scoped storage.

use crate::repo::repo_slug;
use crate::session::home_dir;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// `$HOME/.config/playbook`, the root for every directory playbook itself
/// owns. Reuses `session::home_dir()`'s documented symlink/env-var caveat.
pub fn playbook_root() -> PathBuf {
    playbook_root_from(&home_dir())
}

/// Core of `playbook_root`, taking `home` explicitly so tests, and `init`'s
/// own home-parameterised modules, can resolve it without the real `$HOME`.
pub fn playbook_root_from(home: &Path) -> PathBuf {
    home.join(".config").join("playbook")
}

/// `$HOME/.config/playbook/lsp`: language servers installed by
/// `playbook code lsp install`. Not under the data folder, which holds
/// databases only.
pub fn lsp_root() -> PathBuf {
    playbook_root().join("lsp")
}

/// `$HOME/.config/playbook/memory`: saved memory facts. The graph built from
/// them is `data/memory.db`.
pub fn memory_dir() -> PathBuf {
    playbook_root().join("memory")
}

/// `$HOME/.config/playbook/runtime`: per-session state (`session_dir`'s
/// parent).
pub fn runtime_root() -> PathBuf {
    playbook_root().join("runtime")
}

/// `$HOME/.config/playbook/cc-state`: per-project config-drift baselines.
pub fn cc_state_dir() -> PathBuf {
    playbook_root().join("cc-state")
}

/// A stable identity for the CURRENT git worktree: `cc::project_slug`'s
/// slugifier applied to `git rev-parse --show-toplevel`, not the raw cwd.
pub fn worktree_id() -> String {
    worktree_id_at(&std::env::current_dir().unwrap_or_default())
}

/// Core of `worktree_id`, taking the starting directory explicitly so tests
/// can point it at a real scratch worktree instead of mutating cwd.
fn worktree_id_at(dir: &Path) -> String {
    slugify(&git_toplevel(dir))
}

/// A spawn failure or timeout gets this many attempts total before giving
/// up; a real git failure (not a repo) is never retried, see `git_toplevel_with`.
const GIT_TOPLEVEL_ATTEMPTS: u32 = 3;
const GIT_TOPLEVEL_RETRY_DELAY: Duration = Duration::from_millis(50);

/// `git -C <dir> rev-parse --show-toplevel`, trimmed. Empty on any failure,
/// including a timeout. Never panics.
fn git_toplevel(dir: &Path) -> String {
    if let Some(top) = crate::gitfacts::toplevel(dir) {
        return top.to_string_lossy().into_owned();
    }
    git_toplevel_with(
        dir,
        GIT_TOPLEVEL_ATTEMPTS,
        GIT_TOPLEVEL_RETRY_DELAY,
        |dir| {
            crate::git::run(
                Some(dir),
                &["--no-optional-locks", "rev-parse", "--show-toplevel"],
                crate::git::SLOW_TIMEOUT,
            )
        },
    )
}

/// Core of `git_toplevel`, with the attempt itself injected so a test can
/// simulate a transient failure without spawning real processes. A `None`
/// attempt (spawn failure or timeout, both transient) is retried up to
/// `attempts` times; a `Some` with a non-success status is a real result
/// (e.g. `dir` genuinely isn't a git repo) and returns immediately, since
/// retrying it would never change the answer.
fn git_toplevel_with(
    dir: &Path,
    attempts: u32,
    delay: Duration,
    mut attempt: impl FnMut(&Path) -> Option<std::process::Output>,
) -> String {
    for i in 0..attempts {
        if i > 0 {
            std::thread::sleep(delay);
        }
        let Some(output) = attempt(dir) else {
            continue;
        };
        if !output.status.success() {
            return String::new();
        }
        return String::from_utf8_lossy(&output.stdout).trim().to_string();
    }
    String::new()
}

/// Every non-alphanumeric character becomes `-`. Duplicates
/// `cc::project_slug`'s rule since `common` sits below `cc` in this layering.
fn slugify(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The worktree id of the worktree rooted at `path`: the same slug
/// [`worktree_id`] gives from inside it, for a worktree seen from outside.
pub fn worktree_id_for(path: &Path) -> String {
    slugify(&path.to_string_lossy())
}

/// Which tier of a repo's scoped storage a path resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoScope {
    /// `repos/<owner>/<repo>/.config/`: shared across every worktree.
    /// `playbook config set --repo` writes `config.json` here.
    Config,
    /// `repos/<owner>/<repo>/<worktree-id>/`: scoped to one worktree, so
    /// two worktrees of the same repo never collide.
    Worktree,
    /// `repos/<owner>/<repo>/`: one folder per repo, the same from every
    /// worktree and every checkout. Plans, implementation notes, worktrees
    /// and handoffs live under it, so a person or an agent with no binary can
    /// derive the path from the `origin` remote alone.
    Repo,
}

/// Resolve `kind`'s directory for the CURRENT repo, under
/// `playbook_root()/repos/<owner>/<repo>/...`. `None` on any git failure.
pub fn repo_scoped_dir(kind: RepoScope) -> Option<PathBuf> {
    let slug = repo_slug();
    let (owner, repo) = slug.split_once('/')?;
    let id = if kind == RepoScope::Worktree {
        worktree_id()
    } else {
        String::new()
    };
    if kind == RepoScope::Worktree && id.is_empty() {
        return None;
    }
    Some(repo_scope_path(&playbook_root(), owner, repo, kind, &id))
}

/// `repos/<owner>/<repo>/` for an `<owner>/<repo>` slug, or `None` when the
/// slug is empty or has no owner.
pub fn repo_dir_for_slug(slug: &str) -> Option<PathBuf> {
    let (owner, repo) = slug.split_once('/')?;
    Some(repo_scope_path(
        &playbook_root(),
        owner,
        repo,
        RepoScope::Repo,
        "",
    ))
}

/// Pure form of `repo_scoped_dir` for a known root, owner, repo and worktree id.
pub fn repo_scope_path(
    root: &Path,
    owner: &str,
    repo: &str,
    kind: RepoScope,
    worktree_id: &str,
) -> PathBuf {
    let base = root.join("repos").join(owner).join(repo);
    match kind {
        RepoScope::Config => base.join(".config"),
        RepoScope::Worktree => base.join(worktree_id),
        RepoScope::Repo => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::scratch_dir;
    use std::fs;
    use std::process::Command;
    use std::process::Output;

    #[test]
    fn playbook_root_is_config_playbook_under_home() {
        // Arrange
        let home = Path::new("/scratch-home-for-test");

        // Act
        let got = playbook_root_from(home);

        // Assert
        assert_eq!(got, home.join(".config").join("playbook"));
    }

    #[test]
    fn memory_dir_runtime_root_and_cc_state_dir_sit_flat_under_playbook_root() {
        // Arrange, Act
        let root = playbook_root();

        // Assert: each is a direct child of playbook_root().
        assert_eq!(memory_dir(), root.join("memory"));
        assert_eq!(runtime_root(), root.join("runtime"));
        assert_eq!(cc_state_dir(), root.join("cc-state"));
    }

    /// Isolated from the host's own git config, matching
    /// `tests/cc_worktree.rs`'s `git`/`git_ok` helpers.
    fn git(repo_path: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .arg("-C")
            .arg(repo_path)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("git command should spawn")
    }

    fn git_ok(repo_path: &Path, args: &[&str]) {
        let out = git(repo_path, args);
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A real repo with one commit on `main`, canonicalized: macOS resolves
    /// `/tmp` through `/private/tmp`, and `git` reports the resolved path.
    fn seeded_repo(tag: &str) -> PathBuf {
        let dir = scratch_dir(tag);
        fs::create_dir_all(&dir).expect("create scratch repo dir");
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "T"],
        ] {
            git_ok(&dir, &args);
        }
        fs::write(dir.join("README.md"), "seed\n").expect("write seed file");
        git_ok(&dir, &["add", "."]);
        git_ok(&dir, &["commit", "-q", "-m", "seed"]);
        dir.canonicalize().expect("seeded repo should resolve")
    }

    /// Adds a real linked worktree on a new branch, returning its
    /// canonical path.
    fn add_worktree(repo_root: &Path, branch: &str) -> PathBuf {
        let dest = repo_root.join(format!("wt-{branch}"));
        git_ok(
            repo_root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                branch,
                dest.to_str().expect("utf8 path"),
            ],
        );
        dest.canonicalize().expect("worktree should resolve")
    }

    #[test]
    fn a_transient_none_attempt_is_retried_until_it_succeeds() {
        // Arrange: the repo used for the real, successful attempt.
        let repo = seeded_repo("git-toplevel-retry-success");
        let calls = std::cell::Cell::new(0u32);

        // Act: first two attempts simulate a spawn failure or timeout
        // (`None`); the third runs a real `git` invocation.
        let got = git_toplevel_with(&repo, 3, Duration::from_millis(1), |dir| {
            let n = calls.get();
            calls.set(n + 1);
            if n < 2 {
                None
            } else {
                Some(git(dir, &["rev-parse", "--show-toplevel"]))
            }
        });

        // Assert
        assert_eq!(got, repo.to_str().unwrap());
        assert_eq!(
            calls.get(),
            3,
            "should have retried twice before succeeding"
        );

        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn exhausting_every_retry_on_persistent_none_returns_empty() {
        // Arrange
        let calls = std::cell::Cell::new(0u32);

        // Act: every attempt is a simulated transient failure.
        let got = git_toplevel_with(Path::new("/unused"), 3, Duration::from_millis(1), |_| {
            calls.set(calls.get() + 1);
            None
        });

        // Assert: gave up empty, and spent the whole budget, not less or more.
        assert_eq!(got, "");
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn a_real_git_failure_is_returned_immediately_not_retried() {
        // Arrange: a directory that is genuinely not a git repo, so `git`
        // exits non-zero on the very first attempt.
        let not_a_repo = scratch_dir("git-toplevel-not-a-repo");
        fs::create_dir_all(&not_a_repo).expect("scratch dir");
        let calls = std::cell::Cell::new(0u32);

        // Act
        let got = git_toplevel_with(&not_a_repo, 3, Duration::from_millis(1), |dir| {
            calls.set(calls.get() + 1);
            Some(git(dir, &["rev-parse", "--show-toplevel"]))
        });

        // Assert: a real failure is not a transient one, so it must not
        // burn the retry budget.
        assert_eq!(got, "");
        assert_eq!(calls.get(), 1, "a real git failure must not be retried");

        let _ = fs::remove_dir_all(&not_a_repo);
    }

    #[test]
    fn worktree_id_stable_regardless_of_cwd_depth_within_one_worktree() {
        // Arrange
        let repo = seeded_repo("wtid-stable");
        let nested = repo.join("a").join("b");
        fs::create_dir_all(&nested).expect("nested dir");

        // Act
        let at_top = worktree_id_at(&repo);
        let at_depth = worktree_id_at(&nested);

        // Assert
        assert!(!at_top.is_empty());
        assert_eq!(at_top, at_depth);

        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn worktree_id_differs_between_two_worktrees_of_the_same_repo() {
        // Arrange: a real `git worktree add`, not a mocked toplevel.
        let repo = seeded_repo("wtid-differ");
        let worktree = add_worktree(&repo, "feature");

        // Act
        let id_repo = worktree_id_at(&repo);
        let id_worktree = worktree_id_at(&worktree);

        // Assert
        assert!(!id_repo.is_empty());
        assert!(!id_worktree.is_empty());
        assert_ne!(id_repo, id_worktree);

        let _ = fs::remove_dir_all(&repo);
    }

    // Tests that mutate the process cwd take this lock: cwd is process-global.
    use crate::test_support::lock_cwd;

    #[test]
    fn repo_scoped_dir_none_outside_a_git_repo() {
        // Arrange: a scratch dir with no `.git` anywhere in its ancestry.
        let _guard = lock_cwd();
        let outside = scratch_dir("repo-scoped-outside");
        fs::create_dir_all(&outside).expect("create scratch dir");
        let previous = std::env::current_dir().expect("read current dir");
        std::env::set_current_dir(&outside).expect("cd into scratch dir");

        // Act
        let got = repo_scoped_dir(RepoScope::Worktree);

        // Assert
        std::env::set_current_dir(&previous).expect("restore cwd");
        let _ = fs::remove_dir_all(&outside);
        assert_eq!(got, None);
    }

    #[test]
    fn repo_scoped_dir_none_with_no_origin_remote() {
        // Arrange: a real git repo with no `origin` remote configured.
        let _guard = lock_cwd();
        let repo = scratch_dir("repo-scoped-no-origin");
        fs::create_dir_all(&repo).expect("create scratch repo dir");
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "T"],
        ] {
            git_ok(&repo, &args);
        }
        let previous = std::env::current_dir().expect("read current dir");
        std::env::set_current_dir(&repo).expect("cd into scratch repo");

        // Act
        let got = repo_scoped_dir(RepoScope::Config);

        // Assert
        std::env::set_current_dir(&previous).expect("restore cwd");
        let _ = fs::remove_dir_all(&repo);
        assert_eq!(got, None);
    }
}
