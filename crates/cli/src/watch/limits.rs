// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The caps on automatic actions and the trust ladder. Both are pure: the
//! engine reads the numbers from the store and asks here whether to go on.

use super::store::Approval;

/// One action per rule per object in this many seconds.
pub const OBJECT_COOLDOWN: i64 = 3600;
/// Approved proposals a class needs before it may run on its own.
pub const LADDER_APPROVALS: usize = 10;
/// Seconds the approvals must span: 28 days.
pub const LADDER_SPAN: i64 = 28 * 86_400;

/// The UTC day a time falls on. Counters are kept per day, so they reset at
/// 00:00 UTC without anything running.
pub fn day_of(now: i64) -> i64 {
    now.div_euclid(86_400)
}

/// The caps in `watch.limits.*` that cover every rule.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlobalCaps {
    pub actions_per_day: i64,
    pub usd_per_day: f64,
}

/// What has been used today.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub rule_actions: i64,
    pub rule_usd: f64,
    pub global_actions: i64,
    pub global_usd: f64,
}

/// The caps that apply to one rule.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Caps {
    pub rule_actions: i64,
    pub rule_usd: f64,
    pub global_actions: i64,
    pub global_usd: f64,
}

/// Which cap stopped an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    RuleActions,
    RuleUsd,
    GlobalActions,
    GlobalUsd,
    Object,
}

impl Limit {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RuleActions => "rule_actions",
            Self::RuleUsd => "rule_usd",
            Self::GlobalActions => "global_actions",
            Self::GlobalUsd => "global_usd",
            Self::Object => "object",
        }
    }
}

/// Whether one more action costing `cost` dollars fits today's caps and the
/// per object cap. `last_acted` is when the rule last acted on this object.
pub fn check(
    usage: &Usage,
    caps: &Caps,
    cost: f64,
    last_acted: Option<i64>,
    now: i64,
) -> Result<(), Limit> {
    if usage.rule_actions >= caps.rule_actions {
        return Err(Limit::RuleActions);
    }
    if usage.global_actions >= caps.global_actions {
        return Err(Limit::GlobalActions);
    }
    if usage.rule_usd + cost > caps.rule_usd {
        return Err(Limit::RuleUsd);
    }
    if usage.global_usd + cost > caps.global_usd {
        return Err(Limit::GlobalUsd);
    }
    if last_acted.is_some_and(|at| now - at < OBJECT_COOLDOWN) {
        return Err(Limit::Object);
    }
    Ok(())
}

/// Whether a class that has no free pass may run on its own: enough approved
/// proposals, spanning enough days, and none of them failed.
pub fn ladder_allows(history: &[Approval], now: i64) -> Result<(), String> {
    if history.len() < LADDER_APPROVALS {
        return Err(format!(
            "{} of {LADDER_APPROVALS} approved proposals",
            history.len()
        ));
    }
    let oldest = history.iter().map(|a| a.at).min().unwrap_or(now);
    if now - oldest < LADDER_SPAN {
        return Err("approvals span less than 28 days".to_string());
    }
    if history.iter().any(|a| a.failed) {
        return Err("an approved proposal failed".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAPS: Caps = Caps {
        rule_actions: 3,
        rule_usd: 2.0,
        global_actions: 5,
        global_usd: 4.0,
    };

    #[test]
    fn each_cap_allows_up_to_the_limit_and_stops_one_past_it() {
        // Arrange: (the usage field to set, its cap, the stop it causes)
        type Set = fn(&mut Usage, i64);
        let rows: [(Set, i64, Limit); 2] = [
            (|u, n| u.rule_actions = n, 3, Limit::RuleActions),
            (|u, n| u.global_actions = n, 5, Limit::GlobalActions),
        ];
        for (set, cap, stop) in rows {
            for (before, expected) in [(cap - 1, Ok(())), (cap, Err(stop))] {
                // Act: `before` actions are already counted, this is the next.
                let mut usage = Usage::default();
                set(&mut usage, before);
                let got = check(&usage, &CAPS, 0.0, None, 0);

                // Assert
                assert_eq!(got, expected, "{stop:?} with {before} counted");
            }
        }
    }

    #[test]
    fn spend_stops_an_action_that_would_pass_the_dollar_cap() {
        let rule = Usage {
            rule_usd: 1.5,
            ..Usage::default()
        };
        assert_eq!(check(&rule, &CAPS, 0.5, None, 0), Ok(()));
        assert_eq!(check(&rule, &CAPS, 0.51, None, 0), Err(Limit::RuleUsd));
        let global = Usage {
            global_usd: 3.5,
            ..Usage::default()
        };
        assert_eq!(check(&global, &CAPS, 0.5, None, 0), Ok(()));
        assert_eq!(check(&global, &CAPS, 0.51, None, 0), Err(Limit::GlobalUsd));
    }

    #[test]
    fn a_second_action_on_the_same_object_waits_a_full_hour() {
        let usage = Usage::default();
        let now = 10_000;
        for (age, expected) in [
            (59 * 60, Err(Limit::Object)),
            (60 * 60, Ok(())),
            (61 * 60, Ok(())),
        ] {
            assert_eq!(check(&usage, &CAPS, 0.0, Some(now - age), now), expected);
        }
    }

    #[test]
    fn counters_roll_over_at_midnight_utc() {
        assert_eq!(day_of(86_399), 0);
        assert_eq!(day_of(86_400), 1);
    }

    fn approvals(count: usize, span_days: i64, failed: usize) -> Vec<Approval> {
        (0..count)
            .map(|i| Approval {
                at: -(span_days * 86_400) * i64::try_from(i).unwrap() / 9,
                failed: i < failed,
            })
            .collect()
    }

    #[test]
    fn the_ladder_needs_ten_approvals_over_28_days_with_none_failed() {
        // (approvals, days, failed, runs)
        let rows = [
            (9, 28, 0, false),
            (10, 27, 0, false),
            (10, 28, 1, false),
            (10, 28, 0, true),
        ];
        for (count, days, failed, runs) in rows {
            let history = approvals(count, days, failed);
            assert_eq!(
                ladder_allows(&history, 0).is_ok(),
                runs,
                "{count} approvals over {days} days, {failed} failed"
            );
        }
    }
}
