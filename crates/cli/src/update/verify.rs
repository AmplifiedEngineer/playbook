// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Integrity and provenance: SHA256SUMS, then the build attestation with the
//! same policy as install.sh (lenient by default, strict with
//! `PLAYBOOK_REQUIRE_ATTESTATION=1`).

use crate::common::hash::sha256_hex;
use crate::common::proc::run_with_timeout;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Longest the `gh attestation --help` probe may run.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest `gh attestation verify` may run; it makes network calls.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(60);

/// The repository new downloads and API calls use.
pub const REPO: &str = crate::common::org::PLAYBOOK_REPO;

/// Checks `bytes` against the `<hash>  <asset>` line of a SHA256SUMS file.
pub fn verify_checksum(bytes: &[u8], sums: &str, asset: &str) -> Result<(), String> {
    let expected = sums
        .lines()
        .filter_map(|line| line.split_once("  "))
        .find(|(_, name)| name.trim() == asset)
        .map(|(hash, _)| hash.trim().to_ascii_lowercase())
        .ok_or_else(|| {
            format!("no checksum line for {asset} in SHA256SUMS; the release may be incomplete")
        })?;
    let actual = sha256_hex(bytes);
    if actual == expected {
        Ok(())
    } else {
        Err(format!("checksum mismatch for {asset}: expected {expected}, got {actual}; nothing was installed"))
    }
}

/// Provenance check seam. `Ok(None)` is verified, `Ok(Some(w))` is a lenient
/// pass with a warning to show, `Err` aborts the update.
pub trait Attest {
    fn verify(&self, file: &Path, strict: bool) -> Result<Option<String>, String>;
}

/// Shells out to `gh attestation verify`, pinned to release.yml.
pub struct GhAttest;

/// Failure texts that mean "could not check", not "checked and wrong".
fn is_gap(err: &str) -> bool {
    let err = err.to_ascii_lowercase();
    [
        "no attestations found",
        "failed to fetch",
        "gh auth login",
        "gh_token",
        "http 401",
    ]
    .iter()
    .any(|needle| err.contains(needle))
}

impl Attest for GhAttest {
    fn verify(&self, file: &Path, strict: bool) -> Result<Option<String>, String> {
        let mut probe = Command::new("gh");
        probe.args(["attestation", "--help"]);
        let has_gh =
            run_with_timeout(&mut probe, PROBE_TIMEOUT).is_some_and(|o| o.status.success());
        if !has_gh {
            return if strict {
                Err("PLAYBOOK_REQUIRE_ATTESTATION=1 but gh with 'attestation' support is not available".into())
            } else {
                Ok(Some("checksum only, provenance not verified: install gh to check the build attestation".into()))
            };
        }
        // A release built before the rename carries the old organization in its
        // signer identity, so try the new name first and fall back to the old.
        let mut gaps: Vec<String> = Vec::new();
        let mut failure: Option<String> = None;
        for repo in crate::common::org::PLAYBOOK_REPOS {
            let Some(output) = run_with_timeout(&mut verify_command(file, repo), VERIFY_TIMEOUT)
            else {
                return if strict {
                    Err("gh attestation verify did not finish in time".into())
                } else {
                    Ok(Some(
                        "checksum only, provenance not verified: gh attestation verify did not finish in time".into(),
                    ))
                };
            };
            if output.status.success() {
                return Ok(None);
            }
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stderr),
                String::from_utf8_lossy(&output.stdout)
            );
            if is_gap(&text) {
                gaps.push(text);
            } else if failure.is_none() {
                failure = Some(text);
            }
        }
        // Lenient only when neither name produced a real mismatch.
        if let (false, None, Some(text)) = (strict, &failure, gaps.first()) {
            let first = text.lines().next().unwrap_or("").trim();
            return Ok(Some(format!(
                "checksum only, provenance not verified: {first}"
            )));
        }
        let text = failure
            .or_else(|| gaps.into_iter().next())
            .unwrap_or_default();
        Err(format!("attestation verification failed: {}", text.trim()))
    }
}

/// `gh attestation verify` for `file`, pinned to `repo`'s release workflow.
fn verify_command(file: &Path, repo: &str) -> Command {
    let mut verify = Command::new("gh");
    verify
        .args(["attestation", "verify"])
        .arg(file)
        .args(["--repo", repo, "--signer-workflow"])
        .arg(format!("{repo}/.github/workflows/release.yml"));
    verify
}

#[cfg(test)]
pub mod fake {
    use super::Attest;
    use std::path::Path;

    use std::cell::RefCell;

    /// An attestation result fixed by the test, recording each `strict` it saw.
    pub struct FakeAttest(pub Result<Option<String>, String>, pub RefCell<Vec<bool>>);

    impl FakeAttest {
        pub fn new(result: Result<Option<String>, String>) -> Self {
            FakeAttest(result, RefCell::new(Vec::new()))
        }
    }

    impl Attest for FakeAttest {
        fn verify(&self, _: &Path, strict: bool) -> Result<Option<String>, String> {
            self.1.borrow_mut().push(strict);
            self.0.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_matching_checksum_passes_and_a_mismatch_aborts() {
        let sums = format!(
            "{}  playbook-1-x\n{}  other\n",
            sha256_hex(b"good"),
            sha256_hex(b"zzz")
        );
        assert!(verify_checksum(b"good", &sums, "playbook-1-x").is_ok());
        let err = verify_checksum(b"evil", &sums, "playbook-1-x").unwrap_err();
        assert!(err.contains("checksum mismatch"), "{err}");
    }

    #[test]
    fn a_missing_checksum_line_aborts() {
        assert!(verify_checksum(b"x", "abc  other\n", "playbook-1-x").is_err());
    }

    #[test]
    fn the_verify_command_pins_the_repo_it_is_given() {
        let args = |repo: &str| -> Vec<String> {
            verify_command(Path::new("f"), repo)
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        let new = args(crate::common::org::PLAYBOOK_REPO);
        assert!(new.contains(&"amplified-engineer/playbook".to_string()));
        assert!(
            new.contains(&"amplified-engineer/playbook/.github/workflows/release.yml".to_string())
        );
        let old = args(crate::common::org::LEGACY_PLAYBOOK_REPO);
        assert!(
            old.contains(&"pragmatic-engineer/playbook/.github/workflows/release.yml".to_string())
        );
    }

    #[test]
    fn the_new_repo_is_tried_before_the_old_one() {
        assert_eq!(
            crate::common::org::PLAYBOOK_REPOS,
            [REPO, crate::common::org::LEGACY_PLAYBOOK_REPO]
        );
    }

    #[test]
    fn gap_detection_matches_the_installer_patterns() {
        assert!(is_gap("Error: no attestations found for subject"));
        assert!(!is_gap("verification failed: signer mismatch"));
    }
}
