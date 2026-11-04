// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The pull request state of a branch, read with `gh`, for the sweep. A squash
//! merge leaves the branch out of `git branch --merged`, so git alone cannot
//! tell that such a branch landed. Every failure reads as "no information".

use super::PrState;
use crate::common::git;
use crate::common::proc::run_with_timeout;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const GH_TIMEOUT: Duration = Duration::from_secs(5);

/// The state of the newest pull request of `branch`, or `None` when `gh` is
/// missing, offline, finds none, or the PR is closed unmerged. A merged PR only
/// counts when its head is the local branch tip, so commits made after the
/// merge, or a reused branch name, never read as landed.
pub(crate) fn gh_pr_state(repo_root: &Path, branch: &str) -> Option<PrState> {
    let tip = git::output_slow(repo_root, &["rev-parse", &format!("refs/heads/{branch}")])?;
    let mut command = Command::new("gh");
    command
        .current_dir(repo_root)
        .args([
            "pr", "list", "--head", branch, "--state", "all", "--limit", "1",
        ])
        .args(["--json", "state,headRefOid"]);
    let out = run_with_timeout(&mut command, GH_TIMEOUT).filter(|o| o.status.success())?;
    parse_pr_state(&String::from_utf8_lossy(&out.stdout), tip.trim())
}

fn parse_pr_state(json: &str, local_tip: &str) -> Option<PrState> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let first = value.as_array()?.first()?;
    match first.get("state")?.as_str()? {
        "OPEN" => Some(PrState::Open),
        "MERGED" if first.get("headRefOid")?.as_str()? == local_tip => Some(PrState::Merged),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIP: &str = "abc123";

    #[test]
    fn a_merged_pr_at_the_local_tip_is_merged() {
        let json = r#"[{"state":"MERGED","headRefOid":"abc123"}]"#;
        assert_eq!(parse_pr_state(json, TIP), Some(PrState::Merged));
    }

    #[test]
    fn a_merged_pr_with_another_head_is_unknown() {
        let json = r#"[{"state":"MERGED","headRefOid":"def456"}]"#;
        assert_eq!(parse_pr_state(json, TIP), None);
    }

    #[test]
    fn an_open_pr_is_open_whatever_its_head() {
        let json = r#"[{"state":"OPEN","headRefOid":"def456"}]"#;
        assert_eq!(parse_pr_state(json, TIP), Some(PrState::Open));
    }

    #[test]
    fn closed_none_and_garbage_are_unknown() {
        for json in [
            r#"[{"state":"CLOSED","headRefOid":"abc123"}]"#,
            "[]",
            "not json",
            r#"[{"state":"MERGED"}]"#,
        ] {
            assert_eq!(parse_pr_state(json, TIP), None, "{json}");
        }
    }
}
