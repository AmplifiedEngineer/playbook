// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The local side of an action that writes code: a worktree of the pull
//! request, a model with tools running in it, and a signed commit pushed to a
//! new branch. The clone is the one `watch` runs in. Nothing here force-pushes
//! or touches the default branch.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::claude::Call;
use crate::common::proc::run_with_timeout;
use crate::config;

const GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// How a tool-using call ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolEnd {
    /// The call exited successfully and printed this.
    Output(String),
    /// The kill switch tripped and the call's process group was stopped.
    Killed,
}

/// The effects of a code writing action. A fake stands in for git and
/// `claude` in tests.
pub trait CodeWork {
    /// Whether this platform can run the action at all.
    fn supported(&self) -> bool;
    /// Whether the kill switch (`watch.enabled = false`) is on.
    fn killed(&self) -> bool;
    /// Where the worktree for `branch` lives.
    fn worktree_path(&self, branch: &str) -> String;
    /// Makes `dir` a worktree of pull request `number`, on a new `branch`.
    fn checkout(&self, repo: &str, number: u64, branch: &str, dir: &str) -> Result<(), String>;
    /// Runs the call, stopping it when the kill switch trips.
    fn run_tool(&self, call: &Call) -> Result<ToolEnd, String>;
    /// Stages every change in `dir` and returns the files and lines touched.
    fn staged_size(&self, dir: &str) -> Result<(i64, i64), String>;
    /// Commits the staged change, signed and signed off, and pushes `branch`.
    fn publish(&self, dir: &str, branch: &str, message: &str) -> Result<(), String>;
    /// Removes the worktree and what it holds.
    fn discard(&self, dir: &str) -> Result<(), String>;
}

/// Git and `claude` on this machine.
pub struct GitCode<'a> {
    /// The clone that `watch` runs in.
    pub clone: &'a Path,
    /// Worktrees are made under this directory.
    pub worktrees: PathBuf,
    pub home: &'a Path,
}

