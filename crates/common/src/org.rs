// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The GitHub organization names playbook trusts.
//!
//! The organization was renamed from `pragmatic-engineer` to
//! `amplified-engineer`. GitHub redirects the old name, but nothing here may
//! depend on that, and a binary or plugin installed before the rename still
//! carries the old name. Code that builds a URL uses the new name. Code that
//! checks identity (a download, an attestation, an installed plugin key, a
//! cache folder) accepts both and tries the new name first.

/// The one place the current organization name is written. A macro, because
/// `concat!` takes only literals, and every repo constant below derives from it.
/// A later rename changes this line and the docs.
macro_rules! current_org {
    () => {
        "amplified-engineer"
    };
}

/// The current organization.
pub const ORG: &str = current_org!();
/// The organization before the rename. Old installs and old releases use it.
pub const LEGACY_ORG: &str = "pragmatic-engineer";
/// Every organization name to accept, preferred name first.
pub const ORGS: [&str; 2] = [ORG, LEGACY_ORG];

/// The playbook repository under the current organization.
pub const PLAYBOOK_REPO: &str = concat!(current_org!(), "/playbook");
/// The playbook repository under the old organization.
pub const LEGACY_PLAYBOOK_REPO: &str = "pragmatic-engineer/playbook";
/// Every playbook repository to accept, preferred name first.
pub const PLAYBOOK_REPOS: [&str; 2] = [PLAYBOOK_REPO, LEGACY_PLAYBOOK_REPO];

/// The marketplace repository, for `claude plugin marketplace add`.
pub const MARKETPLACE_REPO: &str = concat!(current_org!(), "/marketplace");
/// The fully qualified Homebrew formula, for `brew install`.
pub const BREW_FORMULA: &str = concat!(current_org!(), "/tap/playbook");
/// The fully qualified Homebrew formula for the pbtop viewer.
pub const BREW_PBTOP_FORMULA: &str = concat!(current_org!(), "/tap/pbtop");

/// Whether `owner` is the current or the old organization name, ignoring case.
pub fn is_playbook_org(owner: &str) -> bool {
    ORGS.iter().any(|o| o.eq_ignore_ascii_case(owner))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_names_are_accepted_and_the_new_one_comes_first() {
        assert_eq!(ORGS[0], ORG);
        assert!(is_playbook_org("amplified-engineer"));
        assert!(is_playbook_org("Pragmatic-Engineer"));
        assert!(!is_playbook_org("acme"));
    }

    #[test]
    fn the_repo_constants_follow_the_org_constants() {
        assert_eq!(PLAYBOOK_REPO, format!("{ORG}/playbook"));
        assert_eq!(LEGACY_PLAYBOOK_REPO, format!("{LEGACY_ORG}/playbook"));
        assert_eq!(MARKETPLACE_REPO, format!("{ORG}/marketplace"));
        assert_eq!(BREW_FORMULA, format!("{ORG}/tap/playbook"));
        assert_eq!(BREW_PBTOP_FORMULA, format!("{ORG}/tap/pbtop"));
    }
}
