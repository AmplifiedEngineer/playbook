// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The typed query layer both views read: the date range, the totals over it,
//! and the live summary. `--json` turns these into JSON (`api.rs`) and
//! the terminal view draws them, so a number is computed in one place only.

use super::aggregate::civil_from_days;
use super::db;
use super::telemetry_db::RowScope;
use super::{ToolInvocationEvent, UsageEvent};
use playbook_store::Connection;
use std::collections::BTreeMap;

/// The day a calendar week starts on. Monday is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WeekStart {
    #[default]
    Monday,
    Sunday,
    Saturday,
}

impl WeekStart {
    pub fn parse(value: &str) -> Option<WeekStart> {
        match value {
            "monday" => Some(WeekStart::Monday),
            "sunday" => Some(WeekStart::Sunday),
            "saturday" => Some(WeekStart::Saturday),
            _ => None,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            WeekStart::Monday => "monday",
            WeekStart::Sunday => "sunday",
            WeekStart::Saturday => "saturday",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            WeekStart::Monday => "Mon",
            WeekStart::Sunday => "Sun",
            WeekStart::Saturday => "Sat",
        }
    }

    /// The weekday index of the first day, with Monday as 0.
    fn index(self) -> i64 {
        match self {
            WeekStart::Monday => 0,
            WeekStart::Saturday => 5,
            WeekStart::Sunday => 6,
        }
    }
}

/// Weekday of a day number, Monday as 0 to Sunday as 6. 1970-01-01 (day 0)
/// was a Thursday.
pub fn weekday(day: i64) -> usize {
    (day + 3).rem_euclid(7) as usize
}

/// The date window both views show. Days are in the chosen time zone (`tz`), so
/// with daylight saving a day can be 23 or 25 hours long; use `tz::day_start`
/// for a day boundary, never `day * 86_400`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    Today,
    ThisWeek(WeekStart),
    LastWeek(WeekStart),
    LastDays(i64),
    CurrentMonth,
    LastMonth,
    All,
}

impl Range {
    /// The `range` query value, with weeks starting on Monday. No value means
    /// all time, so older callers keep their behaviour.
    pub fn parse(value: Option<&str>) -> Option<Range> {
        Range::parse_with(value, WeekStart::Monday)
    }

    /// Like `parse`, with the configured first day of the week.
    pub fn parse_with(value: Option<&str>, week: WeekStart) -> Option<Range> {
        match value {
            None | Some("all") => Some(Range::All),
            Some("today") => Some(Range::Today),
            Some("week") => Some(Range::ThisWeek(week)),
            Some("last-week") => Some(Range::LastWeek(week)),
            Some("30d") => Some(Range::LastDays(30)),
            Some("60d") => Some(Range::LastDays(60)),
            Some("90d") => Some(Range::LastDays(90)),
            Some("month") => Some(Range::CurrentMonth),
            Some("last-month") => Some(Range::LastMonth),
            Some(_) => None,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Range::Today => "today",
            Range::ThisWeek(_) => "week",
            Range::LastWeek(_) => "last-week",
            Range::LastDays(30) => "30d",
            Range::LastDays(60) => "60d",
            Range::LastDays(_) => "90d",
            Range::CurrentMonth => "month",
            Range::LastMonth => "last-month",
            Range::All => "all",
        }
    }

    /// The name shown to people.
    pub fn label(self) -> &'static str {
        match self {
            Range::Today => "today",
            Range::ThisWeek(_) => "this week",
            Range::LastWeek(_) => "last week",
            Range::LastDays(30) => "last 30 days",
            Range::LastDays(60) => "last 60 days",
            Range::LastDays(_) => "last 90 days",
            Range::CurrentMonth => "this month",
            Range::LastMonth => "last month",
            Range::All => "all time",
        }
    }

    /// The same range with weeks starting on `week`.
    pub fn with_week_start(self, week: WeekStart) -> Range {
        match self {
            Range::ThisWeek(_) => Range::ThisWeek(week),
            Range::LastWeek(_) => Range::LastWeek(week),
            other => other,
        }
    }

    /// First and one-past-last day, or `None` for all time. "Last N days"
    /// includes today; a month runs from its 1st to its last day; a week runs
    /// seven days from its first day.
    pub fn days(self, now: i64) -> Option<(i64, i64)> {
        let today = crate::tz::day_of(now);
        match self {
            Range::All => None,
            Range::Today => Some((today, today + 1)),
            Range::ThisWeek(w) => {
                let first = week_first_day(today, w);
                Some((first, first + 7))
            }
            Range::LastWeek(w) => {
                let first = week_first_day(today, w);
                Some((first - 7, first))
            }
            Range::LastDays(n) => Some((today - n + 1, today + 1)),
            Range::CurrentMonth => {
                let first = month_first_day(today);
                Some((first, month_first_day(first + 31)))
            }
            Range::LastMonth => {
                let first = month_first_day(today);
                Some((month_first_day(first - 1), first))
            }
        }
    }

    /// Whether an event at `timestamp` falls inside the range at `now`.
    pub fn contains(self, now: i64, timestamp: i64) -> bool {
        in_window(self.days(now), timestamp)
    }
}

