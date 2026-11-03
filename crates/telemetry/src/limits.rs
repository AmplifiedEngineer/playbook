// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Spending limits and budget headroom. A limit is a dollar budget (manual, or
//! measured behind a Claude apps gateway) or a Pro and Max usage window in
//! percent (measured, from Claude Code's statusline input). The math here is
//! pure: it takes the period bounds, the amount used so far and the limit, and
//! says how fast the amount is being used, how fast it may be used for the
//! rest of the period, and where that ends. Every instant is a UTC second.
//!
//! The same math serves dollars and percent: for a window the limit is 100
//! and `used` is the percent consumed.

use super::aggregate::SECONDS_PER_DAY;
use super::query::{day_of, month_first_day, utc_month_bounds, week_first_day, WeekStart};
use super::UsageEvent;
use serde_json::{json, Value};

const HOUR: f64 = 3600.0;
const DAY: f64 = SECONDS_PER_DAY as f64;

/// A projection needs at least this much elapsed time. Before that the burn
/// rate is one message scaled up by a thousand, which says nothing.
pub const MIN_PROJECTION_SECONDS: i64 = 3600;

/// A projected total at or above this share of the limit is "on track" (close
/// to the limit) rather than "under".
const NEAR_LIMIT: f64 = 0.8;

/// What the number measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Dollars,
    Percent,
}

/// The period a limit covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitPeriod {
    Daily,
    Weekly,
    Monthly,
    /// The Pro and Max session window.
    FiveHour,
    /// The Pro and Max weekly window.
    SevenDay,
}

impl LimitPeriod {
    pub fn key(self) -> &'static str {
        match self {
            LimitPeriod::Daily => "daily",
            LimitPeriod::Weekly => "weekly",
            LimitPeriod::Monthly => "monthly",
            LimitPeriod::FiveHour => "five_hour",
            LimitPeriod::SevenDay => "seven_day",
        }
    }

    pub fn parse(value: &str) -> Option<LimitPeriod> {
        match value {
            "daily" => Some(LimitPeriod::Daily),
            "weekly" => Some(LimitPeriod::Weekly),
            "monthly" => Some(LimitPeriod::Monthly),
            _ => None,
        }
    }

    /// The period that ends at `end` (a reset time), as `(start, end)`.
    /// Windows have a fixed length. A monthly period starts on the 1st of the
    /// month before `end`, so a leap February is 29 days long.
    pub fn ending_at(self, end: i64) -> (i64, i64) {
        let start = match self {
            LimitPeriod::Daily => end - SECONDS_PER_DAY,
            LimitPeriod::Weekly | LimitPeriod::SevenDay => end - 7 * SECONDS_PER_DAY,
            LimitPeriod::FiveHour => end - 5 * 3600,
            LimitPeriod::Monthly => {
                // `end` is a reset time chosen by the server, not a local
                // calendar boundary, so its month is found in UTC.
                utc_month_bounds(end - 1).0
            }
        };
        (start, end)
    }

    /// The calendar period that holds `now`, as `(start, end)`.
    pub fn containing(self, now: i64, week: WeekStart) -> (i64, i64) {
        let today = day_of(now);
        let (first, past) = match self {
            LimitPeriod::Daily => (today, today + 1),
            LimitPeriod::Weekly | LimitPeriod::SevenDay => {
                let first = week_first_day(today, week);
                (first, first + 7)
            }
            LimitPeriod::Monthly => {
                let first = month_first_day(today);
                (first, month_first_day(first + 31))
            }
            LimitPeriod::FiveHour => {
                return (
                    now - now.rem_euclid(5 * 3600),
                    now - now.rem_euclid(5 * 3600) + 5 * 3600,
                )
            }
        };
        (crate::tz::day_start(first), crate::tz::day_start(past))
    }
}

/// Where a limit comes from. The UI says which, so a measured number is never
/// mistaken for one the user typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Read from Claude Code's own statusline input (`rate_limits`).
    Measured,
    /// Set by the user in `usage.budget.*`.
    Manual,
    /// Read from the usage numbers Claude Code caches in its config file.
    /// The age of that cache is shown next to it.
    Cached,
}

impl Origin {
    pub fn key(self) -> &'static str {
        match self {
            Origin::Measured => "measured",
            Origin::Manual => "manual",
            Origin::Cached => "cached",
        }
    }
}

/// One known limit with the amount used against it.
#[derive(Debug, Clone, PartialEq)]
pub struct Limit {
    pub unit: Unit,
    pub period: LimitPeriod,
    pub origin: Origin,
    /// Dollars or percent used so far in the period.
    pub used: f64,
    /// The limit in the same unit (100 for a window).
    pub limit: f64,
    /// Period bounds, UTC seconds, half open.
    pub start: i64,
    pub end: i64,
}

