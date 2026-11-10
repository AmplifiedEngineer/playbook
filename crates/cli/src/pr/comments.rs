// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr comments`: Steps 1 and 2 of `/playbook:pr-comments`.
//! It parses the flags, resolves the PR, and fetches the review threads and
//! the PR-level comments to two files, leaving the filtering to the model.

use super::shared::gh;
use super::stack::Gh;
use crate::common::par;
use std::fs;
use std::path::Path;

/// The flags and the PR the arguments name.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Parsed {
    pub include_bots: bool,
    pub dry_run: bool,
    pub auto_commit: bool,
    pub pr: Option<String>,
    pub warnings: Vec<String>,
}

/// Split the argument string. `--bots`, `--dry-run`, `-y`/`--yes`, a PR number
/// with or without `#`; `--auto` and `--ask` are accepted and ignored; any
/// other word is reported and skipped. The last PR number wins.
pub fn parse(args: &str) -> Parsed {
    let mut p = Parsed::default();
    for tok in args.split_whitespace() {
        match tok {
            "--bots" => p.include_bots = true,
            "--dry-run" => p.dry_run = true,
            "-y" | "--yes" => p.auto_commit = true,
            "--auto" | "--ask" => {}
            _ => {
                let digits = tok.strip_prefix('#').unwrap_or(tok);
                if digits.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    p.pr = Some(tok.strip_prefix('#').unwrap_or(tok).to_string());
                } else {
                    p.warnings
                        .push(format!("warning: ignoring unknown arg '{tok}'"));
                }
            }
        }
    }
    p
}

const THREADS_QUERY: &str = r#"
  query($owner: String!, $name: String!, $pr: Int!) {
    repository(owner: $owner, name: $name) {
      pullRequest(number: $pr) {
        reviewThreads(first: 100) {
          nodes {
            id
            isResolved
            isOutdated
            path
            line
            originalLine
            comments(first: 50) {
              nodes {
                id
                databaseId
                author { login }
                body
                url
                createdAt
              }
            }
          }
        }
      }
    }
  }"#;

/// Where the two comment files go.
const OUT_DIR: &str = "/tmp";

/// The error for a wave that returns fewer results than it had calls.
const FEWER_RESULTS: &str = "gh returned fewer results than calls";

/// Runs the independent `gh` calls of one wave side by side. Results come back
/// in call order, a panic in one call is that call's `Err`, and a call that
/// fails never stops the others.
fn wave<const N: usize>(
    run: Gh,
    cap: usize,
    calls: &[Vec<&str>; N],
) -> Result<[Result<String, String>; N], String> {
    let results = par::try_map(calls, cap, |args| run(args));
    <[Result<String, String>; N]>::try_from(results).map_err(|_| FEWER_RESULTS.to_string())
}

/// Resolve the PR, fetch both comment sources, and report. `Err` is the text
/// for stderr (exit 1).
pub fn run(args: &str) -> Result<String, String> {
    run_with(&|a| gh(a), Path::new(OUT_DIR), par::MAX_CONCURRENT, args)
}

