// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `pr prepare`: every judgment-free step that comes before drafting a PR's
//! title and body (branch and base, size and test checks, the diff, the
//! ticket), printed as labeled lines the calling command copies verbatim.

use crate::pr::shared::{
    current_branch, git, git_net, git_to_file, pr_state_dir, reject_option_like, resolve_base,
    GhClient,
};
use regex::Regex;
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

/// Above this many changed lines a PR is refused outright.
const HARD_LIMIT: u64 = 1500;
/// Above this the PR needs written justification.
const ENFORCED_LIMIT: u64 = 1000;
/// Above this the PR is flagged as large.
const SOFT_LIMIT: u64 = 500;

/// Test-file name patterns, anchored with `(^|/)` because git prints
/// repo-relative paths with no leading slash.
const TEST_PATH_PATTERN: &str =
    r"(?i)(\.test\.|\.spec\.|_test\.|test_|(^|/)tests?/|(^|/)__tests__/)";
/// A Rust inline test: a new `mod tests` block or just a new `#[test]` fn.
const INLINE_TEST_PATTERN: &str = r"^\+.*#\[(cfg\(test\)|test)\]";

/// What a prepare found, before it is rendered as text or JSON.
pub enum Outcome {
    /// An open PR already exists for the branch.
    Exists {
        url: String,
    },
    Ready(Box<Ready>),
}

/// A branch that is ready to be drafted into a PR.
pub struct Ready {
    pub branch: String,
    pub state_dir: std::path::PathBuf,
    pub base: String,
    /// `--base flag`, `repo default`, `git symbolic-ref`, `fallback` or `stack`.
    pub source: String,
    pub stacked: bool,
    pub ahead: u64,
    pub changed: u64,
    pub dirty: usize,
    pub tests: usize,
    pub fetch_warning: Option<String>,
    pub stat: String,
    pub log: String,
    pub diff_file: std::path::PathBuf,
    pub ticket: String,
}

/// `Ok` carries both a normal prepare and the "a PR already exists" redirect
/// (both exit 0); `Err` is reserved for the hard stops.
pub fn run(
    gh: &dyn GhClient,
    base_arg: Option<&str>,
    ticket_arg: Option<&str>,
) -> Result<String, String> {
    let branch = current_branch()?;
    let state_dir = pr_state_dir(&branch)?;
    prepare(&state_dir, &branch, gh, base_arg, ticket_arg)
}

/// The injectable core of `run`: `state_dir` and `branch` are parameters so a
/// test can point it at a scratch directory.
pub fn prepare(
    state_dir: &Path,
    branch: &str,
    gh: &dyn GhClient,
    base_arg: Option<&str>,
    ticket_arg: Option<&str>,
) -> Result<String, String> {
    Ok(render_text(&gather(
        state_dir, branch, gh, base_arg, ticket_arg, false,
    )?))
}

