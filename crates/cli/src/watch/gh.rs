// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The writes watch mode makes, done by the `gh` program. `gh` is given the
//! token through its environment, never on a command line.

use std::process::Command;
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::engine::{BranchUpdate, GithubWrite, IssueView, MergeCheck, Post, HIDDEN_MARKER};
use super::journal::RecoverOps;
use super::store::WorkRow;
use crate::common::proc::run_with_timeout;

const GH_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
struct Label {
    name: String,
}

#[derive(Deserialize)]
struct Issue {
    state: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    labels: Vec<Label>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    pull_request: Option<serde::de::IgnoredAny>,
}

/// Reads the JSON `gh api repos/<repo>/issues/<n>` prints.
///
/// `fork` stays false here. The reply says whether the issue is a pull
/// request through `is_pull_request`, and the caller asks the pulls API.
pub fn parse_issue(json: &str) -> Result<(IssueView, bool), String> {
    let issue: Issue =
        serde_json::from_str(json).map_err(|e| format!("the issue reply is not valid: {e}"))?;
    let body = issue.body.unwrap_or_default();
    let view = IssueView {
        open: issue.state == "open",
        labels: issue.labels.into_iter().map(|l| l.name).collect(),
        body_hash: Sha256::digest(body.as_bytes()).into(),
        title: issue.title,
        body,
        fork: false,
    };
    Ok((view, issue.pull_request.is_some()))
}

#[derive(Deserialize)]
struct RawPost {
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    association: Option<String>,
    #[serde(default)]
    body: Option<String>,
}

