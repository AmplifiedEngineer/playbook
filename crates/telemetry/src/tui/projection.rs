// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The projection of each limit, which the Budget strip shows: one plain line
//! saying when it will be reached at the current pace, or that it will not be
//! reached in this period. A spending budget shows two paces side by side, the last 24
//! hours and the average of the period, so a recent spike and a steady month
//! can be told apart. A usage window (5 hour, weekly) only has its average,
//! because Claude Code reports a percentage and not a history.

use super::fmt::money;
use crate::limits::{Limit, Unit, MIN_PROJECTION_SECONDS};
use crate::period::elapsed_text;

const DAY: f64 = 86_400.0;

/// The trailing window the recent pace is measured over.
pub const RECENT_SECONDS: i64 = 24 * 3600;

/// What a pace means for a limit.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Outcome {
    /// The limit is already reached.
    Reached,
    /// Nothing was used in the span, so nothing can be projected.
    Idle,
    /// The limit is reached at this UTC second, before the period ends.
    At(i64),
    /// The period ends first.
    Safe,
}

fn outcome(remaining: f64, pace_per_day: f64, now: i64, end: i64) -> Outcome {
    if remaining <= 0.0 {
        return Outcome::Reached;
    }
    if !(pace_per_day.is_finite() && pace_per_day > 0.0) {
        return Outcome::Idle;
    }
    // Compare in seconds as a float first, so a tiny pace cannot overflow.
    let wait = (remaining / pace_per_day * DAY).ceil();
    if wait < (end - now) as f64 {
        Outcome::At(now + wait as i64)
    } else {
        Outcome::Safe
    }
}

