// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The mutation score floor. The score and the floor are integer basis
//! points. The score comes through an injected reader, so a test fakes it.

use crate::metrics::{BasisPoints, BASIS_POINTS_PER_UNIT};
use serde::Deserialize;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MutantsError {
    #[error("the mutation outcomes do not parse: {0}")]
    Parse(String),
    #[error("no mutant was caught, missed or timed out, so there is no score")]
    NoMutants,
    #[error("the mutation score could not be read: {0}")]
    Read(String),
}

/// The counts of cargo-mutants `outcomes.json` that decide the score.
#[derive(Debug, Deserialize)]
struct Outcomes {
    caught: u64,
    missed: u64,
    timeout: u64,
}

/// Caught mutants over caught, missed and timed out ones, rounded down. An
/// unviable mutant does not compile and says nothing about the tests.
pub fn score_from_outcomes(text: &str) -> Result<BasisPoints, MutantsError> {
    let o: Outcomes = serde_json::from_str(text).map_err(|e| MutantsError::Parse(e.to_string()))?;
    let total = o.caught + o.missed + o.timeout;
    if total == 0 {
        return Err(MutantsError::NoMutants);
    }
    let score = o.caught * BASIS_POINTS_PER_UNIT / total;
    Ok(BasisPoints::try_from(score).unwrap_or(BasisPoints::MAX))
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// No floor is committed yet; the score is the first measurement.
    FirstMeasurement(BasisPoints),
    Pass {
        score: BasisPoints,
        floor: BasisPoints,
    },
    Below {
        score: BasisPoints,
        floor: BasisPoints,
    },
}

impl Verdict {
    pub fn passes(&self) -> bool {
        !matches!(self, Verdict::Below { .. })
    }
}

/// Compares the score from `read_score` with the committed floor. A score
/// equal to the floor passes and one basis point lower fails.
pub fn judge(
    read_score: impl FnOnce() -> Result<BasisPoints, MutantsError>,
    floor: Option<BasisPoints>,
) -> Result<Verdict, MutantsError> {
    let score = read_score()?;
    Ok(match floor {
        None => Verdict::FirstMeasurement(score),
        Some(floor) if score >= floor => Verdict::Pass { score, floor },
        Some(floor) => Verdict::Below { score, floor },
    })
}

/// True when the floor of the merge base is higher than the current one. A
/// floor that appears for the first time is an addition.
pub fn floor_lowered(before: Option<BasisPoints>, now: Option<BasisPoints>) -> bool {
    match (before, now) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(before), Some(now)) => now < before,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLOOR: BasisPoints = 7000;

    fn read(score: BasisPoints) -> impl FnOnce() -> Result<BasisPoints, MutantsError> {
        move || Ok(score)
    }

    #[test]
    fn a_score_exactly_at_the_floor_passes() {
        let verdict = judge(read(FLOOR), Some(FLOOR)).unwrap();
        assert!(verdict.passes());
    }

    #[test]
    fn a_score_one_basis_point_under_the_floor_fails() {
        let verdict = judge(read(FLOOR - 1), Some(FLOOR)).unwrap();
        assert_eq!(
            verdict,
            Verdict::Below {
                score: FLOOR - 1,
                floor: FLOOR
            }
        );
        assert!(!verdict.passes());
    }

    #[test]
    fn a_score_one_basis_point_over_the_floor_passes() {
        assert!(judge(read(FLOOR + 1), Some(FLOOR)).unwrap().passes());
    }

    #[test]
    fn no_floor_records_the_first_measurement() {
        assert_eq!(
            judge(read(FLOOR), None).unwrap(),
            Verdict::FirstMeasurement(FLOOR)
        );
    }

    #[test]
    fn a_reader_error_is_returned() {
        let failing = || Err(MutantsError::Read("no file".into()));
        assert_eq!(
            judge(failing, Some(FLOOR)),
            Err(MutantsError::Read("no file".into()))
        );
    }

    #[test]
    fn the_score_counts_timeouts_and_ignores_unviable_mutants() {
        let text = r#"{"caught": 6, "missed": 3, "timeout": 1, "unviable": 40}"#;
        assert_eq!(score_from_outcomes(text), Ok(6000));
    }

    #[test]
    fn the_score_rounds_down() {
        let text = r#"{"caught": 2, "missed": 1, "timeout": 0}"#;
        assert_eq!(score_from_outcomes(text), Ok(6666));
    }

    #[test]
    fn no_mutants_has_no_score() {
        let text = r#"{"caught": 0, "missed": 0, "timeout": 0}"#;
        assert_eq!(score_from_outcomes(text), Err(MutantsError::NoMutants));
    }

    #[test]
    fn text_that_is_not_outcomes_fails() {
        assert!(matches!(
            score_from_outcomes("[]"),
            Err(MutantsError::Parse(_))
        ));
    }

    #[test]
    fn a_lower_floor_is_flagged_and_a_first_floor_is_not() {
        assert!(floor_lowered(Some(FLOOR), Some(FLOOR - 1)));
        assert!(floor_lowered(Some(FLOOR), None));
        assert!(!floor_lowered(Some(FLOOR), Some(FLOOR)));
        assert!(!floor_lowered(Some(FLOOR), Some(FLOOR + 1)));
        assert!(!floor_lowered(None, Some(FLOOR)));
        assert!(!floor_lowered(None, None));
    }
}