/// The first day of the week that holds `day`.
pub(super) fn week_first_day(day: i64, week: WeekStart) -> i64 {
    day - (weekday(day) as i64 - week.index()).rem_euclid(7)
}

/// The 1st of the month that holds `day`.
pub(super) fn month_first_day(day: i64) -> i64 {
    day - (civil_from_days(day).2 - 1)
}

/// The UTC month that holds `at`, as `(start, end)` in seconds. Claude bills by
/// the UTC month, whatever zone this machine is in.
pub(super) fn utc_month_bounds(at: i64) -> (i64, i64) {
    let utc = crate::tz::Zone::Utc;
    let first = month_first_day(utc.day_of(at));
    (
        utc.day_start(first),
        utc.day_start(month_first_day(first + 31)),
    )
}

pub fn day_of(timestamp: i64) -> i64 {
    crate::tz::day_of(timestamp)
}

pub fn in_window(window: Option<(i64, i64)>, timestamp: i64) -> bool {
    window.is_none_or(|(first, past)| (first..past).contains(&day_of(timestamp)))
}

pub fn total_tokens(e: &UsageEvent) -> u64 {
    e.input_tokens + e.output_tokens + e.cache_creation_tokens + e.cache_read_tokens
}

/// The sums over a set of events, the headline numbers of both views.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Totals {
    pub messages: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub cost_usd: f64,
}

impl Totals {
    pub fn tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_creation_tokens + self.cache_read_tokens
    }

    pub fn of(events: &[UsageEvent]) -> Totals {
        // `sum` rather than a fold from 0.0: an empty sum is -0.0 and the
        // pinned JSON bytes include it.
        Totals {
            messages: events.len(),
            input_tokens: events.iter().map(|e| e.input_tokens).sum(),
            output_tokens: events.iter().map(|e| e.output_tokens).sum(),
            cache_creation_tokens: events.iter().map(|e| e.cache_creation_tokens).sum(),
            cache_read_tokens: events.iter().map(|e| e.cache_read_tokens).sum(),
            cost_usd: events.iter().map(|e| e.cost_usd).sum(),
        }
    }
}

/// The usage and tool events inside `range` at `now`.
pub fn in_range(
    usage: &[UsageEvent],
    tools: &[ToolInvocationEvent],
    now: i64,
    range: Range,
) -> (Vec<UsageEvent>, Vec<ToolInvocationEvent>) {
    let window = range.days(now);
    (
        usage
            .iter()
            .filter(|e| in_window(window, e.timestamp))
            .cloned()
            .collect(),
        tools
            .iter()
            .filter(|t| in_window(window, t.timestamp))
            .cloned()
            .collect(),
    )
}

/// The events a ranged view is built from. Everything before the first day of
/// the range is dropped downstream, so all time is the only range that needs
/// every row.
pub fn load_window(conn: &Connection, range: Range, now: i64) -> Result<Vec<UsageEvent>, String> {
    match range.days(now) {
        Some((first_day, _)) => db::load_usage_events_since(conn, crate::tz::day_start(first_day)),
        None => db::load_usage_events(conn),
    }
}

/// Cost per agent from telemetry. Only telemetry knows which agent made a
/// request, so this is empty for a range with no telemetry rows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentView {
    /// One group per agent name, the main session as `main`. Sorted by name.
    pub rows: Vec<super::aggregate::Group>,
    /// Telemetry cost over the range, all agents.
    pub cost_usd: f64,
    /// The rows are invocation counts read from transcripts, with no cost.
    pub from_transcripts: bool,
}

