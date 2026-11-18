// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The busy sample month the README pictures are drawn from: two months of
//! working days with three models, hourly activity and a monthly budget. It
//! is fixed arrays and arithmetic only, so every run gives the same events.

use crate::limits::LimitPeriod;
use crate::limits_source::{Snapshot, Sources, Window};
use crate::UsageEvent;

const DAY: i64 = 86_400;
const HOUR: i64 = 3_600;
const MINUTE: i64 = 60;
/// Days of history before today: all of last month and the days of this one.
const HISTORY_DAYS: i64 = 50;
/// Days before today that belong to this month, so older days are last month's.
const THIS_MONTH_DAYS: i64 = 20;
/// Last month runs at this share of this month's level.
const PREVIOUS_MONTH_LEVEL: f64 = 0.8;
/// 1970-01-01 was a Thursday, three days after Monday.
const EPOCH_WEEKDAY_SHIFT: i64 = 3;
const WEEK_DAYS: i64 = 7;
/// Spend in dollars of a whole day, Monday first.
const WEEKDAY_SPEND: [f64; 7] = [26.0, 34.0, 38.0, 31.0, 22.0, 6.0, 4.0];
/// A day-to-day factor so no two days match, picked by day number.
const DAY_WOBBLE: [f64; 7] = [0.85, 1.1, 0.95, 1.2, 0.9, 1.05, 1.0];
/// The hours of work and how much of the day's spend each one gets.
const WORK_HOURS: [(i64, f64); 9] = [
    (9, 0.08),
    (10, 0.14),
    (11, 0.13),
    (12, 0.05),
    (13, 0.1),
    (14, 0.16),
    (15, 0.14),
    (16, 0.12),
    (17, 0.08),
];
/// Hours of work that share one session.
const HOURS_PER_SESSION: usize = 3;
/// Model id and its share of the spend: an Opus, a Sonnet and a Haiku.
const MODEL_SHARES: [(&str, f64); 3] = [
    ("claude-opus-5", 0.62),
    ("claude-sonnet-5", 0.30),
    ("claude-haiku-5", 0.08),
];
const PROJECTS: [&str; 4] = ["api-gateway", "web-app", "data-pipeline", "docs-site"];
/// Tokens an event carries per dollar it costs.
const INPUT_PER_USD: f64 = 60_000.0;
const OUTPUT_PER_USD: f64 = 8_000.0;
const CACHE_READ_PER_USD: f64 = 600_000.0;
/// Events of the live session, as seconds before now, with their cost.
const LIVE_EVENTS: [(i64, f64); 4] = [(20, 0.9), (300, 0.6), (900, 1.4), (1_800, 0.8)];
/// The monthly budget in dollars, and how much of the 5 hour and 7 day plan
/// windows is used.
const MONTHLY_BUDGET: f64 = 600.0;
const FIVE_HOUR_USED: f64 = 38.0;
const SEVEN_DAY_USED: f64 = 57.0;
const FIVE_HOUR_LEFT: i64 = 3 * HOUR;
const SEVEN_DAY_LEFT: i64 = 4 * DAY;
/// The statusline read the plan windows this long ago.
const SNAPSHOT_AGE: i64 = MINUTE;

fn event(ts: i64, session: String, model: &str, repo: &str, cost: f64) -> UsageEvent {
    UsageEvent {
        event_id: format!("{ts}{session}{model}"),
        timestamp: ts,
        session_id: session,
        model: model.into(),
        repo: repo.into(),
        branch: "main".into(),
        input_tokens: (cost * INPUT_PER_USD) as u64,
        output_tokens: (cost * OUTPUT_PER_USD) as u64,
        cache_read_tokens: (cost * CACHE_READ_PER_USD) as u64,
        cost_usd: cost,
        ..UsageEvent::default()
    }
}

/// The events of the working days before `now` and a live session just before
/// it. The days count back from the UTC start of the day of `now`.
pub(super) fn events(now: i64) -> Vec<UsageEvent> {
    let today = now - now % DAY;
    let mut events = Vec::new();
    for age in 1..=HISTORY_DAYS {
        let start = today - age * DAY;
        let day = start / DAY;
        let weekday = (day + EPOCH_WEEKDAY_SHIFT).rem_euclid(WEEK_DAYS) as usize;
        let level = if age > THIS_MONTH_DAYS {
            PREVIOUS_MONTH_LEVEL
        } else {
            1.0
        };
        let spend = WEEKDAY_SPEND[weekday] * DAY_WOBBLE[day.rem_euclid(WEEK_DAYS) as usize] * level;
        for (slot, (hour, share)) in WORK_HOURS.iter().enumerate() {
            let repo = PROJECTS[(day as usize + slot) % PROJECTS.len()];
            let session = format!("s-{day}-{}", slot / HOURS_PER_SESSION);
            let ts = start + hour * HOUR + slot as i64 * MINUTE;
            for (model, model_share) in MODEL_SHARES {
                let cost = spend * share * model_share;
                events.push(event(ts, session.clone(), model, repo, cost));
            }
        }
    }
    let (live_model, _) = MODEL_SHARES[0];
    for (ago, cost) in LIVE_EVENTS {
        events.push(event(
            now - ago,
            "s-live".into(),
            live_model,
            PROJECTS[0],
            cost,
        ));
    }
    events
}

/// The plan windows and the monthly budget a user of the pictures would have.
pub(super) fn sources(now: i64) -> Sources {
    let window = |used_percentage, left| Window {
        used_percentage,
        resets_at: now + left,
    };
    Sources {
        snapshot: Some(Snapshot {
            captured_at: now - SNAPSHOT_AGE,
            five_hour: Some(window(FIVE_HOUR_USED, FIVE_HOUR_LEFT)),
            seven_day: Some(window(SEVEN_DAY_USED, SEVEN_DAY_LEFT)),
            ..Snapshot::default()
        }),
        budgets: vec![(LimitPeriod::Monthly, MONTHLY_BUDGET)],
        ..Sources::default()
    }
}