/// Every judgment-free fact about the branch, or the hard stop.
pub fn gather(
    state_dir: &Path,
    branch: &str,
    gh: &dyn GhClient,
    base_arg: Option<&str>,
    ticket_arg: Option<&str>,
    detect_stack: bool,
) -> Result<Outcome, String> {
    reject_option_like("branch", branch)?;
    fs::create_dir_all(state_dir)
        .map_err(|e| format!("could not create {}: {e}", state_dir.display()))?;

    if let Some(pr) = gh.pr_view(branch)? {
        if pr.state == "OPEN" {
            return Ok(Outcome::Exists { url: pr.url });
        }
    }

    let (mut base, mut source) = resolve_base(gh, base_arg)?;
    let mut stacked = false;
    if detect_stack && base_arg.is_none_or(str::is_empty) {
        let heads = gh.open_pr_heads();
        let parent = crate::pr::shared::nearest_parent(
            &heads,
            branch,
            &|remote| git(&["merge-base", "--is-ancestor", remote, "HEAD"]).is_ok(),
            &|range| {
                git(&["rev-list", "--count", range])
                    .ok()
                    .and_then(|n| n.parse().ok())
            },
        );
        if let Some(parent) = parent {
            base = parent;
            source = "stack";
            stacked = true;
        }
    }
    if branch == base {
        return Err(format!(
            "on the base branch ({base}); create a feature branch first"
        ));
    }
    // After the on-base check so an immediate abort never pays for a fetch.
    // A failed fetch is not fatal, but the output says the ref may be stale.
    let fetch_warning = git_net(&["fetch", "origin", &base, "--quiet"])
        .err()
        .map(|e| {
            // git's error spans lines; the output is one labeled line per fact.
            let reason: Vec<&str> = e.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
            let reason = reason.join("; ");
            format!(
                "WARN fetch: could not fetch origin/{base} ({reason}); results use the local ref"
            )
        });

    let remote_base = format!("origin/{base}");
    let ahead = git(&["rev-list", "--count", &format!("{remote_base}..HEAD")])
        .map_err(|e| format!("could not compare against {remote_base}: {e}"))?
        .parse::<u64>()
        .unwrap_or(0);
    if ahead == 0 {
        return Err(format!(
            "nothing ahead of {base}; there is nothing to open a PR for"
        ));
    }

    let range = format!("{remote_base}...HEAD");
    let changed = changed_lines(&git(&["diff", "--shortstat", &range]).unwrap_or_default());
    if changed > HARD_LIMIT {
        return Err(format!(
            "{changed} changed lines is over the {HARD_LIMIT}-line hard size limit; split the work into smaller PRs"
        ));
    }

    let dirty = git(&["status", "--porcelain"])
        .map(|out| out.lines().filter(|l| !l.is_empty()).count())
        .unwrap_or(0);
    let tests = test_blocks_touched(&range, state_dir)?;

    let diff_file = state_dir.join("pr-diff.txt");
    git_to_file(&["diff", &range], &diff_file)?;
    let stat = git(&["diff", "--stat", &range]).unwrap_or_default();
    let log = git(&["log", &format!("{remote_base}..HEAD"), "--format=%h %s"]).unwrap_or_default();

    Ok(Outcome::Ready(Box::new(Ready {
        branch: branch.to_string(),
        state_dir: state_dir.to_path_buf(),
        base,
        source: source.to_string(),
        stacked,
        ahead,
        changed,
        dirty,
        tests,
        fetch_warning,
        stat,
        log,
        diff_file,
        ticket: ticket(branch, ticket_arg),
    })))
}

/// The labeled lines the calling command copies verbatim.
pub fn render_text(outcome: &Outcome) -> String {
    let r = match outcome {
        Outcome::Exists { url } => {
            return format!(
                "A PR already exists: {url}\nUse /playbook:pr-comments or /playbook:quick-review instead."
            )
        }
        Outcome::Ready(r) => r,
    };
    let mut out = vec![
        format!("branch={}", r.branch),
        format!("state_dir={}", r.state_dir.display()),
        format!("base={} (source: {})", r.base, r.source),
        format!(
            "commits_ahead={} changed_lines={} dirty_files={} test_files_touched={}",
            r.ahead, r.changed, r.dirty, r.tests
        ),
        dirty_verdict(r.dirty),
        size_verdict(r.changed),
        tests_verdict(r.tests),
    ];
    out.extend(r.fetch_warning.clone());
    out.extend([
        "=== diff stat ===".to_string(),
        r.stat.clone(),
        "=== commit log ===".to_string(),
        r.log.clone(),
        format!("diff_file={}", r.diff_file.display()),
        format!("ticket={}", r.ticket),
    ]);
    out.join("\n")
}

fn size_label(changed: u64) -> &'static str {
    if changed > ENFORCED_LIMIT {
        "OVER"
    } else if changed > SOFT_LIMIT {
        "SOFT"
    } else {
        "OK"
    }
}

/// Inputs of the JSON form beyond the git facts.
pub struct JsonInput<'a> {
    pub root: &'a str,
    pub mode: &'a str,
    pub ready_flag: bool,
    pub draft_default: bool,
    pub issue: Option<&'a str>,
    pub rules: Option<&'a crate::pr::rules::RulesText>,
    /// Why the rule text is missing, when it is.
    pub rules_error: Option<&'a str>,
    pub max_body_chars: usize,
}