impl AgentView {
    /// Share of `total_cost` (the transcript spend of the range) that
    /// telemetry covers, from 0 to 1.
    pub fn coverage(&self, total_cost: f64) -> f64 {
        if total_cost > 0.0 {
            (self.cost_usd / total_cost).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// Whether telemetry has any request in the range.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// The agent split of the range, read from the telemetry tables. The
/// transcript totals are not touched.
pub fn load_agents(
    conn: &Connection,
    range: Range,
    now: i64,
    machine: Option<&str>,
) -> Result<AgentView, String> {
    let (since, until) = window_seconds(range, now);
    let rows = super::telemetry_db::agent_groups(conn, since, until, RowScope::machine(machine))?;
    if rows.is_empty() {
        return transcript_view(conn, crate::ToolKind::Agent, since, until, machine);
    }
    let cost_usd = rows.iter().map(|g| g.cost_usd).sum();
    Ok(AgentView {
        rows,
        cost_usd,
        from_transcripts: false,
    })
}

/// The name Claude Code reports for a skill of a non official plugin.
const HIDDEN_PLUGIN_SKILL: &str = "third-party";

/// Cost per skill from telemetry, in the same shape as the agent split. A
/// request made while a skill was active carries its name; the rest are not
/// part of any skill row. Claude Code reports a skill of a plugin that is not
/// an official one as `third-party`, the real name is hidden unless tool
/// details are logged, which playbook never asks for. The row is relabeled so
/// it does not read as a skill with that name.
pub fn load_skills(
    conn: &Connection,
    range: Range,
    now: i64,
    machine: Option<&str>,
) -> Result<AgentView, String> {
    let (since, until) = window_seconds(range, now);
    let mut rows =
        super::telemetry_db::skill_groups(conn, since, until, RowScope::machine(machine))?;
    for row in &mut rows {
        if row.key == HIDDEN_PLUGIN_SKILL {
            row.key = "plugin skills (names hidden)".to_string();
        }
    }
    if rows.is_empty() {
        return transcript_view(conn, crate::ToolKind::Skill, since, until, machine);
    }
    let cost_usd = rows.iter().map(|g| g.cost_usd).sum();
    Ok(AgentView {
        rows,
        cost_usd,
        from_transcripts: false,
    })
}

/// Invocation counts of `kind`  read from transcripts,
/// the fallback when telemetry has no request in the range. Counts only,
/// since a transcript does not say which agent paid for a request.
fn transcript_view(
    conn: &Connection,
    kind: crate::ToolKind,
    since: i64,
    until: i64,
    machine: Option<&str>,
) -> Result<AgentView, String> {
    let rows = super::telemetry_db::invocation_groups(
        conn,
        kind,
        since,
        until,
        RowScope::machine(machine),
    )?;
    Ok(AgentView {
        rows,
        cost_usd: 0.0,
        from_transcripts: true,
    })
}

/// Tool latency and errors, API errors and retries, and busy time from
/// telemetry, for the range and, for a period, the previous span of the same
/// length. All of it comes from telemetry, so a range with no rows is empty.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HealthView {
    pub tools: Vec<super::telemetry_db::ToolStat>,
    pub tools_before: Vec<super::telemetry_db::ToolStat>,
    pub now: super::telemetry_db::Health,
    /// `None` for a range that is not a period.
    pub before: Option<super::telemetry_db::Health>,
}

pub fn load_health(
    conn: &Connection,
    range: Range,
    now: i64,
    machine: Option<&str>,
) -> Result<HealthView, String> {
    use super::telemetry_db::{health, tool_stats};
    if let Some(period) = super::period::Period::of_range(range) {
        let w = period.windows(now);
        return Ok(HealthView {
            tools: tool_stats(
                conn,
                w.current.start,
                w.current.end,
                RowScope::machine(machine),
            )?,
            tools_before: tool_stats(
                conn,
                w.previous.start,
                w.previous.end,
                RowScope::machine(machine),
            )?,
            now: health(
                conn,
                w.current.start,
                w.current.end,
                RowScope::machine(machine),
            )?,
            before: Some(health(
                conn,
                w.previous.start,
                w.previous.end,
                RowScope::machine(machine),
            )?),
        });
    }
    let (since, until) = window_seconds(range, now);
    Ok(HealthView {
        tools: tool_stats(conn, since, until, RowScope::machine(machine))?,
        tools_before: Vec::new(),
        now: health(conn, since, until, RowScope::machine(machine))?,
        before: None,
    })
}

fn window_seconds(range: Range, now: i64) -> (i64, i64) {
    match range.days(now) {
        Some((first, past)) => (crate::tz::day_start(first), crate::tz::day_start(past)),
        None => (i64::MIN, i64::MAX),
    }
}

/// A session counts as active when its last message is this recent.
pub const ACTIVE_SECONDS: i64 = 15 * 60;
pub const BURN_MINUTES: i64 = 60;
const ACTIVE_LIMIT: usize = 20;
/// Messages in the live feed.
pub const FEED_LIMIT: usize = 20;

/// Start of the first of the 60 minutes the burn chart and "last hour" cover:
/// the current minute and the 59 before it.
pub fn hour_start(now: i64) -> i64 {
    (now.div_euclid(60) - (BURN_MINUTES - 1)) * 60
}

/// The oldest timestamp the live summary needs: the start of today or
/// of the last hour, whichever is earlier.
pub fn live_window_start(now: i64) -> i64 {
    hour_start(now).min(crate::tz::day_start(crate::tz::day_of(now)))
}

/// Ids of sessions with a message in the last 15 minutes, newest first.
pub fn active_session_ids(window: &[UsageEvent], now: i64) -> Vec<String> {
    let mut last: BTreeMap<&str, i64> = BTreeMap::new();
    for e in window.iter().filter(|e| e.timestamp > now - ACTIVE_SECONDS) {
        let at = last.entry(e.session_id.as_str()).or_insert(0);
        *at = (*at).max(e.timestamp);
    }
    let mut ids: Vec<(&str, i64)> = last.into_iter().collect();
    ids.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    ids.into_iter()
        .take(ACTIVE_LIMIT)
        .map(|(id, _)| id.to_string())
        .collect()
}

/// Messages, tokens and cost over a slice of events.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Spend {
    pub messages: usize,
    pub tokens: u64,
    pub cost_usd: f64,
    pub unpriced: usize,
}

impl Spend {
    pub fn of(events: &[&UsageEvent]) -> Spend {
        Spend {
            messages: events.len(),
            tokens: events.iter().map(|e| total_tokens(e)).sum(),
            cost_usd: events.iter().map(|e| e.cost_usd).sum(),
            unpriced: events.iter().filter(|e| e.unpriced).count(),
        }
    }
}

/// One session's activity on one day. A session that runs past midnight
/// has a row for each day it was active.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionDay {
    pub day: i64,
    pub id: String,
    pub repo: String,
    /// The model that cost the most in the session that day.
    pub model: String,
    pub messages: usize,
    pub tokens: u64,
    pub cost_usd: f64,
    pub unpriced: usize,
}

