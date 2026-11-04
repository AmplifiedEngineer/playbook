// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `review_pr` action: a model with no tools reads a pull request's diff
//! and the result is saved as a pending review that only you can see. The
//! diff is read through the API, so fork code is never checked out or run.

use super::claude::{parse_reply, review_call};
use super::clock::Clock;
use super::engine::{Engine, GithubWrite, IssueView, Step, HIDDEN_MARKER};
use super::rule::Rule;
use super::store::WorkRow;
use super::trust::wrap_untrusted;

/// How much of the diff the model reads, in characters.
const DIFF_LIMIT: usize = 60_000;
/// How much of the reply is kept, in characters.
const REVIEW_LIMIT: usize = 8_000;

impl<W: GithubWrite, C: Clock> Engine<'_, W, C> {
    /// Carries out `review_pr` for `row`. The review text comes from a model
    /// that read untrusted text, which is why it only ever becomes a pending
    /// review.
    pub(super) fn review(
        &mut self,
        row: &WorkRow,
        rule: &Rule,
        view: &IssueView,
    ) -> Result<Step, String> {
        let marker = format!("<!-- playbook-watch:{} -->", row.key);
        if self.write.has_review(&row.repo, row.object, &marker)? {
            return Ok(Step::AlreadyDone);
        }
        let Some(budget) = self.remaining_usd(row, rule)? else {
            return Ok(Step::Demote("limit"));
        };
        let diff = self.write.diff(&row.repo, row.object)?;
        let text = format!("Title: {}\n\n{diff}", view.title);
        let text: String = text.chars().take(DIFF_LIMIT).collect();
        let call = review_call(budget, &wrap_untrusted(&text));
        let reply = match self.model.run(&call).and_then(|raw| parse_reply(&raw)) {
            Ok(reply) => reply,
            Err(why) => {
                // The call may have run, so its worst case cost is spent.
                self.spend(row, budget)?;
                return Err(why);
            }
        };
        let cost = reply.charge(budget);
        let review: String = reply
            .text
            .as_deref()
            .unwrap_or_default()
            .trim()
            .chars()
            .take(REVIEW_LIMIT)
            .collect();
        if review.is_empty() {
            self.spend(row, cost)?;
            return Ok(Step::Demote("invalid_reply"));
        }
        let body = format!("{review}\n\n{HIDDEN_MARKER}\n{marker}");
        if let Err(why) = self
            .write
            .create_pending_review(&row.repo, row.object, &body)
        {
            self.spend(row, cost)?;
            return Err(why);
        }
        Ok(Step::Done(cost))
    }
}