/// [`run`] with the `gh` runner, the output folder and the worker cap given.
/// It makes up to six calls. The repo name, the head commit and the login do
/// not depend on each other, and the threads and the issue comments need only
/// the repo name, so they make two waves. It reports the first error in call
/// order, as a run of one call after another does.
fn run_with(run: Gh, out_dir: &Path, cap: usize, args: &str) -> Result<String, String> {
    let parsed = parse(args);
    for w in &parsed.warnings {
        eprintln!("{w}");
    }
    let pr = match &parsed.pr {
        Some(n) => n.clone(),
        None => run(&["pr", "view", "--json", "number", "-q", ".number"])
            .ok()
            .filter(|n| !n.is_empty())
            .ok_or("error: no PR for current branch; pass a PR number")?,
    };
    let [repo, head, me] = wave(
        run,
        cap,
        &[
            vec![
                "repo",
                "view",
                "--json",
                "nameWithOwner",
                "-q",
                ".nameWithOwner",
            ],
            vec![
                "pr",
                "view",
                &pr,
                "--json",
                "headRefOid",
                "-q",
                ".headRefOid",
            ],
            vec!["api", "/user", "-q", ".login"],
        ],
    )?;
    let repo = repo?;
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| format!("unexpected repo name: {repo}"))?;
    let (head, me) = (head?, me?);

    let query = format!("query={THREADS_QUERY}");
    let (owner_arg, name_arg, pr_arg) = (
        format!("owner={owner}"),
        format!("name={name}"),
        format!("pr={pr}"),
    );
    let issues_endpoint = format!("/repos/{owner}/{name}/issues/{pr}/comments");
    let [threads, issues] = wave(
        run,
        cap,
        &[
            vec![
                "api", "graphql", "-f", &query, "-F", &owner_arg, "-F", &name_arg, "-F", &pr_arg,
            ],
            vec!["api", &issues_endpoint, "--paginate"],
        ],
    )?;
    let (threads, issues) = (threads?, issues?);
    let threads_path = out_dir.join(format!("pr-comments-{pr}-threads.json"));
    let issues_path = out_dir.join(format!("pr-comments-{pr}-issues.json"));
    fs::write(&threads_path, threads)
        .map_err(|e| format!("could not write {}: {e}", threads_path.display()))?;
    fs::write(&issues_path, issues)
        .map_err(|e| format!("could not write {}: {e}", issues_path.display()))?;

    Ok(format!(
        "PR: {repo}#{pr}\nHead SHA: {head}\nMe: {me}\nFlags: bots={} dry-run={} auto-commit={}\nThreads: {}\nIssue comments: {}",
        parsed.include_bots,
        parsed.dry_run,
        parsed.auto_commit,
        threads_path.display(),
        issues_path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_and_a_pr_number_parse() {
        let p = parse("#12 --bots --dry-run -y");
        assert_eq!(p.pr.as_deref(), Some("12"));
        assert!(p.include_bots && p.dry_run && p.auto_commit);
    }

    #[test]
    fn a_bare_number_and_the_run_mode_flags_parse() {
        let p = parse("--auto 7 --ask");
        assert_eq!(p.pr.as_deref(), Some("7"));
        assert!(!p.include_bots && !p.dry_run && !p.auto_commit);
    }

    #[test]
    fn an_unknown_word_is_reported_and_skipped() {
        let p = parse("--wat 7");
        assert_eq!(p.warnings, vec!["warning: ignoring unknown arg '--wat'"]);
        assert_eq!(p.pr.as_deref(), Some("7"));
    }

    #[test]
    fn no_pr_means_the_current_branch() {
        assert_eq!(parse("--bots").pr, None);
        assert_eq!(parse("").pr, None);
    }

    use std::collections::HashMap;
    use std::sync::{Condvar, Mutex};
    use std::time::Duration;

    /// Longest a fake call waits for another call before it fails the run.
    const OVERLAP_LIMIT: Duration = Duration::from_secs(10);

    /// A one-shot flag a fake call can set and another can wait on.
    #[derive(Default)]
    struct Latch {
        set: Mutex<bool>,
        changed: Condvar,
    }

    impl Latch {
        fn raise(&self) {
            *self.set.lock().unwrap() = true;
            self.changed.notify_all();
        }

        fn wait(&self) -> bool {
            let set = self.set.lock().unwrap();
            let (set, _) = self
                .changed
                .wait_timeout_while(set, OVERLAP_LIMIT, |s| !*s)
                .unwrap();
            *set
        }
    }

    /// A scripted `gh` that records every call. The `answers` keys are the
    /// first words of the arguments, joined by a space.
    struct Fake {
        answers: HashMap<&'static str, Result<String, String>>,
        calls: Mutex<Vec<String>>,
        /// The repo call finishes only after the user call has: the finish
        /// order differs from the call order, and a run that makes the calls
        /// one at a time never gets the repo answer.
        user_done: Latch,
        /// The call with this key panics.
        panic_on: Option<&'static str>,
    }

    impl Fake {
        fn healthy() -> Fake {
            let ok = |s: &str| Ok(s.to_string());
            Fake {
                answers: HashMap::from([
                    ("repo view", ok("acme/widgets")),
                    ("pr view", ok("abc123")),
                    ("api /user", ok("octo")),
                    ("api graphql", ok("{\"threads\":1}")),
                    ("api /repos/acme/widgets/issues/7/comments", ok("[1,2]")),
                ]),
                calls: Mutex::new(Vec::new()),
                user_done: Latch::default(),
                panic_on: None,
            }
        }

        fn with(mut self, key: &'static str, answer: Result<String, String>) -> Fake {
            self.answers.insert(key, answer);
            self
        }

        fn call(&self, args: &[&str]) -> Result<String, String> {
            let key = if args[0] == "api" {
                format!("api {}", args[1])
            } else {
                format!("{} {}", args[0], args[1])
            };
            self.calls.lock().unwrap().push(key.clone());
            assert!(self.panic_on != Some(key.as_str()), "{key} exploded");
            if key == "api /user" {
                self.user_done.raise();
            }
            if key == "repo view" && !self.user_done.wait() {
                return Err("the repo call never overlapped the user call".to_string());
            }
            self.answers[key.as_str()].clone()
        }

        fn called(&self, key: &str) -> bool {
            self.calls.lock().unwrap().iter().any(|c| c == key)
        }
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = crate::common::test_support::scratch_dir(name);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn report(fake: &Fake, dir: &Path, cap: usize) -> Result<String, String> {
        run_with(&|a| fake.call(a), dir, cap, "#7")
    }

    #[test]
    fn the_report_and_files_are_the_same_one_call_at_a_time_and_in_parallel() {
        // Arrange
        let (one, four) = (scratch("comments-one"), scratch("comments-four"));
        let sequential = Fake::healthy();
        // One worker runs the calls in order, so the repo call would wait
        // forever for the user call behind it. Release it up front.
        sequential.user_done.raise();

        // Act
        let first = report(&sequential, &one, 1);
        let second = report(&Fake::healthy(), &four, par::MAX_CONCURRENT);

        // Assert
        let strip = |text: &str, dir: &Path| text.replace(&dir.display().to_string(), "DIR");
        assert_eq!(strip(&first.unwrap(), &one), strip(&second.unwrap(), &four));
        for file in ["pr-comments-7-threads.json", "pr-comments-7-issues.json"] {
            assert_eq!(
                fs::read_to_string(one.join(file)).unwrap(),
                fs::read_to_string(four.join(file)).unwrap()
            );
        }
        assert_eq!(
            fs::read_to_string(four.join("pr-comments-7-issues.json")).unwrap(),
            "[1,2]"
        );
    }

    #[test]
    fn the_report_names_the_repo_head_and_user_whatever_finishes_first() {
        let dir = scratch("comments-order");

        let text = report(&Fake::healthy(), &dir, par::MAX_CONCURRENT).unwrap();

        assert!(
            text.starts_with("PR: acme/widgets#7\nHead SHA: abc123\nMe: octo\nFlags: "),
            "{text}"
        );
    }

    #[test]
    fn a_failing_call_does_not_hide_the_others_and_the_old_order_picks_the_error() {
        // Arrange
        let dir = scratch("comments-fail");
        let fake = Fake::healthy()
            .with("pr view", Err("head failed".to_string()))
            .with("api /user", Err("user failed".to_string()));

        // Act
        let result = report(&fake, &dir, par::MAX_CONCURRENT);

        // Assert
        assert_eq!(result, Err("head failed".to_string()));
        assert!(fake.called("repo view") && fake.called("pr view") && fake.called("api /user"));
        assert!(!dir.join("pr-comments-7-threads.json").exists());
    }

    #[test]
    fn a_repo_failure_is_reported_before_a_later_call_fails() {
        let dir = scratch("comments-repo-fail");
        let fake = Fake::healthy()
            .with("repo view", Err("no repo".to_string()))
            .with("api /user", Err("user failed".to_string()));

        let result = report(&fake, &dir, par::MAX_CONCURRENT);

        assert_eq!(result, Err("no repo".to_string()));
        assert!(fake.called("api /user"));
        assert!(!fake.called("api graphql"));
    }

    #[test]
    fn a_failing_second_wave_call_reports_its_error_and_writes_nothing() {
        let dir = scratch("comments-wave-two");
        let fake = Fake::healthy().with(
            "api /repos/acme/widgets/issues/7/comments",
            Err("issues failed".to_string()),
        );

        let result = report(&fake, &dir, par::MAX_CONCURRENT);

        assert_eq!(result, Err("issues failed".to_string()));
        assert!(fake.called("api graphql"));
        assert!(!dir.join("pr-comments-7-threads.json").exists());
    }

    #[test]
    fn a_panicking_call_is_that_calls_error_and_the_others_still_run() {
        let dir = scratch("comments-panic");
        let mut fake = Fake::healthy();
        fake.panic_on = Some("repo view");

        let result = report(&fake, &dir, par::MAX_CONCURRENT);

        let error = result.unwrap_err();
        assert!(error.contains("repo view exploded"), "{error}");
        assert!(fake.called("pr view") && fake.called("api /user"));
        assert!(!fake.called("api graphql"));
    }

    #[test]
    fn when_both_second_wave_calls_fail_the_threads_error_comes_first() {
        let dir = scratch("comments-both-fail");
        let fake = Fake::healthy()
            .with("api graphql", Err("threads failed".to_string()))
            .with(
                "api /repos/acme/widgets/issues/7/comments",
                Err("issues failed".to_string()),
            );

        let result = report(&fake, &dir, par::MAX_CONCURRENT);

        assert_eq!(result, Err("threads failed".to_string()));
        assert!(fake.called("api /repos/acme/widgets/issues/7/comments"));
    }
}