/// The first characters of a session id, enough to tell sessions apart.
pub fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Groups events by day and session, newest day first and the dearest session
/// first within a day. Ties fall back to the session id so the order is stable.
pub fn session_days(events: &[UsageEvent]) -> Vec<SessionDay> {
    let mut groups: BTreeMap<(i64, &str), Vec<&UsageEvent>> = BTreeMap::new();
    for e in events {
        groups
            .entry((day_of(e.timestamp), e.session_id.as_str()))
            .or_default()
            .push(e);
    }
    let mut rows: Vec<SessionDay> = groups
        .into_iter()
        .map(|((day, id), group)| {
            let mut per_model: BTreeMap<&str, (f64, usize)> = BTreeMap::new();
            for e in &group {
                let slot = per_model.entry(e.model.as_str()).or_default();
                slot.0 += e.cost_usd;
                slot.1 += 1;
            }
            let model = per_model
                .iter()
                .max_by(|a, b| {
                    a.1 .0
                        .total_cmp(&b.1 .0)
                        .then(a.1 .1.cmp(&b.1 .1))
                        .then_with(|| b.0.cmp(a.0))
                })
                .map_or(String::new(), |(name, _)| (*name).to_string());
            let newest = group.iter().max_by_key(|e| e.timestamp);
            let spend = Spend::of(&group);
            SessionDay {
                day,
                id: id.to_string(),
                repo: newest.map_or(String::new(), |e| e.repo.clone()),
                model,
                messages: spend.messages,
                tokens: spend.tokens,
                cost_usd: spend.cost_usd,
                unpriced: spend.unpriced,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.day
            .cmp(&a.day)
            .then_with(|| b.cost_usd.total_cmp(&a.cost_usd))
            .then_with(|| a.id.cmp(&b.id))
    });
    rows
}

/// One active session: its newest message and the spend over the whole session.
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveSession {
    pub id: String,
    pub repo: String,
    pub agent: String,
    pub model: String,
    pub last: i64,
    pub messages: usize,
    pub cost_usd: f64,
    pub unpriced: usize,
}

/// What the live view shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveView {
    pub generated_at: i64,
    pub active: Vec<ActiveSession>,
    pub hour: Spend,
    pub today: Spend,
    /// Cost per minute for the last 60 minutes, oldest first, as (`HH:MM`, USD).
    pub burn: Vec<(String, f64)>,
    /// The newest messages, newest first.
    pub feed: Vec<UsageEvent>,
}

/// `HH:MM:SS` of a timestamp in the chosen time zone.
pub fn clock(timestamp: i64) -> String {
    crate::tz::clock(timestamp)
}

/// Builds the live view. `window` holds every event since `live_window_start`,
/// `sessions` every event of the active sessions (so "spend so far" is the
/// whole session), `latest` the newest messages, newest first.
pub fn live_view(
    window: &[UsageEvent],
    sessions: &[UsageEvent],
    latest: &[UsageEvent],
    now: i64,
) -> LiveView {
    let today_start = crate::tz::day_start(crate::tz::day_of(now));
    let hour: Vec<&UsageEvent> = window
        .iter()
        .filter(|e| e.timestamp >= hour_start(now))
        .collect();
    let today: Vec<&UsageEvent> = window
        .iter()
        .filter(|e| e.timestamp >= today_start)
        .collect();

    let first_minute = now.div_euclid(60) - (BURN_MINUTES - 1);
    let mut per_minute = vec![0.0_f64; BURN_MINUTES as usize];
    for e in &hour {
        let slot = e.timestamp.div_euclid(60) - first_minute;
        if (0..BURN_MINUTES).contains(&slot) {
            per_minute[slot as usize] += e.cost_usd;
        }
    }
    let burn = per_minute
        .into_iter()
        .enumerate()
        .map(|(i, cost)| (clock((first_minute + i as i64) * 60)[..5].to_string(), cost))
        .collect();

    let active = active_session_ids(window, now)
        .iter()
        .filter_map(|id| {
            let events: Vec<&UsageEvent> =
                sessions.iter().filter(|e| &e.session_id == id).collect();
            let newest = events.iter().max_by_key(|e| e.timestamp).copied()?;
            Some(ActiveSession {
                id: id.clone(),
                repo: newest.repo.clone(),
                agent: newest.agent.clone(),
                model: newest.model.clone(),
                last: newest.timestamp,
                messages: events.len(),
                cost_usd: events.iter().map(|e| e.cost_usd).sum(),
                unpriced: events.iter().filter(|e| e.unpriced).count(),
            })
        })
        .collect();

    LiveView {
        generated_at: now,
        active,
        hour: Spend::of(&hour),
        today: Spend::of(&today),
        burn,
        feed: latest.iter().take(FEED_LIMIT).cloned().collect(),
    }
}

