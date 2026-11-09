// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A scripted `GhClient` for the `pr` integration tests.

use playbook::pr::shared::{ExistingPr, GhClient};
use std::cell::RefCell;

/// A scripted `GhClient` that records every call it receives.
#[derive(Default)]
pub(crate) struct FakeGh {
    pub existing: Option<ExistingPr>,
    pub default_branch: Option<String>,
    pub created_url: String,
    /// What `pr_view_base` reports; `None` echoes the base the PR was created with.
    pub reported_base: Option<String>,
    /// Makes `pr_view_base` fail with this message.
    pub base_view_error: Option<String>,
    /// Head branches of open PRs, for stack detection.
    pub heads: Vec<String>,
    pub calls: RefCell<Vec<String>>,
    pub created_base: RefCell<String>,
    /// The `draft` flag the last `pr_create` call received.
    pub created_draft: std::cell::Cell<Option<bool>>,
}

impl FakeGh {
    // Only the `pr create` tests build a fake that creates; `pr prepare` never does.
    #[allow(dead_code)]
    pub(crate) fn creating(url: &str) -> Self {
        Self {
            created_url: url.to_string(),
            ..Self::default()
        }
    }

    pub(crate) fn count(&self, prefix: &str) -> usize {
        self.calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with(prefix))
            .count()
    }
}

impl GhClient for FakeGh {
    fn pr_view(&self, _branch: &str) -> Result<Option<ExistingPr>, String> {
        self.calls.borrow_mut().push("pr_view".into());
        Ok(self.existing.clone())
    }
    fn open_pr_heads(&self) -> Vec<String> {
        self.calls.borrow_mut().push("open_pr_heads".into());
        self.heads.clone()
    }
    fn repo_default_branch(&self) -> Result<Option<String>, String> {
        self.calls.borrow_mut().push("repo_default_branch".into());
        Ok(self.default_branch.clone())
    }
    fn pr_create(
        &self,
        _title: &str,
        _body_file: &str,
        base: &str,
        draft: bool,
    ) -> Result<String, String> {
        self.calls.borrow_mut().push("pr_create".into());
        self.created_draft.set(Some(draft));
        *self.created_base.borrow_mut() = base.to_string();
        Ok(self.created_url.clone())
    }
    fn pr_view_base(&self, _branch: &str) -> Result<String, String> {
        self.calls.borrow_mut().push("pr_view_base".into());
        if let Some(err) = &self.base_view_error {
            return Err(err.clone());
        }
        Ok(self
            .reported_base
            .clone()
            .unwrap_or_else(|| self.created_base.borrow().clone()))
    }
    fn pr_edit_base(&self, branch: &str, base: &str) -> Result<(), String> {
        self.calls
            .borrow_mut()
            .push(format!("pr_edit_base:{branch}:{base}"));
        Ok(())
    }
}