/// The JSON report of an outcome. `verdict` is `READY` or `PR_EXISTS`.
pub fn render_json(outcome: &Outcome, input: &JsonInput<'_>) -> serde_json::Value {
    use serde_json::json;
    match outcome {
        Outcome::Exists { url } => json!({
            "verdict": "PR_EXISTS", "reason": "an open PR already exists for the branch",
            "root": input.root, "existing_pr": url, "mode": input.mode,
        }),
        Outcome::Ready(r) => {
            let mut rules = serde_json::to_value(input.rules).unwrap_or_default();
            if !rules.is_object() {
                rules = json!({});
            }
            rules["max_body_chars"] = json!(input.max_body_chars);
            rules["error"] = json!(input.rules_error);
            let commits: Vec<&str> = r.log.lines().filter(|l| !l.is_empty()).collect();
            json!({
                "verdict": "READY", "reason": "",
                "root": input.root, "branch": r.branch, "base": r.base,
                "base_source": match r.source.as_str() {
                    "stack" => "stack",
                    "--base flag" => "flag",
                    _ => "default",
                },
                "stacked": r.stacked, "commits_ahead": r.ahead,
                "changed_lines": r.changed, "dirty_files": r.dirty, "tests_touched": r.tests,
                "checks": {
                    "dirty": if r.dirty > 0 { "WARN" } else { "OK" },
                    "size": size_label(r.changed),
                    "tests": if r.tests == 0 { "NONE" } else { "OK" },
                },
                "ticket": r.ticket, "issue": input.issue, "existing_pr": null,
                "fetch_warning": r.fetch_warning,
                "diff_stat": r.stat, "commit_log": commits,
                "diff_file": r.diff_file.display().to_string(),
                "state_dir": r.state_dir.display().to_string(),
                "draft_default": input.draft_default, "ready_flag": input.ready_flag,
                "rules": rules,
                "mode": input.mode,
            })
        }
    }
}

/// A refusal in JSON: `REFUSED` or `WRONG_DIRECTORY`, in the envelope with
/// the plain mode exit code 1.
pub fn refusal_json(verdict: &str, reason: &str, mode: &str) -> serde_json::Value {
    crate::json::envelope::wrap(
        1,
        reason,
        serde_json::json!({ "verdict": verdict, "reason": reason, "mode": mode }),
    )
}

/// The whole `--json` run: parse the inline text, check the worktree, move
/// into it, gather the facts and render them. Returns the report and the exit
/// code plain mode would use (0 for READY and PR_EXISTS, 1 for a refusal).
pub fn run_json(
    gh: &dyn GhClient,
    args_text: Option<&str>,
    base: Option<String>,
    ticket: Option<String>,
    dir: Option<String>,
) -> (serde_json::Value, i32) {
    use crate::common::inline_args;
    let home = crate::common::home_dir();
    let parsed = match inline_args::parse(args_text.unwrap_or("")) {
        Ok(p) => p,
        Err(e) => {
            return (
                refusal_json("REFUSED", &format!("{}: {}", e.code, e.message), "ask"),
                1,
            )
        }
    };
    let slug = crate::common::repo_slug();
    let mode = parsed
        .mode_at(
            std::env::var("PLAYBOOK_MODE").ok().as_deref(),
            &home,
            (!slug.is_empty()).then_some(slug.as_str()),
        )
        .mode;
    let mode = if mode == crate::common::mode::Mode::Auto {
        "auto"
    } else {
        "ask"
    };
    let target = parsed.root.clone().or(parsed.dir.clone()).or(dir);
    if let (Some(root), Some(branch)) = (&parsed.root, &parsed.branch) {
        if let Err(reason) = inline_args::guard(Path::new(root), branch) {
            return (refusal_json("WRONG_DIRECTORY", &reason, mode), 1);
        }
    }
    if let Some(target) = &target {
        if let Err(reason) = crate::pr::shared::enter_dir(target) {
            return (refusal_json("REFUSED", &reason, mode), 1);
        }
    }
    let base = parsed.base.clone().or(base);
    let ticket = parsed.ticket.clone().or(ticket);
    let run = || -> Result<serde_json::Value, String> {
        let branch = current_branch()?;
        let state_dir = pr_state_dir(&branch)?;
        let outcome = gather(
            &state_dir,
            &branch,
            gh,
            base.as_deref(),
            ticket.as_deref(),
            true,
        )?;
        let top = git(&["rev-parse", "--show-toplevel"])?;
        let repo = (!slug.is_empty()).then_some(slug.as_str());
        let rules = crate::pr::body_rules::Rules::load(&home, repo);
        let texts = crate::pr::rules::extract_texts(&crate::pr::rules::plugin_root_quiet());
        let input = JsonInput {
            root: &top,
            mode,
            ready_flag: parsed.ready,
            draft_default: crate::pr::create::draft_setting(&home, repo),
            issue: parsed.issue.as_deref(),
            rules: texts.as_ref().ok(),
            rules_error: texts.as_ref().err().map(String::as_str),
            max_body_chars: rules.max_chars,
        };
        Ok(render_json(&outcome, &input))
    };
    match run() {
        Ok(report) => (crate::json::envelope::wrap(0, "", report), 0),
        Err(reason) => (refusal_json("REFUSED", &reason, mode), 1),
    }
}