/// Reads one JSON object per line. A missing author becomes the empty login,
/// which no one trusts.
pub fn parse_posts(text: &str) -> Result<Vec<Post>, String> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let raw: RawPost =
                serde_json::from_str(line).map_err(|e| format!("a comment is not valid: {e}"))?;
            Ok(Post {
                author: raw.author.unwrap_or_default(),
                association: raw.association.unwrap_or_default(),
                body: raw.body.unwrap_or_default(),
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct RawReview {
    user: Option<String>,
    #[serde(default)]
    association: String,
    state: String,
    #[serde(default)]
    commit_id: String,
}

/// What the reviews of a pull request say about its current head.
#[derive(Debug, PartialEq, Eq)]
struct Reviews {
    /// Latest review is an approval of the head: login and association.
    approvers: Vec<(String, String)>,
    /// Someone's latest review asks for changes, on any commit.
    changes_requested: bool,
}

/// The reviewers whose latest review is an approval, from one JSON object per
/// line in the order GitHub lists them. A later comment does not undo an
/// approval, but a dismissal or a change request does. An approval of an older
/// commit does not count: only the head `head` was approved by a person.
fn approvers(text: &str, head: &str) -> Result<Reviews, String> {
    let mut latest: Vec<(String, String, String, String)> = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let review: RawReview =
            serde_json::from_str(line).map_err(|e| format!("a review is not valid: {e}"))?;
        let Some(user) = review.user else { continue };
        if review.state == "COMMENTED" {
            continue;
        }
        latest.retain(|(login, _, _, _)| login != &user);
        latest.push((user, review.association, review.state, review.commit_id));
    }
    let changes_requested = latest
        .iter()
        .any(|(_, _, state, _)| state == "CHANGES_REQUESTED");
    let approvers = latest
        .into_iter()
        .filter(|(_, _, state, commit)| state == "APPROVED" && commit == head)
        .map(|(login, association, _, _)| (login, association))
        .collect();
    Ok(Reviews {
        approvers,
        changes_requested,
    })
}

/// Reads `<mergeable_state> <head sha>`.
fn parse_pull_state(text: &str) -> Result<(&str, &str), String> {
    text.trim()
        .split_once(' ')
        .ok_or_else(|| "the pull request state is not valid".to_string())
}

/// `gh` calls for one token.
pub struct GhWrite {
    token: String,
    me: std::sync::OnceLock<String>,
}

impl GhWrite {
    pub fn new(token: String) -> Self {
        Self {
            token,
            me: std::sync::OnceLock::new(),
        }
    }

    /// The jq program that keeps only the bodies written by the token's own
    /// user. A marker from anyone else is not ours, so a stranger cannot make
    /// an action look already done.
    fn mine_jq(&self) -> Result<String, String> {
        let me = match self.me.get() {
            Some(me) => me.clone(),
            None => {
                let me = self.login()?;
                if me.is_empty() || !me.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                    return Err("the token user is not a plain login".to_string());
                }
                self.me.get_or_init(|| me).clone()
            }
        };
        Ok(format!(r#".[] | select(.user.login == "{me}") | .body"#))
    }

    fn gh(&self, args: &[&str]) -> Result<String, String> {
        let mut command = Command::new("gh");
        command.args(args).env("GH_TOKEN", &self.token);
        let output = run_with_timeout(&mut command, GH_TIMEOUT)
            .ok_or_else(|| "gh did not finish (missing, or timed out)".to_string())?;
        if !output.status.success() {
            // stderr from gh never holds the token: it is only in the env.
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// The login the token belongs to.
    pub fn login(&self) -> Result<String, String> {
        self.gh(&["api", "user", "--jq", ".login"])
    }
}

impl RecoverOps for GhWrite {
    fn remove_worktree(&self, path: &str) -> Result<(), String> {
        let mut command = Command::new("git");
        command.args(["worktree", "remove", "--force", path]);
        let output = run_with_timeout(&mut command, GH_TIMEOUT)
            .ok_or_else(|| "git did not finish (missing, or timed out)".to_string())?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    }

    fn branch_pushed(&self, repo: &str, branch: &str) -> Result<bool, String> {
        match self.gh(&["api", &format!("repos/{repo}/branches/{branch}")]) {
            Ok(_) => Ok(true),
            Err(why) if why.contains("Not Found") => Ok(false),
            Err(why) => Err(why),
        }
    }

    fn ensure_marker(&self, row: &WorkRow) -> Result<(), String> {
        let marker = format!("<!-- playbook-watch:{} -->", row.key);
        if self.has_comment(&row.repo, row.object, &marker)? {
            return Ok(());
        }
        self.add_comment(&row.repo, row.object, &format!("{HIDDEN_MARKER}\n{marker}"))
    }
}

impl GithubWrite for GhWrite {
    fn issue(&self, repo: &str, number: u64) -> Result<IssueView, String> {
        let path = format!("repos/{repo}/issues/{number}");
        let (mut view, is_pull_request) = parse_issue(&self.gh(&["api", &path])?)?;
        if is_pull_request {
            let pull = format!("repos/{repo}/pulls/{number}");
            // A deleted fork has no head repo, which counts as a fork too.
            let jq = r#"(.head.repo.full_name // "") != .base.repo.full_name"#;
            view.fork = self.gh(&["api", &pull, "--jq", jq])? == "true";
        }
        Ok(view)
    }

    fn add_labels(&self, repo: &str, number: u64, labels: &[String]) -> Result<(), String> {
        let path = format!("repos/{repo}/issues/{number}/labels");
        let mut args = vec!["api", "--method", "POST", &path];
        let fields: Vec<String> = labels.iter().map(|l| format!("labels[]={l}")).collect();
        for field in &fields {
            args.extend(["-f", field]);
        }
        self.gh(&args).map(|_| ())
    }

    fn merge_check(&self, repo: &str, number: u64) -> Result<MergeCheck, String> {
        let path = format!("repos/{repo}/pulls/{number}");
        let jq = r#".mergeable_state + " " + .head.sha + " " + .user.login + " " + ((.requested_reviewers | length) + (.requested_teams | length) | tostring)"#;
        let pull = self.gh(&["api", &path, "--jq", jq])?;
        let mut fields = pull.split_whitespace();
        let (Some(state), Some(head), Some(author), Some(requested)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err("the pull request state is not valid".to_string());
        };
        let reviews = format!("{path}/reviews");
        let jq = r#".[] | {user: .user.login, association: .author_association, state: .state, commit_id: .commit_id}"#;
        let reviews = approvers(
            &self.gh(&["api", "--paginate", &reviews, "--jq", jq])?,
            head,
        )?;
        // `clean` can show before any check has started on a new head, so at
        // least one finished check run must exist and none may be unfinished
        // or failed.
        let runs = format!("repos/{repo}/commits/{head}/check-runs?per_page=100");
        let jq = r#"[.total_count, ([.check_runs[] | select(.status != "completed" or (.conclusion | IN("success", "neutral", "skipped") | not))] | length)] | map(tostring) | join(" ")"#;
        let counts = self.gh(&["api", &runs, "--jq", jq])?;
        let checks_ok = match counts.split_once(' ') {
            Some((total, bad)) => total != "0" && bad == "0",
            None => false,
        };
        Ok(MergeCheck {
            green: state == "clean" && checks_ok,
            head: head.to_string(),
            author: author.to_string(),
            approvers: reviews.approvers,
            changes_requested: reviews.changes_requested,
            review_requested: requested != "0",
        })
    }

    fn merge(&self, repo: &str, number: u64, head: &str) -> Result<(), String> {
        // `sha` makes GitHub refuse the merge when a push landed after the check.
        let path = format!("repos/{repo}/pulls/{number}/merge");
        let sha = format!("sha={head}");
        self.gh(&["api", "--method", "PUT", &path, "-f", &sha])
            .map(|_| ())
    }

    fn diff(&self, repo: &str, number: u64) -> Result<String, String> {
        let path = format!("repos/{repo}/pulls/{number}");
        self.gh(&["api", &path, "-H", "Accept: application/vnd.github.diff"])
    }

    fn has_review(&self, repo: &str, number: u64, marker: &str) -> Result<bool, String> {
        let path = format!("repos/{repo}/pulls/{number}/reviews");
        let bodies = self.gh(&["api", "--paginate", &path, "--jq", &self.mine_jq()?])?;
        Ok(bodies.contains(marker))
    }

    fn create_pending_review(&self, repo: &str, number: u64, body: &str) -> Result<(), String> {
        // No `event` field: GitHub keeps the review pending until you submit it.
        let path = format!("repos/{repo}/pulls/{number}/reviews");
        let field = format!("body={body}");
        self.gh(&["api", "--method", "POST", &path, "-f", &field])
            .map(|_| ())
    }

    fn update_branch(&self, repo: &str, number: u64) -> Result<BranchUpdate, String> {
        let path = format!("repos/{repo}/pulls/{number}");
        let jq = r#".mergeable_state + " " + .head.sha"#;
        let reply = self.gh(&["api", &path, "--jq", jq])?;
        let (state, head) = parse_pull_state(&reply)?;
        match state {
            "behind" => {}
            "dirty" => return Ok(BranchUpdate::Conflict),
            // GitHub is still working it out: try again on a later cycle.
            "unknown" => return Err("GitHub has not worked out the mergeable state yet".into()),
            _ => return Ok(BranchUpdate::UpToDate),
        }
        // The head the state was read at, so a push in between is not merged over.
        let sha = format!("expected_head_sha={head}");
        let update = format!("{path}/update-branch");
        self.gh(&["api", "--method", "PUT", &update, "-f", &sha])
            .map(|_| BranchUpdate::Updated)
    }

    fn thread(&self, repo: &str, number: u64) -> Result<Vec<Post>, String> {
        const POST: &str =
            r#"{author: .user.login, association: .author_association, body: .body}"#;
        let each = format!(".[] | {POST}");
        let sources = [
            (format!("repos/{repo}/issues/{number}"), POST),
            (
                format!("repos/{repo}/issues/{number}/comments"),
                each.as_str(),
            ),
            (
                format!("repos/{repo}/pulls/{number}/comments"),
                each.as_str(),
            ),
        ];
        let mut posts = Vec::new();
        for (path, jq) in &sources {
            posts.extend(parse_posts(&self.gh(&[
                "api",
                "--paginate",
                path,
                "--jq",
                jq,
            ])?)?);
        }
        Ok(posts)
    }

    fn has_comment(&self, repo: &str, number: u64, marker: &str) -> Result<bool, String> {
        let path = format!("repos/{repo}/issues/{number}/comments");
        let bodies = self.gh(&["api", "--paginate", &path, "--jq", &self.mine_jq()?])?;
        Ok(bodies.contains(marker))
    }

    fn add_comment(&self, repo: &str, number: u64, body: &str) -> Result<(), String> {
        let path = format!("repos/{repo}/issues/{number}/comments");
        let field = format!("body={body}");
        self.gh(&["api", "--method", "POST", &path, "-f", &field])
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_open_issue_reads_labels_and_a_body_hash() {
        // Arrange
        let json =
            r#"{"state":"open","title":"T","body":"hi","labels":[{"name":"bug"},{"name":"x"}]}"#;

        // Act
        let (view, is_pr) = parse_issue(json).unwrap();
        assert!(!is_pr);
        assert_eq!((view.title.as_str(), view.body.as_str()), ("T", "hi"));

        // Assert
        assert!(view.open);
        assert_eq!(view.labels, vec!["bug".to_string(), "x".to_string()]);
        assert_eq!(view.body_hash, <[u8; 32]>::from(Sha256::digest(b"hi")));
    }

    #[test]
    fn a_closed_issue_with_no_body_hashes_the_empty_string() {
        let (view, _) = parse_issue(r#"{"state":"closed","body":null}"#).unwrap();
        assert!(!view.open);
        assert_eq!(view.body_hash, <[u8; 32]>::from(Sha256::digest(b"")));
    }

    #[test]
    fn a_pull_request_is_recognised_by_its_pull_request_key() {
        let (_, is_pr) = parse_issue(r#"{"state":"open","pull_request":{"url":"u"}}"#).unwrap();
        assert!(is_pr);
    }

    #[test]
    fn posts_are_read_one_per_line_and_a_missing_author_is_empty() {
        // Arrange
        let text = "{\"author\":\"ann\",\"association\":\"MEMBER\",\"body\":\"hi\"}\n\n{\"author\":null,\"body\":null}";

        // Act
        let posts = parse_posts(text).unwrap();

        // Assert
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[0].association, "MEMBER");
        assert_eq!((posts[1].author.as_str(), posts[1].body.as_str()), ("", ""));
        assert!(parse_posts("not json").is_err());
    }

    #[test]
    fn only_the_latest_non_comment_review_of_each_user_counts() {
        // Arrange: ann approves then asks for changes, bob approves, cy only comments.
        let text = [
            r#"{"user":"ann","association":"MEMBER","state":"APPROVED","commit_id":"h1"}"#,
            r#"{"user":"bob","association":"OWNER","state":"APPROVED","commit_id":"h1"}"#,
            r#"{"user":"ann","association":"MEMBER","state":"CHANGES_REQUESTED"}"#,
            r#"{"user":"cy","association":"NONE","state":"COMMENTED"}"#,
            r#"{"user":null,"association":"NONE","state":"APPROVED"}"#,
        ]
        .join("\n");

        // Act / Assert
        assert_eq!(
            approvers(&text, "h1").unwrap(),
            Reviews {
                approvers: vec![("bob".to_string(), "OWNER".to_string())],
                changes_requested: true,
            }
        );
    }

    #[test]
    fn an_approval_of_an_older_commit_does_not_count() {
        let text = r#"{"user":"bob","association":"OWNER","state":"APPROVED","commit_id":"old"}"#;
        let reviews = approvers(text, "new").unwrap();
        assert!(reviews.approvers.is_empty());
        assert!(!reviews.changes_requested);
    }

    #[test]
    fn a_pull_state_is_the_mergeable_state_then_the_head() {
        assert_eq!(
            parse_pull_state("behind abc123\n"),
            Ok(("behind", "abc123"))
        );
        assert!(parse_pull_state("behind").is_err());
    }

    #[test]
    fn a_reply_that_is_not_json_is_an_error() {
        assert!(parse_issue("not json").is_err());
    }
}