/// How the period is going.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum State {
    /// The limit is already reached.
    Exceeded,
    /// Not enough elapsed time (or the period is over) to project.
    Unknown,
    /// The projected total stays below 80 percent of the limit.
    Under,
    /// The projected total lands between 80 percent and the limit.
    OnTrack,
    /// At the current rate the limit is reached at this UTC second.
    WillExceed { at: i64 },
}

impl State {
    pub fn key(self) -> &'static str {
        match self {
            State::Exceeded => "exceeded",
            State::Unknown => "unknown",
            State::Under => "under",
            State::OnTrack => "on_track",
            State::WillExceed { .. } => "will_exceed",
        }
    }
}

/// The headroom of one limit at one instant. Rates are per hour and per day in
/// the limit's unit; `None` where the input has no meaning (no elapsed time, no
/// time left).
#[derive(Debug, Clone, PartialEq)]
pub struct Headroom {
    pub used: f64,
    pub limit: f64,
    pub remaining: f64,
    pub elapsed_seconds: i64,
    pub remaining_seconds: i64,
    pub avg_per_hour: Option<f64>,
    pub avg_per_day: Option<f64>,
    pub allowed_per_hour: Option<f64>,
    pub allowed_per_day: Option<f64>,
    pub projected: Option<f64>,
    pub state: State,
}

/// The headroom for `used` against `limit` over `[start, end)` at `now`. A
/// `limit` of zero or less, or an empty period, yields `None`: there is
/// nothing to measure against.
pub fn headroom(start: i64, end: i64, now: i64, used: f64, limit: f64) -> Option<Headroom> {
    if !(limit.is_finite() && limit > 0.0) || end <= start || !used.is_finite() {
        return None;
    }
    let used = used.max(0.0);
    let elapsed = (now - start).clamp(0, end - start);
    let left = (end - now).clamp(0, end - start);
    let remaining = (limit - used).max(0.0);

    let rate = (elapsed > 0).then(|| used / elapsed as f64);
    let avg_per_hour = rate.map(|r| r * HOUR);
    let avg_per_day = rate.map(|r| r * DAY);
    let allowed_per_hour = (left > 0).then(|| remaining / (left as f64 / HOUR));
    let allowed_per_day = (left > 0).then(|| remaining / (left as f64 / DAY));
    let projected = match rate {
        Some(r) if elapsed >= MIN_PROJECTION_SECONDS => Some(used + r * left as f64),
        _ => None,
    };

    let state = if used >= limit {
        State::Exceeded
    } else {
        match (projected, rate) {
            (Some(p), Some(r)) if p > limit && r > 0.0 => State::WillExceed {
                at: now + ((limit - used) / r).ceil() as i64,
            },
            (Some(p), _) if p >= limit * NEAR_LIMIT => State::OnTrack,
            (Some(_), _) => State::Under,
            (None, _) => State::Unknown,
        }
    };
    Some(Headroom {
        used,
        limit,
        remaining,
        elapsed_seconds: elapsed,
        remaining_seconds: left,
        avg_per_hour,
        avg_per_day,
        allowed_per_hour,
        allowed_per_day,
        projected,
        state,
    })
}

/// The share of the limit, in percent, that the first crossing marks.
const HALF_PCT: f64 = 50.0;

/// One percent as a fraction, to turn [`NEAR_LIMIT`] into a percent.
const PERCENT: f64 = 100.0;

/// Where a limit stands against one threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossState {
    /// Already crossed. `at` is when, on the assumption that spend grew at a
    /// steady pace since the period began.
    Crossed { at: i64 },
    /// At the current pace it is crossed at this UTC second, before the
    /// period ends (the period is half open, so the end itself is beyond).
    Projected { at: i64 },
    /// At the current pace it is not crossed before the period ends.
    Beyond { period_end: i64 },
    /// Too little time has passed to say.
    Unknown,
}

impl CrossState {
    pub fn key(self) -> &'static str {
        match self {
            CrossState::Crossed { .. } => "crossed",
            CrossState::Projected { .. } => "projected",
            CrossState::Beyond { .. } => "beyond",
            CrossState::Unknown => "unknown",
        }
    }

    /// When it was or will be crossed, if the state says.
    pub fn at(self) -> Option<i64> {
        match self {
            CrossState::Crossed { at } | CrossState::Projected { at } => Some(at),
            CrossState::Beyond { .. } | CrossState::Unknown => None,
        }
    }
}

/// One threshold of a limit, in percent, with where the limit stands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Crossing {
    pub pct: f64,
    pub state: CrossState,
}