/// The three event sets `live_view` takes.
#[derive(Debug, Default)]
pub struct LiveInputs {
    pub window: Vec<UsageEvent>,
    pub sessions: Vec<UsageEvent>,
    pub latest: Vec<UsageEvent>,
}

/// The three event sets `live_view` takes, read from the store at `now`: the
/// last hour and today, every event of the active sessions, and the newest
/// messages. Not the whole dataset.
pub fn load_live_inputs(
    conn: &Connection,
    now: i64,
    machine: Option<&str>,
) -> Result<LiveInputs, String> {
    let mut window = db::load_usage_events_since(conn, live_window_start(now))?;
    crate::tui::machine::keep(&mut window, machine);
    let ids = active_session_ids(&window, now);
    let mut sessions = db::load_usage_events_of_sessions(conn, &ids)?;
    crate::tui::machine::keep(&mut sessions, machine);
    let latest = db::load_latest_usage_events(conn, FEED_LIMIT as i64, machine)?;
    Ok(LiveInputs {
        window,
        sessions,
        latest,
    })
}

pub fn load_live(conn: &Connection, now: i64, machine: Option<&str>) -> Result<LiveView, String> {
    let i = load_live_inputs(conn, now, machine)?;
    Ok(live_view(&i.window, &i.sessions, &i.latest, now))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_utc_month_runs_from_the_first_to_the_first_whatever_the_zone() {
        // 2027-01-01 and 2027-02-01 at 00:00 UTC.
        let (jan, feb) = (1_798_761_600, 1_801_440_000);

        assert_eq!(utc_month_bounds(jan), (jan, feb));
        assert_eq!(utc_month_bounds(jan + 12_345), (jan, feb));
        assert_eq!(utc_month_bounds(feb - 1), (jan, feb));
        assert_eq!(utc_month_bounds(feb).0, feb);
    }

    use super::*;
    use crate::aggregate::SECONDS_PER_DAY;

    const NOW: i64 = 1_788_400_000;

    fn event(ts: i64, session: &str, cost: f64) -> UsageEvent {
        UsageEvent {
            event_id: format!("{session}{ts}"),
            timestamp: ts,
            session_id: session.into(),
            model: "sonnet".into(),
            input_tokens: 10,
            output_tokens: 20,
            cache_creation_tokens: 1,
            cache_read_tokens: 2,
            cost_usd: cost,
            ..UsageEvent::default()
        }
    }

    #[test]
    fn totals_sum_every_counter() {
        let events = vec![event(NOW, "a", 0.25), event(NOW - 5, "b", 0.5)];
        let t = Totals::of(&events);
        assert_eq!((t.messages, t.input_tokens, t.output_tokens), (2, 20, 40));
        assert_eq!((t.cache_creation_tokens, t.cache_read_tokens), (2, 4));
        assert!((t.cost_usd - 0.75).abs() < 1e-9);
    }

    #[test]
    fn range_parse_and_key_round_trip() {
        for key in ["30d", "60d", "90d", "month", "all"] {
            assert_eq!(Range::parse(Some(key)).map(Range::key), Some(key));
        }
        assert_eq!(Range::parse(None), Some(Range::All));
        assert_eq!(Range::parse(Some("7d")), None);
    }

    // All times are UTC, so there is no daylight saving: a day is always
    // 86,400 seconds and a week is always 7 days. These tests rely on that.
    const SEP_3_NOON: i64 = 1_788_436_800; // Thursday 2026-09-03
    const SEP_6_LAST_SECOND: i64 = 1_788_739_199; // Sunday 2026-09-06 23:59:59
    const SEP_7_MIDNIGHT: i64 = 1_788_739_200; // Monday 2026-09-07
    const JAN_15_2027: i64 = 1_800_014_400;
    const MAR_1_2027: i64 = 1_803_859_200;
    const MAR_1_2028: i64 = 1_835_481_600; // after a leap February

    fn span(range: Range, now: i64) -> (String, String) {
        let (first, past) = range.days(now).expect("a bounded range");
        (
            crate::aggregate::date_key(first),
            crate::aggregate::date_key(past - 1),
        )
    }

    fn dates(a: &str, b: &str) -> (String, String) {
        (a.to_string(), b.to_string())
    }

    const MON: WeekStart = WeekStart::Monday;

    #[test]
    fn today_is_one_utc_day() {
        assert_eq!(
            span(Range::Today, SEP_3_NOON),
            dates("2026-09-03", "2026-09-03")
        );
        assert_eq!(
            span(Range::Today, SEP_7_MIDNIGHT),
            dates("2026-09-07", "2026-09-07")
        );
        assert_eq!(
            span(Range::Today, SEP_7_MIDNIGHT - 1),
            dates("2026-09-06", "2026-09-06")
        );
    }

    #[test]
    fn a_week_that_spans_a_month_boundary_is_one_calendar_week() {
        // Thursday 2026-09-03: the Monday week started on August 31.
        assert_eq!(
            span(Range::ThisWeek(MON), SEP_3_NOON),
            dates("2026-08-31", "2026-09-06")
        );
        assert_eq!(
            span(Range::LastWeek(MON), SEP_3_NOON),
            dates("2026-08-24", "2026-08-30")
        );
    }

    #[test]
    fn the_week_turns_over_at_midnight_on_its_first_day() {
        assert_eq!(
            span(Range::ThisWeek(MON), SEP_6_LAST_SECOND),
            dates("2026-08-31", "2026-09-06")
        );
        assert_eq!(
            span(Range::ThisWeek(MON), SEP_7_MIDNIGHT),
            dates("2026-09-07", "2026-09-13")
        );
        assert_eq!(
            span(Range::LastWeek(MON), SEP_7_MIDNIGHT),
            dates("2026-08-31", "2026-09-06")
        );
    }

    #[test]
    fn the_configured_week_start_moves_the_week() {
        let sun = WeekStart::Sunday;
        let sat = WeekStart::Saturday;
        // Thursday 2026-09-03.
        assert_eq!(
            span(Range::ThisWeek(sun), SEP_3_NOON),
            dates("2026-08-30", "2026-09-05")
        );
        assert_eq!(
            span(Range::LastWeek(sun), SEP_3_NOON),
            dates("2026-08-23", "2026-08-29")
        );
        assert_eq!(
            span(Range::ThisWeek(sat), SEP_3_NOON),
            dates("2026-08-29", "2026-09-04")
        );
        // On the first day itself the week starts today, and on the day
        // before it the week started six days ago.
        assert_eq!(
            span(Range::ThisWeek(sun), SEP_6_LAST_SECOND),
            dates("2026-09-06", "2026-09-12")
        );
        assert_eq!(
            span(Range::ThisWeek(sun), SEP_6_LAST_SECOND - 86_400),
            dates("2026-08-30", "2026-09-05")
        );
        // Every week is seven days whatever the start.
        for w in [MON, sun, sat] {
            for r in [Range::ThisWeek(w), Range::LastWeek(w)] {
                let (first, past) = r.days(SEP_3_NOON).unwrap();
                assert_eq!(past - first, 7);
                assert_eq!(weekday(first) as i64, w.index());
            }
        }
    }

    #[test]
    fn last_month_is_the_whole_previous_calendar_month() {
        assert_eq!(
            span(Range::LastMonth, SEP_3_NOON),
            dates("2026-08-01", "2026-08-31")
        );
        // From January it is December of the year before.
        assert_eq!(
            span(Range::LastMonth, JAN_15_2027),
            dates("2026-12-01", "2026-12-31")
        );
        // Midnight of the 1st is already the new month.
        assert_eq!(
            span(Range::LastMonth, MAR_1_2027),
            dates("2027-02-01", "2027-02-28")
        );
        assert_eq!(
            span(Range::CurrentMonth, MAR_1_2027),
            dates("2027-03-01", "2027-03-31")
        );
        assert_eq!(
            span(Range::LastMonth, MAR_1_2028),
            dates("2028-02-01", "2028-02-29")
        );
        assert_eq!(
            span(Range::LastMonth, MAR_1_2027 - 1),
            dates("2027-01-01", "2027-01-31")
        );
    }

    #[test]
    fn weekday_counts_from_monday() {
        assert_eq!(weekday(0), 3); // 1970-01-01 was a Thursday
        assert_eq!(weekday(SEP_3_NOON.div_euclid(86_400)), 3);
        assert_eq!(weekday(SEP_7_MIDNIGHT.div_euclid(86_400)), 0);
        assert_eq!(weekday(SEP_6_LAST_SECOND.div_euclid(86_400)), 6);
        assert_eq!(weekday(-1), 2); // 1969-12-31 was a Wednesday
    }

    #[test]
    fn every_range_key_parses_back_and_week_start_applies_to_weeks_only() {
        for key in [
            "today",
            "week",
            "last-week",
            "30d",
            "60d",
            "90d",
            "month",
            "last-month",
            "all",
        ] {
            let r = Range::parse_with(Some(key), WeekStart::Sunday).unwrap();
            assert_eq!(r.key(), key);
            assert!(!r.label().is_empty());
        }
        assert_eq!(
            Range::parse_with(Some("week"), WeekStart::Sunday),
            Some(Range::ThisWeek(WeekStart::Sunday))
        );
        assert_eq!(
            Range::parse(Some("week")),
            Some(Range::ThisWeek(WeekStart::Monday))
        );
        assert_eq!(
            Range::ThisWeek(MON).with_week_start(WeekStart::Saturday),
            Range::ThisWeek(WeekStart::Saturday)
        );
        assert_eq!(
            Range::Today.with_week_start(WeekStart::Saturday),
            Range::Today
        );
        assert_eq!(Range::parse(Some("yesterday")), None);
        for w in ["monday", "sunday", "saturday"] {
            assert_eq!(WeekStart::parse(w).map(WeekStart::key), Some(w));
        }
        assert_eq!(WeekStart::parse("friday"), None);
    }

    #[test]
    fn contains_holds_at_the_edges_of_every_new_range() {
        let day = |d: i64| d * 86_400;
        let (first, past) = Range::LastWeek(MON).days(SEP_3_NOON).unwrap();
        let r = Range::LastWeek(MON);
        assert!(r.contains(SEP_3_NOON, day(first)));
        assert!(r.contains(SEP_3_NOON, day(past) - 1));
        assert!(!r.contains(SEP_3_NOON, day(first) - 1));
        assert!(!r.contains(SEP_3_NOON, day(past)));
        let t = Range::Today;
        assert!(t.contains(SEP_3_NOON, day(SEP_3_NOON.div_euclid(86_400))));
        assert!(!t.contains(SEP_3_NOON, day(SEP_3_NOON.div_euclid(86_400)) - 1));
    }

    #[test]
    fn range_contains_follows_utc_days() {
        let day = NOW.div_euclid(SECONDS_PER_DAY) * SECONDS_PER_DAY;
        let r = Range::LastDays(30);
        assert!(r.contains(NOW, day));
        assert!(!r.contains(NOW, day - 30 * SECONDS_PER_DAY));
        assert!(Range::All.contains(NOW, 0));
    }

    #[test]
    fn in_range_filters_both_event_kinds() {
        let old = NOW - 100 * SECONDS_PER_DAY;
        let usage = vec![event(NOW - 60, "a", 1.0), event(old, "a", 2.0)];
        let (inside, _) = in_range(&usage, &[], NOW, Range::LastDays(30));
        assert_eq!(inside.len(), 1);
        assert_eq!(Totals::of(&inside).cost_usd, 1.0);
    }

    #[test]
    fn session_days_group_by_day_and_session_newest_day_first() {
        let day = |d: i64| NOW.div_euclid(SECONDS_PER_DAY) * SECONDS_PER_DAY + d * SECONDS_PER_DAY;
        let mut a1 = event(day(0) + 100, "aaaaaaaa-1111", 1.0);
        a1.repo = "alpha".into();
        let mut a2 = event(day(0) + 200, "aaaaaaaa-1111", 2.0);
        a2.repo = "alpha".into();
        a2.model = "opus".into();
        let b = event(day(0) + 50, "bbbbbbbb-2222", 5.0);
        // The first session also worked the day before.
        let a_before = event(day(-1) + 10, "aaaaaaaa-1111", 0.5);
        let rows = session_days(&[a1, b, a_before, a2]);
        let keys: Vec<(i64, &str)> = rows.iter().map(|r| (r.day, r.id.as_str())).collect();
        let today = day(0) / SECONDS_PER_DAY;
        // Within a day the dearest session comes first.
        assert_eq!(
            keys,
            [
                (today, "bbbbbbbb-2222"),
                (today, "aaaaaaaa-1111"),
                (today - 1, "aaaaaaaa-1111")
            ]
        );
        let a_today = &rows[1];
        assert_eq!((a_today.messages, a_today.tokens), (2, 66));
        assert!((a_today.cost_usd - 3.0).abs() < 1e-9);
        assert_eq!(
            (a_today.repo.as_str(), a_today.model.as_str()),
            ("alpha", "opus")
        );
        assert_eq!(rows[2].messages, 1);
    }

    #[test]
    fn a_session_is_split_at_midnight_utc() {
        let midnight = NOW.div_euclid(SECONDS_PER_DAY) * SECONDS_PER_DAY;
        let rows = session_days(&[event(midnight - 1, "s", 1.0), event(midnight, "s", 1.0)]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].day, rows[1].day + 1);
    }

    #[test]
    fn session_days_tie_break_by_id_and_short_ids_take_eight_characters() {
        let rows = session_days(&[event(NOW, "b", 1.0), event(NOW + 1, "a", 1.0)]);
        assert_eq!(rows[0].id, "a");
        assert!(session_days(&[]).is_empty());
        assert_eq!(short_id("0199aaaa-0000-7000-8000-000000000001"), "0199aaaa");
        assert_eq!(short_id("abc"), "abc");
    }

    #[test]
    fn live_view_reports_active_sessions_and_burn() {
        let recent = event(NOW - 60, "live", 0.5);
        let stale = event(NOW - 3 * 3600, "idle", 9.0);
        let window = vec![stale, recent.clone()];
        let sessions = vec![recent.clone()];
        let v = live_view(&window, &sessions, &[recent], NOW);
        assert_eq!(v.active.len(), 1);
        assert_eq!(v.active[0].id, "live");
        assert_eq!(v.hour.messages, 1);
        assert_eq!(v.burn.len(), BURN_MINUTES as usize);
        assert!((v.burn.iter().map(|b| b.1).sum::<f64>() - 0.5).abs() < 1e-9);
        assert_eq!(v.feed.len(), 1);
    }

    #[test]
    fn a_hidden_plugin_skill_gets_a_readable_label() {
        use crate::telemetry_db::{insert_request, migrate_up, TelemetryRequest};
        let conn = Connection::open_in_memory().unwrap();
        migrate_up(&conn).unwrap();
        for (id, skill) in [("r1", "third-party"), ("r2", "review")] {
            let r = TelemetryRequest {
                session_id: "s".into(),
                request_id: id.into(),
                timestamp: NOW - 10,
                skill_name: skill.into(),
                cost_usd: 1.0,
                ..Default::default()
            };
            insert_request(&conn, &r).unwrap();
        }
        let v = load_skills(&conn, Range::LastDays(1), NOW, None).unwrap();
        let keys: Vec<&str> = v.rows.iter().map(|g| g.key.as_str()).collect();
        assert!(keys.contains(&"review") && keys.contains(&"plugin skills (names hidden)"));
        assert!(!keys.contains(&"third-party"));
    }

    #[test]
    fn health_for_a_period_compares_the_same_span_of_the_previous_one() {
        use crate::telemetry_db::{insert_tool, migrate_up, TelemetryTool};
        let conn = Connection::open_in_memory().unwrap();
        migrate_up(&conn).unwrap();
        let tool = |id: &str, ts: i64| TelemetryTool {
            session_id: "s".into(),
            tool_use_id: id.into(),
            timestamp: ts,
            tool_name: "Bash".into(),
            success: true,
            duration_ms: 10,
        };
        let day = NOW.div_euclid(SECONDS_PER_DAY) * SECONDS_PER_DAY;
        insert_tool(&conn, &tool("today", day)).unwrap();
        insert_tool(&conn, &tool("yesterday", day - SECONDS_PER_DAY + 60)).unwrap();
        insert_tool(&conn, &tool("old", day - 3 * SECONDS_PER_DAY)).unwrap();
        let v = load_health(&conn, Range::Today, NOW, None).unwrap();
        assert_eq!(v.now.tool_calls, 1);
        assert_eq!(v.before.as_ref().unwrap().tool_calls, 1);
        assert_eq!(v.tools_before.len(), 1);
        let v = load_health(&conn, Range::LastDays(30), NOW, None).unwrap();
        assert_eq!(v.now.tool_calls, 3);
        assert!(v.before.is_none() && v.tools_before.is_empty());
    }

    fn transcript_db(tag: &str) -> (Connection, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("pbtop-q-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let conn = crate::db::open_db(&dir.join("usage.db")).unwrap();
        for (id, kind, name) in [
            ("1", crate::ToolKind::Agent, "Explore"),
            ("2", crate::ToolKind::Agent, "Explore"),
            ("3", crate::ToolKind::Skill, "plan"),
        ] {
            let e = crate::ToolInvocationEvent {
                event_id: id.into(),
                timestamp: NOW,
                session_id: "s".into(),
                account: "a".into(),
                kind,
                name: name.into(),
            };
            crate::db::insert_tool_event(&conn, &e).unwrap();
        }
        (conn, dir)
    }

    #[test]
    fn agents_and_skills_fall_back_to_transcript_counts_when_telemetry_is_empty() {
        let (conn, dir) = transcript_db("fallback");

        let agents = load_agents(&conn, Range::All, NOW, None).unwrap();
        let skills = load_skills(&conn, Range::All, NOW, None).unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        assert!(agents.from_transcripts && skills.from_transcripts);
        assert_eq!(agents.rows.len(), 1);
        assert_eq!(agents.rows[0].messages, 2);
        assert_eq!(skills.rows[0].key, "plan");
        assert!(agents.cost_usd.abs() < 1e-9);
    }

    #[test]
    fn telemetry_rows_win_over_the_transcript_fallback() {
        let (conn, dir) = transcript_db("wins");
        let r = crate::telemetry_db::TelemetryRequest {
            session_id: "s".into(),
            request_id: "r".into(),
            timestamp: NOW,
            model: "m".into(),
            effort: String::new(),
            agent_name: "reviewer".into(),
            input_tokens: 1,
            output_tokens: 1,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            cost_usd: 0.5,
            skill_name: String::new(),
            duration_ms: 0,
        };
        crate::telemetry_db::insert_request(&conn, &r).unwrap();

        let agents = load_agents(&conn, Range::All, NOW, None).unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        assert!(!agents.from_transcripts);
        assert_eq!(agents.rows[0].key, "reviewer");
    }
}
