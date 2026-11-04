// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The writes watch mode makes, done by the `gh` program. `gh` is given the
//! token through its environment, never on a command line.

use std::process::Command;
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::engine::{GithubWrite, IssueView};
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
}

/// Reads the JSON `gh api repos/<repo>/issues/<n>` prints.
pub fn parse_issue(json: &str) -> Result<IssueView, String> {
    let issue: Issue =
        serde_json::from_str(json).map_err(|e| format!("the issue reply is not valid: {e}"))?;
    Ok(IssueView {
        open: issue.state == "open",
        labels: issue.labels.into_iter().map(|l| l.name).collect(),
        body_hash: Sha256::digest(issue.body.unwrap_or_default().as_bytes()).into(),
    })
}

/// `gh` calls for one token.
pub struct GhWrite {
    token: String,
}

impl GhWrite {
    pub fn new(token: String) -> Self {
        Self { token }
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

impl GithubWrite for GhWrite {
    fn issue(&self, repo: &str, number: u64) -> Result<IssueView, String> {
        let path = format!("repos/{repo}/issues/{number}");
        parse_issue(&self.gh(&["api", &path])?)
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

    fn has_comment(&self, repo: &str, number: u64, marker: &str) -> Result<bool, String> {
        let path = format!("repos/{repo}/issues/{number}/comments");
        let bodies = self.gh(&["api", "--paginate", &path, "--jq", ".[].body"])?;
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
        let json = r#"{"state":"open","body":"hi","labels":[{"name":"bug"},{"name":"x"}]}"#;

        // Act
        let view = parse_issue(json).unwrap();

        // Assert
        assert!(view.open);
        assert_eq!(view.labels, vec!["bug".to_string(), "x".to_string()]);
        assert_eq!(view.body_hash, <[u8; 32]>::from(Sha256::digest(b"hi")));
    }

    #[test]
    fn a_closed_issue_with_no_body_hashes_the_empty_string() {
        let view = parse_issue(r#"{"state":"closed","body":null}"#).unwrap();
        assert!(!view.open);
        assert_eq!(view.body_hash, <[u8; 32]>::from(Sha256::digest(b"")));
    }

    #[test]
    fn a_reply_that_is_not_json_is_an_error() {
        assert!(parse_issue("not json").is_err());
    }
}