/// Where `used` stands against 50, 80 and 100 percent of `limit` for the
/// period `[start, end)` at `now`, at the same linear pace and with the same
/// [`MIN_PROJECTION_SECONDS`] as [`headroom`]. `None` where [`headroom`] has
/// nothing to measure. A threshold already passed is always `Crossed`. One not
/// yet reached is `Unknown` before the minimum time, `Beyond` when nothing has
/// been used, when the period is over, or when the pace misses the period
/// end, and `Projected` otherwise.
pub fn thresholds(start: i64, end: i64, now: i64, used: f64, limit: f64) -> Option<[Crossing; 3]> {
    if !(limit.is_finite() && limit > 0.0) || end <= start || !used.is_finite() {
        return None;
    }
    let used = used.max(0.0);
    let elapsed = (now - start).clamp(0, end - start);
    let at_pct = |pct: f64| {
        let amount = limit * pct / PERCENT;
        let state = if used >= amount {
            let crossed = if used > 0.0 && elapsed > 0 {
                start + (elapsed as f64 * amount / used).round() as i64
            } else {
                start
            };
            CrossState::Crossed {
                at: crossed.clamp(start, now.max(start)),
            }
        } else if elapsed < MIN_PROJECTION_SECONDS {
            CrossState::Unknown
        } else if used <= 0.0 || now >= end {
            CrossState::Beyond { period_end: end }
        } else {
            let wait = ((amount - used) / (used / elapsed as f64)).ceil();
            // Compared as seconds before the cast, so a tiny rate cannot
            // overflow the instant.
            if wait < (end - now) as f64 {
                CrossState::Projected {
                    at: now + wait as i64,
                }
            } else {
                CrossState::Beyond { period_end: end }
            }
        };
        Crossing { pct, state }
    };
    Some([
        at_pct(HALF_PCT),
        at_pct((NEAR_LIMIT * PERCENT).round()),
        at_pct(PERCENT),
    ])
}

impl Limit {
    pub fn headroom(&self, now: i64) -> Option<Headroom> {
        headroom(self.start, self.end, now, self.used, self.limit)
    }

    pub fn thresholds(&self, now: i64) -> Option<[Crossing; 3]> {
        thresholds(self.start, self.end, now, self.used, self.limit)
    }

    /// A manual dollar budget for the calendar period holding `now`, with
    /// `events` priced and summed from the period start through `now`.
    pub fn manual_budget(
        period: LimitPeriod,
        limit_usd: f64,
        events: &[UsageEvent],
        now: i64,
        week: WeekStart,
    ) -> Limit {
        let (start, end) = period.containing(now, week);
        Limit {
            unit: Unit::Dollars,
            period,
            origin: Origin::Manual,
            used: events
                .iter()
                .filter(|e| e.timestamp >= start && e.timestamp <= now)
                .map(|e| e.cost_usd)
                .sum::<f64>()
                + 0.0,
            limit: limit_usd,
            start,
            end,
        }
    }

    pub fn to_json(&self, now: i64) -> Value {
        let h = self.headroom(now);
        let mut v = json!({
            "unit": match self.unit { Unit::Dollars => "usd", Unit::Percent => "percent" },
            "period": self.period.key(),
            "origin": self.origin.key(),
            "used": self.used,
            "limit": self.limit,
            "start": self.start,
            "end": self.end,
        });
        if let (Some(map), Some(h)) = (v.as_object_mut(), h) {
            map.insert("remaining".into(), json!(h.remaining));
            map.insert("avg_per_hour".into(), json!(h.avg_per_hour));
            map.insert("avg_per_day".into(), json!(h.avg_per_day));
            map.insert("allowed_per_hour".into(), json!(h.allowed_per_hour));
            map.insert("allowed_per_day".into(), json!(h.allowed_per_day));
            map.insert("projected".into(), json!(h.projected));
            map.insert("state".into(), json!(h.state.key()));
            if let State::WillExceed { at } = h.state {
                map.insert("exceeds_at".into(), json!(at));
            }
        }
        if let (Some(map), Some(crossings)) = (v.as_object_mut(), self.thresholds(now)) {
            map.insert(
                "thresholds".into(),
                Value::Array(
                    crossings
                        .iter()
                        .map(|c| {
                            json!({
                                "pct": c.pct,
                                "state": c.state.key(),
                                "at": c.state.at(),
                                "period_end": match c.state {
                                    CrossState::Beyond { period_end } => Some(period_end),
                                    _ => None,
                                },
                            })
                        })
                        .collect(),
                ),
            );
        }
        v
    }
}