/// The limit's name with a capital letter, as the line starts with it.
fn heading(l: &Limit) -> String {
    let n = super::budget::name(l);
    let mut chars = n.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// One clause: the pace, and what it leads to.
fn clause(label: &str, pace: Option<f64>, unit: Unit, o: Outcome) -> String {
    let rate = |n: f64| match unit {
        Unit::Dollars => format!("{}/day", money(n)),
        Unit::Percent => format!("{n:.1}%/day"),
    };
    if o == Outcome::Reached {
        return format!("{label}: limit reached");
    }
    let Some(pace) = pace else {
        return format!("{label}: no pace yet");
    };
    let pace_text = rate(pace);
    match o {
        Outcome::Reached | Outcome::Idle => format!("{label} {pace_text}: no hit"),
        Outcome::At(at) => format!("{label} {pace_text}: hit {}", super::budget::stamp(at)),
        Outcome::Safe => format!("{label} {pace_text}: won't hit before it resets"),
    }
}

/// One pace a limit is measured at and what it leads to.
struct Pace {
    /// The label of the long clause: `last 24h`, `period average`.
    label: &'static str,
    /// The label of the short clause on the budget strip.
    short: &'static str,
    pace: Option<f64>,
    outcome: Outcome,
}

/// The paces of a limit: the last 24 hours for a dollar budget, and the
/// average of the period or window. Empty when the limit has no headroom yet.
fn paces(l: &Limit, recent_cost: f64, now: i64) -> Vec<Pace> {
    let Some(h) = l.headroom(now) else {
        return Vec::new();
    };
    let remaining = h.remaining;
    // Like the Budget panel, wait for an hour of the period before projecting.
    let avg = h
        .avg_per_day
        .filter(|_| h.elapsed_seconds >= MIN_PROJECTION_SECONDS);
    let (label, short) = match l.unit {
        Unit::Dollars => ("period average", "average pace"),
        Unit::Percent => ("window average", "average pace"),
    };
    let average = Pace {
        label,
        short,
        pace: avg,
        outcome: outcome(remaining, avg.unwrap_or(0.0), now, l.end),
    };
    if l.unit == Unit::Percent {
        return vec![average];
    }
    // The recent pace covers the last 24 hours. A period younger than that
    // uses its own spend over its own age, so spend from before the period
    // began never counts. Under an hour there is too little to say.
    let age = now - l.start;
    let (spent, span) = if age < RECENT_SECONDS {
        (l.used, age)
    } else {
        (recent_cost, RECENT_SECONDS)
    };
    let recent = (span >= MIN_PROJECTION_SECONDS).then(|| spent / (span as f64 / DAY));
    let recent = Pace {
        label: "last 24h",
        short: "24h pace",
        pace: recent,
        outcome: outcome(remaining, recent.unwrap_or(0.0), now, l.end),
    };
    vec![recent, average]
}

/// The plain line for one limit. `recent_cost` is the spend of the last
/// 24 hours, all projects, used for dollar budgets that are older than a day.
pub fn line(l: &Limit, recent_cost: f64, now: i64) -> String {
    let used = match l.unit {
        Unit::Dollars => format!("{} of {}", money(l.used), money(l.limit)),
        Unit::Percent => format!("{:.0}% used", l.used),
    };
    let head = format!("{}, {used}.", heading(l));
    let clauses: Vec<String> = paces(l, recent_cost, now)
        .iter()
        .map(|p| clause(p.label, p.pace, l.unit, p.outcome))
        .collect();
    if clauses.is_empty() {
        return head;
    }
    format!("{head} {}.", clauses.join(". "))
}

/// Every limit's line, worst first as the limits are sorted.
pub fn lines(limits: &[Limit], recent_cost: f64, now: i64) -> Vec<String> {
    limits.iter().map(|l| line(l, recent_cost, now)).collect()
}

/// The short clause the budget strip shows beside a limit's meter: when the
/// limit is reached, by the sooner of the paces, or that the period or window
/// ends first. `None` when the limit is reached already or no pace is known.
pub fn reach(l: &Limit, recent_cost: f64, now: i64) -> Option<String> {
    let paces = paces(l, recent_cost, now);
    let sooner = paces
        .iter()
        .filter_map(|p| match p.outcome {
            Outcome::At(at) => Some((at, p.short)),
            _ => None,
        })
        .min_by_key(|(at, _)| *at);
    if let Some((at, short)) = sooner {
        return Some(format!(
            "reaches the limit in {} at the {short}",
            elapsed_text(at - now)
        ));
    }
    paces
        .iter()
        .any(|p| p.outcome == Outcome::Safe)
        .then(|| "will not reach the limit before it resets".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::{LimitPeriod, Origin};

    const NOW: i64 = 1_788_400_000;
    const DAYS: i64 = 86_400;

    fn budget(used: f64, limit: f64, start: i64, end: i64) -> Limit {
        Limit {
            unit: Unit::Dollars,
            period: LimitPeriod::Monthly,
            origin: Origin::Manual,
            used,
            limit,
            start,
            end,
        }
    }

    #[test]
    fn a_fast_recent_pace_hits_the_limit_while_the_slow_average_stays_under() {
        crate::tz::set(crate::tz::Zone::Utc);
        let l = budget(300.0, 1000.0, NOW - 15 * DAYS, NOW + 15 * DAYS);

        let text = line(&l, 100.0, NOW);

        assert!(
            text.contains("last 24h $100.00/day: hit 09-10 01:46"),
            "{text}"
        );
        assert!(
            text.contains("period average $20.00/day: won't hit before it resets"),
            "{text}"
        );
    }

    #[test]
    fn a_limit_already_used_up_says_it_is_reached() {
        let l = budget(1000.0, 1000.0, NOW - 15 * DAYS, NOW + 15 * DAYS);

        let text = line(&l, 10.0, NOW);

        assert!(text.contains("last 24h: limit reached"), "{text}");
        assert!(text.contains("period average: limit reached"), "{text}");
    }

    #[test]
    fn no_spend_in_the_last_day_is_idle_and_not_a_hit() {
        let l = budget(100.0, 1000.0, NOW - 15 * DAYS, NOW + 15 * DAYS);

        let text = line(&l, 0.0, NOW);

        assert!(text.contains("last 24h $0.00/day: no hit"), "{text}");
    }

    #[test]
    fn a_period_younger_than_an_hour_has_no_recent_pace_yet() {
        let l = budget(5.0, 1000.0, NOW - 600, NOW + 30 * DAYS);

        let text = line(&l, 5.0, NOW);

        assert!(text.contains("last 24h: no pace yet"), "{text}");
    }

    #[test]
    fn a_period_younger_than_a_day_uses_its_own_spend_and_age_for_the_recent_pace() {
        let l = budget(6.0, 1000.0, NOW - 12 * 3600, NOW + 30 * DAYS);

        let text = line(&l, 12.0, NOW);

        assert!(text.contains("last 24h $12.00/day"), "{text}");
    }

    #[test]
    fn a_period_younger_than_an_hour_has_no_average_pace_yet() {
        let l = budget(2.0, 300.0, NOW - 600, NOW + 30 * DAYS);

        let text = line(&l, 2.0, NOW);

        assert!(text.contains("period average: no pace yet"), "{text}");
    }

    #[test]
    fn a_tiny_pace_does_not_overflow_the_hit_time() {
        let l = budget(1e-300, 1000.0, NOW - 15 * DAYS, NOW + 15 * DAYS);

        let text = line(&l, 0.0, NOW);

        assert!(text.contains("won't hit before it resets"), "{text}");
    }

    #[test]
    fn a_young_period_that_used_up_its_limit_says_so() {
        let l = budget(1000.0, 1000.0, NOW - 600, NOW + 30 * DAYS);

        let text = line(&l, 1000.0, NOW);

        assert!(text.contains("last 24h: limit reached"), "{text}");
    }

    #[test]
    fn a_usage_window_shows_only_its_average_in_percent() {
        crate::tz::set(crate::tz::Zone::Utc);
        let l = Limit {
            unit: Unit::Percent,
            period: LimitPeriod::FiveHour,
            origin: Origin::Measured,
            used: 50.0,
            limit: 100.0,
            start: NOW - 3 * 3600,
            end: NOW + 2 * 3600,
        };

        let text = line(&l, 0.0, NOW);

        assert!(text.starts_with("5 hour window, 50% used."), "{text}");
        assert!(text.contains("window average"), "{text}");
        assert!(!text.contains("last 24h"), "{text}");
    }

    #[test]
    fn no_limits_give_no_lines() {
        assert!(lines(&[], 0.0, NOW).is_empty());
    }

    #[test]
    fn the_strip_clause_names_the_sooner_pace_and_the_time_to_the_limit() {
        // Arrange
        let l = budget(300.0, 1000.0, NOW - 15 * DAYS, NOW + 15 * DAYS);

        // Act
        let text = reach(&l, 100.0, NOW);

        // Assert
        assert_eq!(
            text.as_deref(),
            Some("reaches the limit in 7d 0h at the 24h pace")
        );
    }

    #[test]
    fn the_strip_clause_says_the_limit_holds_when_both_paces_stay_under() {
        let l = budget(300.0, 1000.0, NOW - 15 * DAYS, NOW + 15 * DAYS);

        let text = reach(&l, 10.0, NOW);

        assert_eq!(
            text.as_deref(),
            Some("will not reach the limit before it resets")
        );
    }

    #[test]
    fn the_strip_clause_is_absent_for_a_reached_limit_and_for_no_pace() {
        let spent = budget(1000.0, 1000.0, NOW - 15 * DAYS, NOW + 15 * DAYS);
        let young = budget(5.0, 1000.0, NOW - 600, NOW + 30 * DAYS);

        assert_eq!(reach(&spent, 10.0, NOW), None);
        assert_eq!(reach(&young, 5.0, NOW), None);
    }

    #[test]
    fn a_usage_window_has_only_the_average_pace_in_its_clause() {
        let l = Limit {
            unit: Unit::Percent,
            period: LimitPeriod::FiveHour,
            origin: Origin::Measured,
            used: 50.0,
            limit: 100.0,
            start: NOW - 2 * 3600,
            end: NOW + 3 * 3600,
        };

        let text = reach(&l, 0.0, NOW);

        assert_eq!(
            text.as_deref(),
            Some("reaches the limit in 2h 0m at the average pace")
        );
    }
}
