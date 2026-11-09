// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The text checks of a delivery as plain sentences. The rules live in
//! `pr::text_rules`; this module keeps the string form `deliver` reports.

use crate::pr::text_rules::{self, messages};

pub use text_rules::TITLE_LIMIT;

/// Every problem with a commit message, empty when it is fine.
pub fn commit_message(message: &str) -> Vec<String> {
    messages(text_rules::commit_message(message))
}

/// Every problem with a PR title, empty when it is fine.
pub fn pr_title(title: &str) -> Vec<String> {
    messages(text_rules::pr_title(title))
}

/// Every problem with a PR body, empty when it is fine.
pub fn pr_body(body: &str) -> Vec<String> {
    messages(text_rules::pr_body(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scoped_conventional_title_passes() {
        assert!(commit_message("feat(deliver): add the command").is_empty());
    }

    #[test]
    fn a_title_without_a_type_is_refused() {
        assert!(!commit_message("add the command").is_empty());
    }

    #[test]
    fn a_title_over_the_limit_is_refused_with_its_length() {
        let title = format!("feat: {}", "x".repeat(70));

        let problems = pr_title(&title);

        assert!(
            problems.iter().any(|p| p.contains("76 characters")),
            "got {problems:?}"
        );
    }

    #[test]
    fn an_em_dash_in_the_body_of_a_commit_is_refused() {
        let problems = commit_message("fix: a thing\n\nbecause \u{2014} reasons");

        assert!(
            problems.iter().any(|p| p.contains("em dash")),
            "got {problems:?}"
        );
    }

    #[test]
    fn a_dash_inside_code_is_allowed() {
        assert!(pr_body("Use `a \u{2013} b` here\n\n```\nx \u{2014} y\n```\n").is_empty());
    }

    #[test]
    fn a_claude_trailer_is_refused() {
        let problems =
            commit_message("fix: a thing\n\nCo-Authored-By: Claude <noreply@anthropic.com>");

        assert!(
            problems.iter().any(|p| p.contains("AI attribution")),
            "got {problems:?}"
        );
    }
}