/// Average cost per hour and per day over the elapsed part of a span.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Burn {
    pub cost_usd: f64,
    pub seconds: i64,
    pub per_hour: Option<f64>,
    pub per_day: Option<f64>,
}

impl Burn {
    /// `None` rates when no time has elapsed. The first day is partial: the
    /// daily average divides by elapsed days as a fraction, not whole days.
    pub fn new(cost_usd: f64, seconds: i64) -> Burn {
        let seconds = seconds.max(0);
        let per = |unit: f64| (seconds > 0).then(|| cost_usd / (seconds as f64 / unit));
        Burn {
            cost_usd,
            seconds,
            per_hour: per(HOUR),
            per_day: per(DAY),
        }
    }

    pub fn to_json(self) -> Value {
        json!({
            "cost_usd": self.cost_usd,
            "elapsed_seconds": self.seconds,
            "avg_per_hour": self.per_hour,
            "avg_per_day": self.per_day,
        })
    }
}

const SECONDS_PER_HOUR: i64 = 3600;
const SECONDS_PER_MINUTE: i64 = 60;

/// Shown for a span under one minute, and for one that is not positive.
const UNDER_A_MINUTE: &str = "<1m";

/// A span of seconds as at most two units, rounded down: `4d9h`, `2d`,
/// `1h20m`, `3h`, `45m`, or `<1m`. A day or more shows days and hours, under a
/// day shows hours and minutes, under an hour shows minutes.
pub fn fmt_span(seconds: i64) -> String {
    let units = |big: i64, big_unit: &str, small: i64, small_unit: &str| {
        let (big_count, rest) = (seconds / big, seconds % big);
        match rest / small {
            0 => format!("{big_count}{big_unit}"),
            n => format!("{big_count}{big_unit}{n}{small_unit}"),
        }
    };
    if seconds >= SECONDS_PER_DAY {
        units(SECONDS_PER_DAY, "d", SECONDS_PER_HOUR, "h")
    } else if seconds >= SECONDS_PER_HOUR {
        units(SECONDS_PER_HOUR, "h", SECONDS_PER_MINUTE, "m")
    } else if seconds >= SECONDS_PER_MINUTE {
        format!("{}m", seconds / SECONDS_PER_MINUTE)
    } else {
        UNDER_A_MINUTE.to_string()
    }
}

/// A one line reading of a limit for a small display such as the statusline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Brief {
    /// The cap is reached or will be reached before the period ends. The
    /// caller marks it so the cue is more than a colour.
    pub alarm: bool,
    /// The label and words, for example `7d CAP HIT` or `5h cap in 1h20m`.
    pub text: String,
}

/// The short label of a Pro and Max window, `None` for any other period.
fn window_label(period: LimitPeriod) -> Option<&'static str> {
    match period {
        LimitPeriod::SevenDay => Some("7d"),
        LimitPeriod::FiveHour => Some("5h"),
        _ => None,
    }
}