/// Insertions plus deletions from a `git diff --shortstat` line, or 0.
fn changed_lines(shortstat: &str) -> u64 {
    shortstat
        .split(',')
        .filter(|part| part.contains("insertion") || part.contains("deletion"))
        .filter_map(|part| part.split_whitespace().next()?.parse::<u64>().ok())
        .sum()
}

/// Test files in the diff plus Rust inline test blocks added.
fn test_blocks_touched(range: &str, state_dir: &Path) -> Result<usize, String> {
    static PATHS: LazyLock<Result<Regex, String>> =
        LazyLock::new(|| Regex::new(TEST_PATH_PATTERN).map_err(|e| e.to_string()));
    let paths = PATHS.as_ref().map_err(Clone::clone)?;
    let files = git(&["diff", "--name-only", range]).unwrap_or_default();
    let by_name = files.lines().filter(|f| paths.is_match(f)).count();

    // The Rust diff goes through a file: it can exceed what a pipe holds.
    let rs_diff = state_dir.join("pr-rs-diff.txt");
    git_to_file(&["diff", "-U0", range, "--", "*.rs"], &rs_diff)?;
    static INLINE: LazyLock<Result<Regex, String>> =
        LazyLock::new(|| Regex::new(INLINE_TEST_PATTERN).map_err(|e| e.to_string()));
    let inline = INLINE.as_ref().map_err(Clone::clone)?;
    let by_inline = fs::read_to_string(&rs_diff)
        .unwrap_or_default()
        .lines()
        .filter(|l| inline.is_match(l))
        .count();
    Ok(by_name + by_inline)
}

fn dirty_verdict(dirty: usize) -> String {
    if dirty > 0 {
        format!("VERDICT dirty: WARN - {dirty} uncommitted file(s) will NOT be in the PR")
    } else {
        "VERDICT dirty: OK - nothing uncommitted".to_string()
    }
}

fn size_verdict(changed: u64) -> String {
    if changed > ENFORCED_LIMIT {
        format!("VERDICT size: OVER - {changed} lines is above the {ENFORCED_LIMIT}-line enforced limit and needs explicit justification in the PR body")
    } else if changed > SOFT_LIMIT {
        format!("VERDICT size: SOFT - {changed} lines is above the {SOFT_LIMIT}-line soft limit")
    } else {
        format!("VERDICT size: OK - {changed} lines")
    }
}

fn tests_verdict(tests: usize) -> String {
    if tests == 0 {
        "VERDICT tests: NONE - the diff adds no test files or inline test blocks; the readiness criteria expect tests for behaviour changes".to_string()
    } else {
        format!("VERDICT tests: OK - {tests} test file(s) or inline test block(s) touched")
    }
}

/// `--ticket` verbatim (it may be the literal `none`), else the first
/// `PROJECT-1234` token in the branch name, else empty.
fn ticket(branch: &str, ticket_arg: Option<&str>) -> String {
    if let Some(arg) = ticket_arg.filter(|t| !t.is_empty()) {
        return arg.to_string();
    }
    // The same detection `pr create` enforces, so the drafter is told the
    // ticket that will be checked.
    let slug = crate::common::repo_slug();
    let rules = crate::pr::body_rules::Rules::load(
        &crate::common::home_dir(),
        (!slug.is_empty()).then_some(slug.as_str()),
    );
    crate::pr::body_rules::detect_ticket(None, branch, &[], &rules.patterns).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_lines_sums_insertions_and_deletions() {
        // Arrange
        let stat = " 3 files changed, 120 insertions(+), 30 deletions(-)";

        // Act / Assert
        assert_eq!(changed_lines(stat), 150);
        assert_eq!(changed_lines(" 1 file changed, 7 insertions(+)"), 7);
        assert_eq!(changed_lines(" 1 file changed, 4 deletions(-)"), 4);
        assert_eq!(changed_lines(""), 0);
    }
}