impl GitCode<'_> {
    fn git(&self, dir: &Path, args: &[&str]) -> Result<String, String> {
        let mut command = Command::new("git");
        command.arg("-C").arg(dir).args(args);
        let output = run_with_timeout(&mut command, GIT_TIMEOUT)
            .ok_or_else(|| "git did not finish (missing, or timed out)".to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

impl CodeWork for GitCode<'_> {
    fn supported(&self) -> bool {
        cfg!(unix)
    }

    fn killed(&self) -> bool {
        // An unreadable setting stops the work: the safe side.
        config::resolve_valid("watch.enabled", self.home, None)
            .map_or(true, |(value, _, _)| value.as_bool() != Some(true))
    }

    fn worktree_path(&self, branch: &str) -> String {
        self.worktrees
            .join(branch.replace('/', "-"))
            .display()
            .to_string()
    }

    fn checkout(&self, repo: &str, number: u64, branch: &str, dir: &str) -> Result<(), String> {
        // The configured URL, not the one after `url.*.insteadOf` rewrites.
        let origin = self.git(self.clone, &["config", "--get", "remote.origin.url"])?;
        if !remote_is(&origin, repo) {
            return Err(format!(
                "the clone watch runs in does not have {repo} as its origin"
            ));
        }
        self.git(
            self.clone,
            &["fetch", "origin", &format!("pull/{number}/head")],
        )?;
        // `-B` so a branch left by an earlier try is reset, not an error.
        self.git(
            self.clone,
            &["worktree", "add", "-B", branch, dir, "FETCH_HEAD"],
        )
        .map(drop)
    }

    #[cfg(unix)]
    fn run_tool(&self, call: &Call) -> Result<ToolEnd, String> {
        use super::child::{supervise, Ended, OsSpawner};
        let mut child = OsSpawner::new("claude")
            .spawn(call)
            .map_err(|e| format!("could not start claude: {e}"))?;
        let ended = supervise(&mut child, &|| self.killed(), &mut |seconds| {
            std::thread::sleep(Duration::from_secs(seconds.unsigned_abs()))
        })
        .map_err(|e| format!("lost track of claude: {e}"))?;
        match ended {
            Ended::Killed => Ok(ToolEnd::Killed),
            Ended::Exited { success: true } => Ok(ToolEnd::Output(child.output())),
            Ended::Exited { success: false } => Err("claude exited with an error".to_string()),
        }
    }

    #[cfg(not(unix))]
    fn run_tool(&self, _: &Call) -> Result<ToolEnd, String> {
        Err("running claude in a process group needs a Unix system".to_string())
    }

    fn staged_size(&self, dir: &str) -> Result<(i64, i64), String> {
        let dir = Path::new(dir);
        self.git(dir, &["add", "-A"])?;
        let numstat = self.git(dir, &["diff", "--cached", "--numstat"])?;
        Ok(parse_numstat(&numstat))
    }

    fn publish(&self, dir: &str, branch: &str, message: &str) -> Result<(), String> {
        let dir = Path::new(dir);
        self.git(dir, &["commit", "--gpg-sign", "--signoff", "-m", message])?;
        let refspec = format!("{branch}:refs/heads/{branch}");
        self.git(dir, &["push", "origin", &refspec]).map(drop)
    }

    fn discard(&self, dir: &str) -> Result<(), String> {
        if !Path::new(dir).exists() {
            return Ok(());
        }
        self.git(self.clone, &["worktree", "remove", "--force", dir])
            .map(drop)
    }
}

/// Whether `url` is a remote of `repo` (`owner/name`), over https or ssh.
fn remote_is(url: &str, repo: &str) -> bool {
    const PREFIXES: [&str; 3] = [
        "https://github.com/",
        "git@github.com:",
        "ssh://git@github.com/",
    ];
    let url = url.trim_end_matches('/').trim_end_matches(".git");
    PREFIXES
        .iter()
        .filter_map(|prefix| url.strip_prefix(prefix))
        .any(|path| path.eq_ignore_ascii_case(repo))
}

/// The files and the added plus deleted lines in `git diff --numstat`. A
/// binary file counts as a file with no lines.
fn parse_numstat(text: &str) -> (i64, i64) {
    let mut files = 0;
    let mut lines = 0;
    for row in text.lines() {
        let mut cols = row.split('\t');
        let (Some(added), Some(deleted)) = (cols.next(), cols.next()) else {
            continue;
        };
        files += 1;
        lines += added.parse::<i64>().unwrap_or(0) + deleted.parse::<i64>().unwrap_or(0);
    }
    (files, lines)
}

/// A [`CodeWork`] that records what it was asked and answers from fields.
#[cfg(test)]
pub struct FakeCode {
    pub unix: bool,
    /// What `run_tool` ends with.
    pub end: std::cell::RefCell<ToolEnd>,
    /// The files and lines `staged_size` reports.
    pub size: std::cell::Cell<(i64, i64)>,
    /// The step names in the order they happened.
    pub steps: std::cell::RefCell<Vec<&'static str>>,
    pub calls: std::cell::RefCell<Vec<Call>>,
}

#[cfg(test)]
impl Default for FakeCode {
    fn default() -> Self {
        Self {
            unix: true,
            end: std::cell::RefCell::new(ToolEnd::Output(r#"{"total_cost_usd":0.1}"#.into())),
            size: std::cell::Cell::new((1, 1)),
            steps: std::cell::RefCell::default(),
            calls: std::cell::RefCell::default(),
        }
    }
}

#[cfg(test)]
impl CodeWork for FakeCode {
    fn supported(&self) -> bool {
        self.unix
    }
    fn killed(&self) -> bool {
        false
    }
    fn worktree_path(&self, branch: &str) -> String {
        format!("/work/{}", branch.replace('/', "-"))
    }
    fn checkout(&self, _: &str, _: u64, _: &str, _: &str) -> Result<(), String> {
        self.steps.borrow_mut().push("checkout");
        Ok(())
    }
    fn run_tool(&self, call: &Call) -> Result<ToolEnd, String> {
        self.steps.borrow_mut().push("run");
        self.calls.borrow_mut().push(call.clone());
        Ok(self.end.borrow().clone())
    }
    fn staged_size(&self, _: &str) -> Result<(i64, i64), String> {
        Ok(self.size.get())
    }
    fn publish(&self, _: &str, _: &str, _: &str) -> Result<(), String> {
        self.steps.borrow_mut().push("publish");
        Ok(())
    }
    fn discard(&self, _: &str) -> Result<(), String> {
        self.steps.borrow_mut().push("discard");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numstat_counts_files_and_added_plus_deleted_lines() {
        // Arrange: a text file, a binary file and a blank line.
        let text = "3\t1\tsrc/a.rs\n-\t-\timg.png\n\n10\t0\tb.md";

        // Act / Assert
        assert_eq!(parse_numstat(text), (3, 14));
        assert_eq!(parse_numstat(""), (0, 0));
    }

    #[test]
    fn a_remote_matches_its_repo_over_https_and_ssh_and_nothing_else() {
        let rows = [
            ("https://github.com/o/r.git", true),
            ("https://github.com/o/r", true),
            ("git@github.com:o/r.git", true),
            ("https://github.com/o/other.git", false),
            ("https://example.com/o/r.git", false),
            ("https://github.com/evil/o/r.git", false),
            ("https://notgithub.com/o/r.git", false),
        ];
        for (url, want) in rows {
            assert_eq!(remote_is(url, "o/r"), want, "{url}");
        }
    }

    /// Real git against local repositories, no network: a bare repository
    /// stands in for GitHub, and a URL rewrite sends the clone's configured
    /// `https://github.com/o/r.git` to it.
    #[cfg(unix)]
    mod real_git {
        use super::*;
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        fn git(dir: &Path, args: &[&str]) -> String {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }

        /// A gpg stand-in that signs anything, so `--gpg-sign` works with no key.
        fn fake_gpg(root: &Path) -> PathBuf {
            let path = root.join("fake-gpg");
            fs::write(
                &path,
                "#!/bin/sh\ncat >/dev/null\necho '[GNUPG:] SIG_CREATED D 1 8 00 0 0 0' >&2\n\
                 printf -- '-----BEGIN PGP SIGNATURE-----\\n\\nZmFrZQ==\\n-----END PGP SIGNATURE-----\\n'\n",
            )
            .unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        struct Site {
            root: PathBuf,
            bare: PathBuf,
            clone: PathBuf,
        }

        /// A bare "GitHub" with `main` and pull request 7, and a clone of it.
        fn site(tag: &str, origin_url: &str) -> Site {
            let root = crate::testing::scratch_root(tag);
            let bare = root.join("origin.git");
            let seed = root.join("seed");
            let clone = root.join("clone");
            fs::create_dir_all(&seed).unwrap();
            git(&root, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
            git(&seed, &["init", "-q", "-b", "main"]);
            for (key, value) in [("user.name", "Seed"), ("user.email", "seed@example.com")] {
                git(&seed, &["config", key, value]);
            }
            fs::write(seed.join("a.txt"), "one\ntwo\n").unwrap();
            git(&seed, &["add", "-A"]);
            git(&seed, &["commit", "-q", "-m", "seed"]);
            let bare_url = bare.display().to_string();
            git(&seed, &["push", "-q", &bare_url, "HEAD:refs/heads/main"]);
            git(&seed, &["push", "-q", &bare_url, "HEAD:refs/pull/7/head"]);
            git(&root, &["clone", "-q", &bare_url, "clone"]);
            let gpg = fake_gpg(&root).display().to_string();
            for (key, value) in [
                ("user.name", "Watch"),
                ("user.email", "watch@example.com"),
                ("gpg.program", gpg.as_str()),
                ("remote.origin.url", origin_url),
            ] {
                git(&clone, &["config", key, value]);
            }
            git(
                &clone,
                &["config", &format!("url.{bare_url}.insteadOf"), origin_url],
            );
            Site { root, bare, clone }
        }

        fn code<'a>(site: &'a Site, home: &'a Path) -> GitCode<'a> {
            GitCode {
                clone: &site.clone,
                worktrees: site.root.join("trees"),
                home,
            }
        }

        const BRANCH: &str = "playbook/watch/r-7";

        #[test]
        fn a_change_goes_from_checkout_to_a_signed_pushed_branch_and_the_checkout_is_removed() {
            // Arrange
            let site = site("watch-git-flow", "https://github.com/o/r.git");
            let code = code(&site, &site.root);
            let dir = code.worktree_path(BRANCH);

            // Act
            code.checkout("o/r", 7, BRANCH, &dir).unwrap();
            fs::write(Path::new(&dir).join("a.txt"), "one\nTWO\nthree\n").unwrap();
            fs::write(Path::new(&dir).join("b.txt"), "new\n").unwrap();
            let size = code.staged_size(&dir).unwrap();
            code.publish(&dir, BRANCH, "fix: address a pull request comment")
                .unwrap();
            code.discard(&dir).unwrap();

            // Assert: two files; a.txt has 2 added and 1 deleted lines, b.txt 1 added.
            assert_eq!(size, (2, 4));
            let pushed = git(
                &site.bare,
                &["cat-file", "-p", &format!("refs/heads/{BRANCH}")],
            );
            assert!(pushed.contains("gpgsig"), "{pushed}");
            assert!(pushed.contains("Signed-off-by: Watch <watch@example.com>"));
            let parent = git(&site.bare, &["rev-parse", &format!("refs/heads/{BRANCH}^")]);
            assert_eq!(parent, git(&site.bare, &["rev-parse", "refs/pull/7/head"]));
            assert!(!Path::new(&dir).exists());
            code.discard(&dir).unwrap();
        }

        #[test]
        fn the_pull_request_branch_and_main_are_left_alone() {
            let site = site("watch-git-untouched", "https://github.com/o/r.git");
            let code = code(&site, &site.root);
            let dir = code.worktree_path(BRANCH);
            let main_before = git(&site.bare, &["rev-parse", "refs/heads/main"]);

            code.checkout("o/r", 7, BRANCH, &dir).unwrap();
            fs::write(Path::new(&dir).join("b.txt"), "new\n").unwrap();
            code.staged_size(&dir).unwrap();
            code.publish(&dir, BRANCH, "fix: x").unwrap();

            assert_eq!(
                git(&site.bare, &["rev-parse", "refs/heads/main"]),
                main_before
            );
            let refs = git(&site.bare, &["for-each-ref", "--format=%(refname)"]);
            assert_eq!(
                refs.lines().collect::<Vec<_>>(),
                [
                    "refs/heads/main",
                    "refs/heads/playbook/watch/r-7",
                    "refs/pull/7/head"
                ]
            );
        }

        #[test]
        fn a_clone_whose_origin_is_another_repository_is_refused() {
            let site = site("watch-git-origin", "https://github.com/o/other.git");
            let code = code(&site, &site.root);
            let dir = code.worktree_path(BRANCH);

            let err = code.checkout("o/r", 7, BRANCH, &dir).unwrap_err();

            assert!(err.contains("does not have o/r as its origin"), "{err}");
            assert!(!Path::new(&dir).exists());
        }

        #[test]
        fn a_checkout_left_by_an_earlier_try_is_reset_not_an_error() {
            let site = site("watch-git-retry", "https://github.com/o/r.git");
            let code = code(&site, &site.root);
            let dir = code.worktree_path(BRANCH);
            code.checkout("o/r", 7, BRANCH, &dir).unwrap();
            code.discard(&dir).unwrap();

            // The branch from the first try still exists locally.
            code.checkout("o/r", 7, BRANCH, &dir).unwrap();

            assert!(Path::new(&dir).join("a.txt").exists());
        }
    }
}