/// The brief of a Pro and Max window given its [`headroom`] at `now`. A window
/// that is reached says `CAP HIT`, one on pace to reach the cap says how long
/// that takes. A window on track, under, or too young to project has nothing
/// to add, as its percent is already shown elsewhere. Other limits give `None`.
pub fn brief(limit: &Limit, headroom: &Headroom, now: i64) -> Option<Brief> {
    let label = window_label(limit.period).filter(|_| limit.unit == Unit::Percent)?;
    let words = match headroom.state {
        State::Exceeded => "CAP HIT".to_string(),
        State::WillExceed { at } => format!("cap in {}", fmt_span(at - now)),
        State::Unknown | State::Under | State::OnTrack => return None,
    };
    Some(Brief {
        alarm: true,
        text: format!("{label} {words}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEVEN_DAYS: i64 = 7 * D;
    const FIVE_HOURS: i64 = 5 * 3600;

    fn window(period: LimitPeriod, used: f64, length: i64) -> Limit {
        Limit {
            unit: Unit::Percent,
            period,
            origin: Origin::Measured,
            used,
            limit: 100.0,
            start: 0,
            end: length,
        }
    }

    fn brief_at(limit: &Limit, now: i64) -> Option<Brief> {
        let h = limit.headroom(now)?;
        brief(limit, &h, now)
    }

    fn text_at(limit: &Limit, now: i64) -> Option<(bool, String)> {
        brief_at(limit, now).map(|b| (b.alarm, b.text))
    }

    #[test]
    fn a_reached_window_says_cap_hit_at_any_elapsed_time() {
        let seven = window(LimitPeriod::SevenDay, 100.0, SEVEN_DAYS);
        assert_eq!(text_at(&seven, 3 * D), Some((true, "7d CAP HIT".into())));
        assert_eq!(text_at(&seven, 0), Some((true, "7d CAP HIT".into())));
        let five = window(LimitPeriod::FiveHour, 100.0, FIVE_HOURS);
        assert_eq!(text_at(&five, 60), Some((true, "5h CAP HIT".into())));
    }

    #[test]
    fn a_window_on_pace_to_reach_the_cap_names_the_span_left() {
        let seven = window(LimitPeriod::SevenDay, 60.0, SEVEN_DAYS);
        assert_eq!(text_at(&seven, 3 * D), Some((true, "7d cap in 2d".into())));
        let five = window(LimitPeriod::FiveHour, 60.0, FIVE_HOURS);
        assert_eq!(
            text_at(&five, 2 * 3600),
            Some((true, "5h cap in 1h20m".into()))
        );
    }

    #[test]
    fn a_window_that_stays_within_the_cap_says_nothing() {
        let at_half_week = 3 * D + D / 2;
        // Exactly 50 percent at the half way point projects to the limit, not past it.
        let exact = window(LimitPeriod::SevenDay, 50.0, SEVEN_DAYS);
        assert_eq!(brief_at(&exact, at_half_week), None);
        for used in [0.0, 49.9] {
            let w = window(LimitPeriod::SevenDay, used, SEVEN_DAYS);
            assert_eq!(brief_at(&w, at_half_week), None, "{used} percent");
        }
    }

    #[test]
    fn a_window_too_young_to_project_says_nothing() {
        let w = window(LimitPeriod::SevenDay, 90.0, SEVEN_DAYS);
        assert_eq!(brief_at(&w, MIN_PROJECTION_SECONDS - 1), None);
    }

    #[test]
    fn a_zero_limit_or_a_finished_period_says_nothing() {
        let mut zero = window(LimitPeriod::SevenDay, 10.0, SEVEN_DAYS);
        zero.limit = 0.0;
        assert_eq!(brief_at(&zero, 3 * D), None);
        let over = window(LimitPeriod::SevenDay, 60.0, SEVEN_DAYS);
        assert_eq!(brief_at(&over, SEVEN_DAYS + D), None);
    }

    #[test]
    fn only_the_pro_and_max_windows_have_a_brief() {
        let mut monthly = window(LimitPeriod::Monthly, 100.0, 30 * D);
        monthly.unit = Unit::Dollars;
        assert_eq!(brief_at(&monthly, 10 * D), None);
    }

    #[test]
    fn fmt_span_rounds_down_to_two_units() {
        let (m, h, d) = (60, 3600, SECONDS_PER_DAY);
        let rows = [
            (2 * d + 3 * h, "2d3h"),
            (2 * d, "2d"),
            (4 * d + 9 * h, "4d9h"),
            (d, "1d"),
            (d - 1, "23h59m"),
            (3 * h, "3h"),
            (h + 20 * m, "1h20m"),
            (h, "1h"),
            (h - 1, "59m"),
            (45 * m, "45m"),
            (59, "<1m"),
            (0, "<1m"),
            (-5, "<1m"),
        ];
        for (seconds, want) in rows {
            assert_eq!(fmt_span(seconds), want, "{seconds}s");
        }
    }

    const D: i64 = SECONDS_PER_DAY;

    fn ev(ts: i64, cost: f64) -> UsageEvent {
        UsageEvent {
            timestamp: ts,
            cost_usd: cost,
            ..UsageEvent::default()
        }
    }

    #[test]
    fn a_month_to_date_projects_and_allows_the_rest() {
        // 10 days into a 30 day month: $100 used of $300.
        let h = headroom(0, 30 * D, 10 * D, 100.0, 300.0).unwrap();
        assert_eq!(h.avg_per_day, Some(10.0));
        assert_eq!(h.avg_per_hour, Some(100.0 / 240.0));
        assert_eq!(h.remaining, 200.0);
        assert_eq!(h.allowed_per_day, Some(10.0));
        assert_eq!(h.projected, Some(300.0));
        assert_eq!(h.state, State::OnTrack);
    }

    #[test]
    fn a_fast_burn_names_when_the_limit_is_reached() {
        // $200 in 10 days of 30, limit $300: 20 a day, 100 left lasts 5 days.
        let h = headroom(0, 30 * D, 10 * D, 200.0, 300.0).unwrap();
        assert_eq!(h.projected, Some(600.0));
        assert_eq!(h.state, State::WillExceed { at: 15 * D });
    }

    #[test]
    fn a_slow_burn_is_under() {
        let h = headroom(0, 30 * D, 10 * D, 30.0, 300.0).unwrap();
        assert_eq!(h.projected, Some(90.0));
        assert_eq!(h.state, State::Under);
    }

    #[test]
    fn zero_elapsed_time_divides_nothing() {
        let h = headroom(100, 100 + 30 * D, 100, 0.0, 300.0).unwrap();
        assert_eq!(h.avg_per_hour, None);
        assert_eq!(h.projected, None);
        assert_eq!(h.state, State::Unknown);
        assert_eq!(h.allowed_per_day, Some(10.0));
    }

    #[test]
    fn before_the_period_starts_counts_as_zero_elapsed() {
        let h = headroom(1000, 1000 + D, 500, 0.0, 10.0).unwrap();
        assert_eq!(h.elapsed_seconds, 0);
        assert_eq!(h.remaining_seconds, D);
    }

    #[test]
    fn a_projection_waits_for_an_hour_of_data() {
        let h = headroom(0, D, 600, 5.0, 10.0).unwrap();
        assert!(h.avg_per_hour.is_some());
        assert_eq!(h.projected, None);
        assert_eq!(h.state, State::Unknown);
    }

    #[test]
    fn no_time_left_allows_nothing_per_hour() {
        let h = headroom(0, D, D, 4.0, 10.0).unwrap();
        assert_eq!(h.allowed_per_hour, None);
        assert_eq!(h.allowed_per_day, None);
        assert_eq!(h.projected, Some(4.0));
        assert_eq!(h.state, State::Under);
    }

    #[test]
    fn a_limit_already_exceeded_keeps_zero_remaining() {
        let h = headroom(0, 30 * D, 10 * D, 350.0, 300.0).unwrap();
        assert_eq!(h.remaining, 0.0);
        assert_eq!(h.allowed_per_day, Some(0.0));
        assert_eq!(h.state, State::Exceeded);
        let at = headroom(0, 30 * D, 10 * D, 300.0, 300.0).unwrap();
        assert_eq!(at.state, State::Exceeded);
    }

    #[test]
    fn no_limit_or_a_bad_period_is_none() {
        assert!(headroom(0, D, 10, 1.0, 0.0).is_none());
        assert!(headroom(0, D, 10, 1.0, -5.0).is_none());
        assert!(headroom(0, D, 10, 1.0, f64::NAN).is_none());
        assert!(headroom(D, D, 10, 1.0, 5.0).is_none());
        assert!(headroom(0, D, 10, f64::INFINITY, 5.0).is_none());
    }

    #[test]
    fn zero_use_is_under_with_a_zero_rate() {
        let h = headroom(0, D, 7200, 0.0, 10.0).unwrap();
        assert_eq!(h.avg_per_hour, Some(0.0));
        assert_eq!(h.state, State::Under);
    }

    #[test]
    fn percent_windows_use_the_same_math() {
        // 5 hour window, 2 hours in, 50 percent used: 25 a hour, full at hour 4.
        let h = headroom(0, 5 * 3600, 2 * 3600, 50.0, 100.0).unwrap();
        assert_eq!(h.avg_per_hour, Some(25.0));
        assert_eq!(h.projected, Some(125.0));
        assert_eq!(h.state, State::WillExceed { at: 4 * 3600 });
    }

    // 2026-03-01 00:00 UTC is day 20513.
    const MARCH_1: i64 = 20_513 * D;

    #[test]
    fn a_calendar_month_starts_on_the_first_utc_midnight() {
        let now = MARCH_1 + 10 * D + 3600; // 2026-03-11 01:00
        let (s, e) = LimitPeriod::Monthly.containing(now, WeekStart::Monday);
        assert_eq!(s, MARCH_1);
        assert_eq!(e, MARCH_1 + 31 * D);
        // The last second of March is still March; the next second is April.
        let (s, _) = LimitPeriod::Monthly.containing(MARCH_1 + 31 * D - 1, WeekStart::Monday);
        assert_eq!(s, MARCH_1);
        let (s, e) = LimitPeriod::Monthly.containing(MARCH_1 + 31 * D, WeekStart::Monday);
        assert_eq!((s, e), (MARCH_1 + 31 * D, MARCH_1 + 61 * D));
    }

    #[test]
    fn a_leap_february_has_29_days() {
        // 2028-02-01 is day 21215.
        let feb = 21_215 * D;
        let (s, e) = LimitPeriod::Monthly.containing(feb + 5 * D, WeekStart::Monday);
        assert_eq!((s, e), (feb, feb + 29 * D));
        let (s, e) = LimitPeriod::Monthly.ending_at(feb + 29 * D);
        assert_eq!((s, e), (feb, feb + 29 * D));
    }

    #[test]
    fn a_week_starts_on_the_configured_day() {
        // 2026-03-11 is a Wednesday. Monday 03-09, Sunday 03-08, Saturday 03-07.
        let now = MARCH_1 + 10 * D;
        let monday = MARCH_1 + 8 * D;
        assert_eq!(
            LimitPeriod::Weekly.containing(now, WeekStart::Monday),
            (monday, monday + 7 * D)
        );
        assert_eq!(
            LimitPeriod::Weekly.containing(now, WeekStart::Sunday).0,
            monday - D
        );
        assert_eq!(
            LimitPeriod::Weekly.containing(now, WeekStart::Saturday).0,
            monday - 2 * D
        );
    }

    #[test]
    fn windows_end_at_their_reset_time() {
        assert_eq!(LimitPeriod::FiveHour.ending_at(100_000), (82_000, 100_000));
        assert_eq!(LimitPeriod::SevenDay.ending_at(10 * D), (3 * D, 10 * D));
        assert_eq!(LimitPeriod::Daily.ending_at(5 * D), (4 * D, 5 * D));
    }

    #[test]
    fn a_manual_budget_sums_only_this_period_up_to_now() {
        let now = MARCH_1 + 10 * D;
        let events = vec![
            ev(MARCH_1 - 1, 99.0), // February
            ev(MARCH_1, 4.0),      // first second of March
            ev(now, 6.0),          // now itself counts
            ev(now + 1, 50.0),     // the future is ignored
        ];
        let l = Limit::manual_budget(LimitPeriod::Monthly, 100.0, &events, now, WeekStart::Monday);
        assert_eq!(l.used, 10.0);
        assert_eq!(l.origin, Origin::Manual);
        assert_eq!((l.start, l.end), (MARCH_1, MARCH_1 + 31 * D));
    }

    #[test]
    fn a_partial_first_day_is_a_fraction_of_a_day() {
        // $6 in the first 6 hours: $24 a day, $1 an hour.
        let b = Burn::new(6.0, 6 * 3600);
        assert_eq!(b.per_hour, Some(1.0));
        assert_eq!(b.per_day, Some(24.0));
    }

    #[test]
    fn burn_with_no_time_has_no_rate() {
        let b = Burn::new(3.0, 0);
        assert_eq!((b.per_hour, b.per_day), (None, None));
        assert_eq!(Burn::new(3.0, -5).seconds, 0);
    }

    #[test]
    fn json_carries_the_origin_and_the_state() {
        let l = Limit {
            unit: Unit::Dollars,
            period: LimitPeriod::Monthly,
            origin: Origin::Manual,
            used: 200.0,
            limit: 300.0,
            start: 0,
            end: 30 * D,
        };
        let v = l.to_json(10 * D);
        assert_eq!(v["origin"], "manual");
        assert_eq!(v["state"], "will_exceed");
        assert_eq!(v["exceeds_at"], 15 * D);
        assert_eq!(v["remaining"], 100.0);
    }

    const MONTH: i64 = 30 * D;
    const DAY_10: i64 = 10 * D;

    fn states(used: f64, now: i64) -> [CrossState; 3] {
        thresholds(0, MONTH, now, used, 100.0)
            .unwrap()
            .map(|c| c.state)
    }

    #[test]
    fn the_thresholds_are_50_80_and_100_percent() {
        let c = thresholds(0, MONTH, DAY_10, 10.0, 100.0).unwrap();

        assert_eq!(c.map(|c| c.pct), [50.0, 80.0, 100.0]);
        assert_eq!(c[1].pct, NEAR_LIMIT * 100.0);
    }

    #[test]
    fn a_threshold_at_or_below_the_used_amount_is_crossed_at_a_steady_pace() {
        // 50 used after 10 days: 50 percent was reached at day 10, and the
        // pace of 5 a day reaches 80 at day 16.
        let c = states(50.0, DAY_10);
        assert_eq!(c[0], CrossState::Crossed { at: DAY_10 });
        assert_eq!(c[1], CrossState::Projected { at: 16 * D });

        // 80 used after 10 days: 50 percent at day 6.25, 80 at day 10.
        let c = states(80.0, DAY_10);
        assert_eq!(c[0], CrossState::Crossed { at: 625 * D / 100 });
        assert_eq!(c[1], CrossState::Crossed { at: DAY_10 });
    }

    #[test]
    fn the_percent_boundaries_at_ten_days_of_thirty() {
        // Rate is used / 10 days. 49 percent reaches 50 on day 10.2, 80 on
        // day 16.33 and 100 on day 20.41. 120 is past all three.
        let table: [(f64, [&str; 3]); 8] = [
            (0.0, ["beyond", "beyond", "beyond"]),
            (49.0, ["projected", "projected", "projected"]),
            (50.0, ["crossed", "projected", "projected"]),
            (79.0, ["crossed", "projected", "projected"]),
            (80.0, ["crossed", "crossed", "projected"]),
            (99.0, ["crossed", "crossed", "projected"]),
            (100.0, ["crossed", "crossed", "crossed"]),
            (120.0, ["crossed", "crossed", "crossed"]),
        ];

        for (used, want) in table {
            let got = states(used, DAY_10).map(CrossState::key);

            assert_eq!(got, want, "used {used}");
        }
    }

    #[test]
    fn a_projected_crossing_is_the_exact_second_at_the_current_pace() {
        // $20 in 10 days is $2 a day: $50 is 15 days away at day 25.
        let c = states(20.0, DAY_10);

        assert_eq!(c[0], CrossState::Projected { at: 25 * D });
        // $80 would be day 40, past the end of the month.
        assert_eq!(c[1], CrossState::Beyond { period_end: MONTH });
        assert_eq!(c[2], CrossState::Beyond { period_end: MONTH });
    }

    #[test]
    fn a_pace_that_reaches_the_period_end_exactly_is_beyond_and_a_faster_one_is_projected() {
        // Limit 30 at 1 a day is reached exactly at day 30, the period end,
        // and the period is half open. One second less elapsed time at the
        // same total means a faster pace, which lands before the end.
        let exact = thresholds(0, MONTH, DAY_10, 10.0, 30.0).unwrap();
        let faster = thresholds(0, MONTH, DAY_10 - 1, 10.0, 30.0).unwrap();

        assert_eq!(exact[2].state, CrossState::Beyond { period_end: MONTH });
        assert!(matches!(
            faster[2].state,
            CrossState::Projected { at } if (MONTH - 3..MONTH).contains(&at)
        ));
    }

    #[test]
    fn the_minimum_projection_time_is_3600_seconds() {
        let early = thresholds(0, MONTH, MIN_PROJECTION_SECONDS - 1, 1.0, 100.0).unwrap();
        let on_time = thresholds(0, MONTH, MIN_PROJECTION_SECONDS, 1.0, 100.0).unwrap();

        assert!(early.iter().all(|c| c.state == CrossState::Unknown));
        assert!(matches!(on_time[0].state, CrossState::Projected { .. }));
    }

    #[test]
    fn a_crossed_threshold_is_reported_even_before_the_minimum_time() {
        let c = thresholds(0, MONTH, 60, 70.0, 100.0).unwrap();

        // 70 percent after 60 seconds: 50 percent was reached at 60 * 50 / 70.
        assert_eq!(c[0].state, CrossState::Crossed { at: 43 });
        assert_eq!(c[2].state, CrossState::Unknown);
    }

    #[test]
    fn after_the_period_ended_what_was_not_crossed_is_beyond() {
        let c = thresholds(0, MONTH, MONTH + D, 40.0, 100.0).unwrap();

        assert_eq!(c.map(|c| c.state.key()), ["beyond", "beyond", "beyond"]);
    }

    #[test]
    fn a_tiny_pace_cannot_overflow_the_projected_instant() {
        let c = thresholds(0, MONTH, MIN_PROJECTION_SECONDS, f64::MIN_POSITIVE, 1e300).unwrap();

        assert_eq!(c[2].state, CrossState::Beyond { period_end: MONTH });
    }

    #[test]
    fn nothing_used_is_beyond_and_a_bad_limit_has_no_thresholds() {
        assert_eq!(
            states(0.0, DAY_10),
            [CrossState::Beyond { period_end: MONTH }; 3]
        );
        assert!(thresholds(0, MONTH, DAY_10, 1.0, 0.0).is_none());
        assert!(thresholds(0, MONTH, DAY_10, 1.0, f64::NAN).is_none());
        assert!(thresholds(MONTH, 0, DAY_10, 1.0, 100.0).is_none());
        assert!(thresholds(0, MONTH, DAY_10, f64::INFINITY, 100.0).is_none());
    }

    #[test]
    fn the_json_of_a_limit_lists_its_thresholds() {
        let l = Limit {
            unit: Unit::Dollars,
            period: LimitPeriod::Monthly,
            origin: Origin::Cached,
            used: 20.0,
            limit: 100.0,
            start: 0,
            end: MONTH,
        };

        let v = l.to_json(DAY_10);

        let t = v["thresholds"].as_array().unwrap();
        assert_eq!(t.len(), 3);
        assert_eq!(t[0]["state"], "projected");
        assert_eq!(t[0]["at"], 25 * D);
        assert_eq!(t[1]["state"], "beyond");
        assert_eq!(t[1]["period_end"], MONTH);
        assert!(t[1]["at"].is_null());
    }
}
