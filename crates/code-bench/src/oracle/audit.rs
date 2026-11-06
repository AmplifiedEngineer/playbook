// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The hand-labeled audit sample. Each case records whether the oracle's
//! answer matched the hand label. The error rate is mismatches divided by
//! cases, in basis points, and `None` for an empty sample.

use crate::metrics::{BasisPoints, BASIS_POINTS_PER_UNIT};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditCase {
    pub repo: String,
    pub name: String,
    /// Why the case is in the sample, such as a spelling-only match.
    pub note: String,
    pub oracle_matches_label: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditSample {
    /// The rate the file publishes. It must equal the recomputed rate.
    pub published_error_bps: Option<BasisPoints>,
    pub cases: Vec<AuditCase>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AuditError {
    #[error("audit sample does not parse: {0}")]
    Parse(String),
    #[error("audit sample publishes {published:?} and the cases give {recomputed:?}")]
    Stale {
        published: Option<BasisPoints>,
        recomputed: Option<BasisPoints>,
    },
}

impl AuditSample {
    /// Parses a sample and checks that its published rate is current.
    pub fn parse(text: &str) -> Result<Self, AuditError> {
        let sample: AuditSample =
            serde_json::from_str(text).map_err(|e| AuditError::Parse(e.to_string()))?;
        let recomputed = sample.error_rate();
        if sample.published_error_bps != recomputed {
            return Err(AuditError::Stale {
                published: sample.published_error_bps,
                recomputed,
            });
        }
        Ok(sample)
    }

    /// Mismatches over cases in basis points, rounded half up.
    pub fn error_rate(&self) -> Option<BasisPoints> {
        let total = self.cases.len() as u64;
        if total == 0 {
            return None;
        }
        let wrong = self
            .cases
            .iter()
            .filter(|c| !c.oracle_matches_label)
            .count() as u64;
        let bps = (2 * wrong * BASIS_POINTS_PER_UNIT + total) / (2 * total);
        u32::try_from(bps).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(cases: usize, wrong: usize, published: Option<BasisPoints>) -> String {
        let cases: Vec<AuditCase> = (0..cases)
            .map(|i| AuditCase {
                repo: "cobra".into(),
                name: format!("n{i}"),
                note: "spelling only".into(),
                oracle_matches_label: i >= wrong,
            })
            .collect();
        serde_json::to_string(&AuditSample {
            published_error_bps: published,
            cases,
        })
        .unwrap()
    }

    #[test]
    fn empty_sample_is_undefined() {
        let parsed = AuditSample::parse(&sample(0, 0, None)).unwrap();
        assert_eq!(parsed.error_rate(), None);
    }

    #[test]
    fn two_mismatches_in_forty_cases_is_500_basis_points() {
        let parsed = AuditSample::parse(&sample(40, 2, Some(500))).unwrap();
        assert_eq!(parsed.error_rate(), Some(500));
    }

    #[test]
    fn stale_published_rate_fails() {
        let err = AuditSample::parse(&sample(40, 2, Some(400))).unwrap_err();
        assert_eq!(
            err,
            AuditError::Stale {
                published: Some(400),
                recomputed: Some(500)
            }
        );
        assert!(matches!(
            AuditSample::parse("{}"),
            Err(AuditError::Parse(_))
        ));
    }
}
