// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The one pure function that decides a session state from its stored row
//! and its pull request. First matching rule wins (ADR-0024).

use super::{Pr, PrKind, Settings, State, DAY, WORKING_SECONDS};

/// The columns of a `sessions` row that decide a state. Times are Unix seconds.
#[allow(dead_code)] // Filled from store rows once the reader loads them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Row {
    pub started_at: Option<i64>,
    pub last_prompt_at: Option<i64>,
    pub last_stop_at: Option<i64>,
    /// Set by a waiting notification, cleared by the next prompt.
    pub waiting_after_prompt: bool,
    pub ended_at: Option<i64>,
    pub crashed_at: Option<i64>,
    pub closed_at: Option<i64>,
}

impl Row {
    /// The largest of the three timestamps.
    fn last_event(&self) -> Option<i64> {
        [self.started_at, self.last_prompt_at, self.last_stop_at]
            .into_iter()
            .flatten()
            .max()
    }
}

/// The state of a session row at `now`.
pub(crate) fn derive(row: &Row, pr: Option<Pr>, now: i64, settings: Settings) -> State {
    // A last event after `now` (clock skew) counts as age 0.
    let age = row.last_event().map_or(i64::MAX, |t| (now - t).max(0));
    let stale = age >= settings.stale_after_days * DAY;
    let landed = matches!(
        pr,
        Some(Pr {
            kind: PrKind::Merged | PrKind::Closed,
            ..
        })
    );
    if row.closed_at.is_some() {
        State::Closed
    } else if landed {
        State::Done
    } else if row.crashed_at.is_some() && row.ended_at.is_none() {
        if stale {
            State::Stale
        } else {
            State::Crashed
        }
    } else if row.ended_at.is_some() {
        if stale {
            State::Stale
        } else {
            State::Idle
        }
    } else if row.waiting_after_prompt && !stale {
        State::Waiting
    } else if age < WORKING_SECONDS {
        State::Working
    } else if stale {
        State::Stale
    } else {
        State::Idle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000;

    #[derive(Default)]
    struct Case {
        ago: i64,
        waiting: bool,
        ended: bool,
        crashed: bool,
        closed: bool,
        pr: Option<PrKind>,
        no_timestamps: bool,
    }

    fn run(c: &Case) -> State {
        let row = Row {
            started_at: (!c.no_timestamps).then_some(NOW - c.ago),
            waiting_after_prompt: c.waiting,
            ended_at: c.ended.then_some(NOW - 1),
            crashed_at: c.crashed.then_some(NOW - 1),
            closed_at: c.closed.then_some(NOW - 1),
            ..Row::default()
        };
        let pr = c.pr.map(|kind| Pr { number: 1, kind });
        derive(&row, pr, NOW, Settings::default())
    }

    #[test]
    fn a_waiting_flag_after_the_last_prompt_is_waiting() {
        // Arrange
        let case = Case {
            ago: 5,
            waiting: true,
            ..Case::default()
        };
        // Act
        let state = run(&case);
        // Assert
        assert_eq!(state, State::Waiting);
    }

    #[test]
    fn derive_follows_the_state_rules() {
        let merged = Some(PrKind::Merged);
        let rows = [
            (
                "flag wins over working",
                Case {
                    ago: 5,
                    waiting: true,
                    ..Case::default()
                },
                State::Waiting,
            ),
            (
                "flag but ended",
                Case {
                    ago: 5,
                    waiting: true,
                    ended: true,
                    ..Case::default()
                },
                State::Idle,
            ),
            (
                "flag but 30 days old",
                Case {
                    ago: 30 * DAY,
                    waiting: true,
                    ..Case::default()
                },
                State::Stale,
            ),
            (
                "flag and merged",
                Case {
                    ago: 5,
                    waiting: true,
                    pr: merged,
                    ..Case::default()
                },
                State::Done,
            ),
            (
                "flag and crashed",
                Case {
                    ago: 5,
                    waiting: true,
                    crashed: true,
                    ..Case::default()
                },
                State::Crashed,
            ),
            (
                "119s is working",
                Case {
                    ago: 119,
                    ..Case::default()
                },
                State::Working,
            ),
            (
                "120s is idle",
                Case {
                    ago: 120,
                    ..Case::default()
                },
                State::Idle,
            ),
            (
                "just under 3 days is idle",
                Case {
                    ago: 3 * DAY - 1,
                    ..Case::default()
                },
                State::Idle,
            ),
            (
                "exactly 3 days is stale",
                Case {
                    ago: 3 * DAY,
                    ..Case::default()
                },
                State::Stale,
            ),
            (
                "flag just under 3 days",
                Case {
                    ago: 3 * DAY - 1,
                    waiting: true,
                    ..Case::default()
                },
                State::Waiting,
            ),
            (
                "flag exactly 3 days",
                Case {
                    ago: 3 * DAY,
                    waiting: true,
                    ..Case::default()
                },
                State::Stale,
            ),
            (
                "merged, not closed",
                Case {
                    ago: 5,
                    pr: merged,
                    ..Case::default()
                },
                State::Done,
            ),
            (
                "closed unmerged pr",
                Case {
                    ago: 5,
                    pr: Some(PrKind::Closed),
                    ..Case::default()
                },
                State::Done,
            ),
            (
                "open pr changes nothing",
                Case {
                    ago: 5,
                    pr: Some(PrKind::Open),
                    ..Case::default()
                },
                State::Working,
            ),
            (
                "merged and closed_at",
                Case {
                    ago: 5,
                    pr: merged,
                    closed: true,
                    ..Case::default()
                },
                State::Closed,
            ),
            (
                "no pr and closed_at",
                Case {
                    ago: 5,
                    closed: true,
                    ..Case::default()
                },
                State::Closed,
            ),
            (
                "a row with no timestamp at all is stale",
                Case {
                    no_timestamps: true,
                    ..Case::default()
                },
                State::Stale,
            ),
            (
                "closed beats crashed",
                Case {
                    ago: 5,
                    closed: true,
                    crashed: true,
                    ..Case::default()
                },
                State::Closed,
            ),
            (
                "crashed 4 days ago",
                Case {
                    ago: 4 * DAY,
                    crashed: true,
                    ..Case::default()
                },
                State::Stale,
            ),
            (
                "crashed recently",
                Case {
                    ago: 10,
                    crashed: true,
                    ..Case::default()
                },
                State::Crashed,
            ),
            (
                "crashed and ended",
                Case {
                    ago: 10,
                    crashed: true,
                    ended: true,
                    ..Case::default()
                },
                State::Idle,
            ),
            (
                "crashed and merged",
                Case {
                    ago: 10,
                    crashed: true,
                    pr: merged,
                    ..Case::default()
                },
                State::Done,
            ),
            (
                "open 4 days ago",
                Case {
                    ago: 4 * DAY,
                    ..Case::default()
                },
                State::Stale,
            ),
            (
                "ended 4 days ago",
                Case {
                    ago: 4 * DAY,
                    ended: true,
                    ..Case::default()
                },
                State::Stale,
            ),
            (
                "ended recently is never working",
                Case {
                    ago: 5,
                    ended: true,
                    ..Case::default()
                },
                State::Idle,
            ),
        ];
        for (name, case, want) in rows {
            // Act
            let got = run(&case);
            // Assert
            assert_eq!(got, want, "{name}");
        }
    }

    #[test]
    fn the_last_event_is_the_largest_of_the_three_timestamps() {
        // Arrange
        let row = Row {
            started_at: Some(NOW - 10 * DAY),
            last_prompt_at: Some(NOW - 5 * DAY),
            last_stop_at: Some(NOW - 30),
            ..Row::default()
        };
        // Act
        let state = derive(&row, None, NOW, Settings::default());
        // Assert
        assert_eq!(state, State::Working);
    }

    #[test]
    fn a_last_event_in_the_future_counts_as_age_zero() {
        // Arrange
        let row = Row {
            last_prompt_at: Some(NOW + 60),
            ..Row::default()
        };
        // Act
        let state = derive(&row, None, NOW, Settings::default());
        // Assert
        assert_eq!(state, State::Working);
    }
}
