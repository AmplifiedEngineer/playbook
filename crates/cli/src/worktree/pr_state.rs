// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The pull request state of a branch, read with `gh`, for the sweep. A squash
//! merge leaves the branch out of `git branch --merged`, so git alone cannot
//! tell that such a branch landed. Every failure reads as "no information".

use super::PrState;
use crate::common::git;
use crate::common::proc::run_with_timeout;
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const GH_TIMEOUT: Duration = Duration::from_secs(5);

/// The state of the pull requests of `branch`, or `None` when `gh` is missing,
/// offline, finds none, or none is open or merged. An open pull request wins
/// over any other, so a newer closed one never hides it. A merged one only
/// counts when it targeted the default branch and its head is the local branch
/// tip, so a stacked PR, commits made after the merge, or a reused branch name
/// never read as landed.
pub(super) fn gh_pr_state(repo_root: &Path, branch: &str) -> Option<PrState> {
    let tip = git::output_slow(repo_root, &["rev-parse", &format!("refs/heads/{branch}")])?;
    let default = super::default_branch(repo_root);
    let mut command = Command::new("gh");
    command
        .current_dir(repo_root)
        .args([
            "pr", "list", "--head", branch, "--state", "all", "--limit", "10",
        ])
        .args(["--json", "state,headRefOid,baseRefName"]);
    let out = run_with_timeout(&mut command, GH_TIMEOUT).filter(|o| o.status.success())?;
    parse_pr_state(
        &String::from_utf8_lossy(&out.stdout),
        tip.trim(),
        default.as_deref(),
    )
}

fn parse_pr_state(json: &str, local_tip: &str, default: Option<&str>) -> Option<PrState> {
    let value: Value = serde_json::from_str(json).ok()?;
    let prs = value.as_array()?;
    let field = |pr: &Value, key: &str| pr.get(key).and_then(Value::as_str).map(str::to_string);
    if prs
        .iter()
        .any(|pr| field(pr, "state").as_deref() == Some("OPEN"))
    {
        return Some(PrState::Open);
    }
    let default = default?;
    let merged_here = prs.iter().any(|pr| {
        field(pr, "state").as_deref() == Some("MERGED")
            && field(pr, "headRefOid").as_deref() == Some(local_tip)
            && field(pr, "baseRefName").as_deref() == Some(default)
    });
    merged_here.then_some(PrState::Merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIP: &str = "abc123";
    const MAIN: Option<&str> = Some("main");

    fn parse(json: &str) -> Option<PrState> {
        parse_pr_state(json, TIP, MAIN)
    }

    #[test]
    fn a_merged_pr_at_the_local_tip_is_merged() {
        let json = r#"[{"state":"MERGED","headRefOid":"abc123","baseRefName":"main"}]"#;
        assert_eq!(parse(json), Some(PrState::Merged));
    }

    #[test]
    fn a_merged_pr_with_another_head_is_unknown() {
        let json = r#"[{"state":"MERGED","headRefOid":"def456","baseRefName":"main"}]"#;
        assert_eq!(parse(json), None);
    }

    #[test]
    fn a_merged_pr_into_another_base_is_unknown() {
        let json = r#"[{"state":"MERGED","headRefOid":"abc123","baseRefName":"feat-a"}]"#;
        assert_eq!(parse(json), None);
    }

    #[test]
    fn a_merged_pr_with_no_known_default_is_unknown() {
        let json = r#"[{"state":"MERGED","headRefOid":"abc123","baseRefName":"main"}]"#;
        assert_eq!(parse_pr_state(json, TIP, None), None);
    }

    #[test]
    fn a_merged_pr_is_found_behind_a_newer_closed_one() {
        let json = r#"[{"state":"CLOSED","headRefOid":"def456","baseRefName":"main"},
            {"state":"MERGED","headRefOid":"abc123","baseRefName":"main"}]"#;
        assert_eq!(parse(json), Some(PrState::Merged));
    }

    #[test]
    fn an_open_pr_is_open_whatever_its_head() {
        let json = r#"[{"state":"OPEN","headRefOid":"def456","baseRefName":"main"}]"#;
        assert_eq!(parse(json), Some(PrState::Open));
    }

    #[test]
    fn an_open_pr_is_not_hidden_by_a_newer_closed_one() {
        let json = r#"[{"state":"CLOSED","headRefOid":"abc123","baseRefName":"main"},
            {"state":"OPEN","headRefOid":"abc123","baseRefName":"main"}]"#;
        assert_eq!(parse(json), Some(PrState::Open));
    }

    #[test]
    fn closed_none_and_garbage_are_unknown() {
        for json in [
            r#"[{"state":"CLOSED","headRefOid":"abc123","baseRefName":"main"}]"#,
            "[]",
            "not json",
            r#"[{"state":"MERGED"}]"#,
        ] {
            assert_eq!(parse(json), None, "{json}");
        }
    }
}
