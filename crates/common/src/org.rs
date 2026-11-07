// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The GitHub organization names playbook trusts.
//!
//! The organization was renamed twice: `pragmatic-engineer`, then
//! `amplified-engineer`, then `amplifiedengineer`. GitHub redirects old names,
//! but nothing here may depend on that, and a binary, plugin or state folder
//! made before a rename still carries an old name. Code that builds a URL uses
//! the current name. Code that checks identity (a download, an attestation, an
//! installed plugin key, a cache folder, a state folder) accepts every name and
//! tries the current one first.

/// The one place the current organization name is written. A macro, because
/// `concat!` takes only literals, and every repo constant below derives from it.
/// A later rename changes this line, adds the old name to `LEGACY_ORGS` and
/// updates the docs.
macro_rules! current_org {
    () => {
        "amplifiedengineer"
    };
}

/// The current organization.
pub const ORG: &str = current_org!();
/// Organizations before the latest rename, newest first. Old installs, old
/// releases and old state folders use them.
pub const LEGACY_ORGS: [&str; 2] = ["amplified-engineer", "pragmatic-engineer"];
/// Every organization name to accept, current name first.
pub const ORGS: [&str; 3] = [ORG, LEGACY_ORGS[0], LEGACY_ORGS[1]];

/// The playbook repository under the current organization.
pub const PLAYBOOK_REPO: &str = concat!(current_org!(), "/playbook");
/// Every playbook repository to accept, current name first.
pub const PLAYBOOK_REPOS: [&str; 3] = [
    PLAYBOOK_REPO,
    "amplified-engineer/playbook",
    "pragmatic-engineer/playbook",
];

/// The marketplace repository, for `claude plugin marketplace add`.
pub const MARKETPLACE_REPO: &str = concat!(current_org!(), "/marketplace");
/// The fully qualified Homebrew formula, for `brew install`.
pub const BREW_FORMULA: &str = concat!(current_org!(), "/tap/playbook");
/// The fully qualified Homebrew formula for the pbtop viewer.
pub const BREW_PBTOP_FORMULA: &str = concat!(current_org!(), "/tap/pbtop");

/// Whether `owner` is the current or an old organization name, ignoring case.
pub fn is_playbook_org(owner: &str) -> bool {
    ORGS.iter().any(|o| o.eq_ignore_ascii_case(owner))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_is_accepted_and_the_current_one_comes_first() {
        assert_eq!(ORGS[0], ORG);
        assert!(is_playbook_org("amplifiedengineer"));
        assert!(is_playbook_org("Amplified-Engineer"));
        assert!(is_playbook_org("pragmatic-engineer"));
        assert!(!is_playbook_org("acme"));
    }

    #[test]
    fn the_repo_constants_follow_the_org_constants() {
        for (i, org) in ORGS.iter().enumerate() {
            assert_eq!(PLAYBOOK_REPOS[i], format!("{org}/playbook"));
        }
        assert_eq!(PLAYBOOK_REPO, format!("{ORG}/playbook"));
        assert_eq!(MARKETPLACE_REPO, format!("{ORG}/marketplace"));
        assert_eq!(BREW_FORMULA, format!("{ORG}/tap/playbook"));
        assert_eq!(BREW_PBTOP_FORMULA, format!("{ORG}/tap/pbtop"));
    }
}
