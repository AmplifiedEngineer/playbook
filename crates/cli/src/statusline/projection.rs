// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Where each Pro and Max window lands at its reset if use goes on at the
//! current pace, for line 3. The pace maths is [`Limit::headroom`], the same
//! one the usage views and line 4 use, so the numbers never disagree.

use super::fmt::{GREEN, RED, YELLOW};
use crate::usage::limits::{Limit, LimitPeriod, State, Unit};
use crate::usage::limits_source::Snapshot;

/// The percent of a window that is its cap.
const CAP_PCT: f64 = 100.0;

/// A projection at or above this percent is shown as near the cap.
const NEAR_CAP_PCT: f64 = 80.0;

/// The largest projection written out. A young 7 day window can project to
/// thousands of percent, which says no more than this and takes more room.
const MAX_SHOWN_PCT: f64 = 999.0;

/// One window's projection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Projection {
    /// Percent of the window used by its reset at the current pace.
    pub at_reset: f64,
    /// When the window resets, UTC seconds.
    pub resets_at: i64,
}

/// The projections line 3 can show. Each is `None` when the data has no such
/// window, the window is too young to have a pace, or it is already reached.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct Projections {
    pub five_hour: Option<Projection>,
    pub seven_day: Option<Projection>,
}

/// The projections for the statusline reading at `now`.
pub(super) fn build(snapshot: Option<&Snapshot>, now: i64) -> Projections {
    let mut out = Projections::default();
    for limit in snapshot.map(|s| s.limits(now)).unwrap_or_default() {
        let projection = of(&limit, now);
        match limit.period {
            LimitPeriod::FiveHour => out.five_hour = projection,
            LimitPeriod::SevenDay => out.seven_day = projection,
            _ => {}
        }
    }
    out
}

fn of(limit: &Limit, now: i64) -> Option<Projection> {
    if limit.unit != Unit::Percent {
        return None;
    }
    let headroom = limit.headroom(now)?;
    // A reached window stays blocked until it resets, so a figure past the
    // cap would say nothing the percent does not.
    if headroom.state == State::Exceeded {
        return None;
    }
    Some(Projection {
        at_reset: headroom.projected?,
        resets_at: limit.end,
    })
}

impl Projection {
    /// `75%`, or `>999%` past [`MAX_SHOWN_PCT`].
    pub(super) fn text(self) -> String {
        if self.at_reset > MAX_SHOWN_PCT {
            format!(">{MAX_SHOWN_PCT:.0}%")
        } else {
            format!("{:.0}%", self.at_reset)
        }
    }

    /// Red past the cap, yellow near it, green below.
    pub(super) fn color(self) -> &'static str {
        if self.at_reset > CAP_PCT {
            RED
        } else if self.at_reset >= NEAR_CAP_PCT {
            YELLOW
        } else {
            GREEN
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3600;
    const FIVE_HOURS: i64 = 5 * HOUR;
    const NOW: i64 = 1_000_000;

    fn snapshot(five_hour: &str) -> Option<Snapshot> {
        Snapshot::from_statusline(&format!(r#"{{"rate_limits":{{{five_hour}}}}}"#), NOW)
    }

    fn five_hour(used: f64, elapsed: i64) -> String {
        let resets_at = NOW + FIVE_HOURS - elapsed;
        format!(r#""five_hour":{{"used_percentage":{used},"resets_at":{resets_at}}}"#)
    }

    #[test]
    fn a_window_with_a_pace_projects_to_its_reset() {
        let p = build(snapshot(&five_hour(30.0, 2 * HOUR)).as_ref(), NOW);
        let five = p.five_hour.unwrap();
        assert!((five.at_reset - 75.0).abs() < 1e-9, "{five:?}");
        assert_eq!(five.resets_at, NOW + 3 * HOUR);
        assert_eq!(p.seven_day, None);
    }

    #[test]
    fn no_reading_a_young_window_or_a_reached_one_projects_nothing() {
        assert_eq!(build(None, NOW), Projections::default());
        let young = build(snapshot(&five_hour(30.0, HOUR - 1)).as_ref(), NOW);
        assert_eq!(young.five_hour, None);
        let reached = build(snapshot(&five_hour(100.0, 2 * HOUR)).as_ref(), NOW);
        assert_eq!(reached.five_hour, None);
    }

    #[test]
    fn the_text_is_capped_and_the_colour_warns_past_the_cap() {
        let at = |pct: f64| Projection {
            at_reset: pct,
            resets_at: 0,
        };
        assert_eq!(at(74.6).text(), "75%");
        assert_eq!(at(999.0).text(), "999%");
        assert_eq!(at(1000.0).text(), ">999%");
        assert_eq!(at(79.9).color(), GREEN);
        assert_eq!(at(80.0).color(), YELLOW);
        assert_eq!(at(100.0).color(), YELLOW);
        assert_eq!(at(100.1).color(), RED);
    }
}
