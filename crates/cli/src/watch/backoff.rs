// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Retry delays: 30 seconds doubled per attempt, capped at an hour, with full
//! jitter so many items that failed together do not retry together.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

pub const BASE: i64 = 30;
pub const CAP: i64 = 3600;
/// The item goes to the dead list after this many failed tries.
pub const MAX_ATTEMPTS: u32 = 5;

/// Picks a number from 0 to `ceiling`, both included.
pub trait Jitter {
    fn pick(&self, ceiling: i64) -> i64;
}

/// Jitter from the standard library's randomly keyed hasher, so no crate is
/// needed.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdJitter;

impl Jitter for StdJitter {
    fn pick(&self, ceiling: i64) -> i64 {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u8(0);
        let span = u64::try_from(ceiling).unwrap_or(0) + 1;
        i64::try_from(hasher.finish() % span).unwrap_or(0)
    }
}

/// The longest wait after `attempt` failed tries, counting from 0.
pub fn ceiling(attempt: u32) -> i64 {
    let doubled = 1_i64.checked_shl(attempt).unwrap_or(i64::MAX);
    BASE.saturating_mul(doubled).min(CAP)
}

/// How many seconds to wait after `attempt` failed tries.
pub fn delay(attempt: u32, jitter: &dyn Jitter) -> i64 {
    jitter.pick(ceiling(attempt))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Max;
    impl Jitter for Max {
        fn pick(&self, ceiling: i64) -> i64 {
            ceiling
        }
    }

    struct Min;
    impl Jitter for Min {
        fn pick(&self, _: i64) -> i64 {
            0
        }
    }

    #[test]
    fn the_ceiling_doubles_from_30_seconds_and_stops_at_an_hour() {
        let got: Vec<i64> = (0..9).map(ceiling).collect();
        assert_eq!(got, [30, 60, 120, 240, 480, 960, 1920, 3600, 3600]);
        assert_eq!(ceiling(200), CAP);
    }

    #[test]
    fn the_longest_jitter_waits_the_whole_ceiling() {
        for attempt in 0..12 {
            assert_eq!(delay(attempt, &Max), ceiling(attempt));
            assert!(delay(attempt, &Max) > 0);
        }
        assert_eq!(delay(7, &Max), 3600);
    }

    #[test]
    fn the_shortest_jitter_retries_at_once() {
        assert_eq!(delay(4, &Min), 0);
    }

    #[test]
    fn real_jitter_stays_inside_the_ceiling() {
        for attempt in 0..8 {
            for _ in 0..1000 {
                let waited = delay(attempt, &StdJitter);
                assert!((0..=ceiling(attempt)).contains(&waited));
            }
        }
    }
}
