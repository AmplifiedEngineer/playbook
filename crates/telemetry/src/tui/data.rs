// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! What the terminal view draws: one immutable snapshot of the store for a
//! range and a filter. Built by the same query layer the web JSON uses
//! (`usage::query`), so both views report the same numbers.

use super::account::{self, AccountSel, AccountsView};
use super::machine::{self, MachineSel, MachinesView};
use super::theme::SERIES;
use crate::aggregate::{group_usage, unpriced_models, Dimension, Group};
use crate::burnup::{burnups_for, spend_only, Burnup};
use crate::db;
use crate::limits::{Burn, Limit, LimitPeriod, Origin, State};
use crate::limits_source::{self, Sources};
use crate::period::{Comparison, Period};
use crate::query::{self, LiveView, Range, SessionDay, Totals, WeekStart};
use crate::run::Paths;
use crate::UsageEvent;
use playbook_store::Connection;
use std::collections::BTreeMap;

/// One calendar day of the spend and token graphs.
#[derive(Debug, Clone, PartialEq)]
pub struct DayPoint {
    pub day: i64,
    pub cost_usd: f64,
    pub tokens: u64,
    pub messages: usize,
    /// Cost per model for the day, by model name.
    pub models: Vec<(String, f64)>,
}

impl DayPoint {
    /// The model that cost the most this day, if the day cost anything.
    pub fn top_model(&self) -> Option<&str> {
        self.models
            .iter()
            .filter(|(_, cost)| *cost > 0.0)
            .max_by(|a, b| a.1.total_cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
            .map(|(name, _)| name.as_str())
    }
}

/// Which series color each model wears, so the Models table, the stacked bars
/// and the legend agree.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Slots {
    /// Models by cost, the biggest first: the stacking order, bottom up.
    pub order: Vec<String>,
    slots: BTreeMap<String, usize>,
}

/// Slots reserved for each model family, two apiece: a family keeps its hue
/// and its versions tell apart by the second slot.
const FAMILY_SLOTS: [(&str, usize); 3] = [("opus", 0), ("sonnet", 2), ("haiku", 4)];
/// First slot for a model of no known family, and how many slots that pool has.
const OTHER_BASE: usize = 6;
const OTHER_POOL: usize = SERIES - OTHER_BASE;
const FAMILY_WIDTH: usize = 2;

/// The slot of a model, from its name alone. It never depends on the other
/// models in view, so a model keeps its color across ranges and panels.
fn slot_for(name: &str) -> usize {
    let digits: usize = name
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(|b| usize::from(b - b'0'))
        .sum();
    match FAMILY_SLOTS
        .iter()
        .find(|(family, _)| name.contains(family))
    {
        Some((_, base)) => base + digits % FAMILY_WIDTH,
        None => {
            let hash = name.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
                (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
            });
            OTHER_BASE + (hash % OTHER_POOL as u64) as usize
        }
    }
}

impl Slots {
    /// Gives every model the slot `slot_for` picks from its name. Two models
    /// can share a slot when their names land on the same one. `earlier` holds
    /// the models of the previous period: they get a color for the table but
    /// are not part of the stacking order.
    pub fn of(models: &[Group], earlier: &[Group]) -> Slots {
        let slots = models
            .iter()
            .chain(earlier)
            .map(|g| (g.key.clone(), slot_for(&g.key)))
            .collect();
        let mut order: Vec<&Group> = models.iter().collect();
        order.sort_by(|a, b| {
            b.cost_usd
                .total_cmp(&a.cost_usd)
                .then_with(|| a.key.cmp(&b.key))
        });
        Slots {
            order: order.into_iter().map(|g| g.key.clone()).collect(),
            slots,
        }
    }

    /// The slot of a model, 0 for one that is not in the range.
    pub fn slot(&self, model: &str) -> usize {
        self.slots.get(model).copied().unwrap_or(0)
    }
}

/// What one day adds up to while the series is built.
#[derive(Default)]
struct DayAcc {
    cost_usd: f64,
    tokens: u64,
    messages: usize,
    models: BTreeMap<String, f64>,
}

/// The Budget panel: the known limits and the burn rate of the period on
/// screen. Account wide: the project filter does not apply.
#[derive(Debug, Clone, PartialEq)]
pub struct LimitsView {
    /// Worst first (reached, then will exceed), then measured before manual.
    pub limits: Vec<Limit>,
    /// The plan limits were read more than six hours ago.
    pub stale: bool,
    /// Seconds since Claude Code cached the numbers a cached limit uses.
    pub cache_age: Option<i64>,
    /// What Claude Code last reported as used this month, as `(read at, USD)`.
    pub reported: Option<(i64, f64)>,
    /// The cached monthly bar is the stale amount Claude Code read plus this
    /// machine's messages since, not this machine alone.
    pub cached_plus_local: bool,
    /// Other machines have synced events, so a manual budget counts them too.
    pub all_machines: bool,
    /// The manual and the detected monthly budget in dollars, when both are
    /// set and differ. The detected one applies.
    pub manual_beside_detected: Option<(f64, f64)>,
    /// Why the 5 hour and weekly windows are missing, when they are.
    pub windows_note: Option<&'static str>,
    pub period: Period,
    pub burn: Burn,
    pub previous: Burn,
    /// Spend of the last 24 hours, all projects, for the recent pace.
    pub recent_cost_24h: f64,
    /// The burn-up of each limit, in the order of `limits`.
    pub burnups: Vec<Option<Burnup>>,
    /// This month's local spend with a projection and no limit, for the
    /// Burn-up panel when no limit is known.
    pub spend_only: Option<Burnup>,
}

impl LimitsView {
    /// The burn-up the panel draws: the limit that is the most used, or the
    /// local spend when no limit is known.
    pub fn charted(&self) -> Option<&Burnup> {
        self.burnups
            .iter()
            .flatten()
            .filter(|b| b.limit.is_some())
            .max_by(|a, b| {
                let share = |x: &Burnup| x.percent_of_limit(x.total).unwrap_or(0.0);
                share(a).total_cmp(&share(b))
            })
            .or(self.spend_only.as_ref())
    }
}

impl Default for LimitsView {
    fn default() -> Self {
        LimitsView {
            limits: Vec::new(),
            stale: false,
            cache_age: None,
            reported: None,
            cached_plus_local: false,
            all_machines: false,
            manual_beside_detected: None,
            windows_note: None,
            period: Period::Week(WeekStart::default()),
            burn: Burn::new(0.0, 0),
            previous: Burn::new(0.0, 0),
            recent_cost_24h: 0.0,
            burnups: Vec::new(),
            spend_only: None,
        }
    }
}

fn severity(l: &Limit, now: i64) -> u8 {
    match l.headroom(now).map(|h| h.state) {
        Some(State::Exceeded) => 0,
        Some(State::WillExceed { .. }) => 1,
        Some(State::OnTrack) => 2,
        _ => 3,
    }
}

impl LimitsView {
    /// `events` must reach back to `limits_source::events_since`. The period
    /// is the one the range is, or this week for the other ranges.
    pub fn build(
        sources: &Sources,
        events: &[UsageEvent],
        now: i64,
        week: WeekStart,
        range: Range,
    ) -> LimitsView {
        let mut limits = limits_source::all_limits(sources, events, now, week);
        limits.sort_by_key(|l| (severity(l, now), l.origin == Origin::Manual));
        let windows_note = limits_source::windows_note(sources, &limits);
        let cache_age = sources
            .account
            .cache
            .as_ref()
            .filter(|_| limits.iter().any(|l| l.origin == Origin::Cached))
            .map(|c| c.age(now));
        let period = Period::of_range(range).unwrap_or(Period::Week(week));
        let (burn, previous) = limits_source::burn_pair(events, period, now);
        let burnups = burnups_for(sources, &limits, events, now);
        let spend_only = spend_only(
            events,
            LimitPeriod::Monthly.containing(now, week),
            crate::tz::current(),
            now,
        )
        .map(|b| b.with_rate_limits(&sources.rate_limited));
        LimitsView {
            limits,
            burnups,
            spend_only,
            stale: sources.stale(now),
            cache_age,
            reported: limits_source::billed_anchor(sources, now),
            cached_plus_local: limits_source::reported_plus_local(sources, now),
            all_machines: false,
            manual_beside_detected: limits_source::manual_beside_detected(sources, now),
            windows_note,
            period,
            burn,
            previous,
            recent_cost_24h: events
                .iter()
                .filter(|e| {
                    e.timestamp > now - super::projection::RECENT_SECONDS && e.timestamp <= now
                })
                .map(|e| e.cost_usd)
                .sum::<f64>()
                + 0.0,
        }
    }
}

/// What the whole store holds, whatever the range: how many messages and the
/// first and last timestamps. It lets an empty range say where the data is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StoreSpan {
    pub events: i64,
    pub first: Option<i64>,
    pub last: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct Data {
    pub now: i64,
    pub range: Range,
    /// The time zone the days were grouped in.
    pub zone: crate::tz::Zone,
    pub filter: String,
    pub totals: Totals,
    /// Per day, oldest first. Idle days are absent.
    pub days: Vec<Group>,
    pub models: Vec<Group>,
    /// The models of the previous span of a period, empty for other ranges.
    pub models_before: Vec<Group>,
    /// The series color of each model.
    pub slots: Slots,
    pub repos: Vec<Group>,
    /// Sessions in the range, one row per session and day.
    pub sessions: Vec<SessionDay>,
    /// Messages in the range whose model has no price (counted as $0).
    pub unpriced: usize,
    /// Every day from the start of the range to today, idle days at zero.
    pub series: Vec<DayPoint>,
    /// The last `HOURS` hours of the zone's clock, the current hour last, idle
    /// hours at zero. A point's `day` holds the second its hour starts at.
    pub hours: Vec<DayPoint>,
    pub live: LiveView,
    /// Today, the week and the month so far against the previous period over
    /// the same elapsed span. `None` for the other ranges.
    pub comparison: Option<Comparison>,
    /// Cost per agent from telemetry. Empty until `load` reads the store, and
    /// for a range with no telemetry.
    pub agents: query::AgentView,
    /// Cost per skill from telemetry, same rules as `agents`.
    pub skills: query::AgentView,
    /// Whether telemetry has any request in the range, skill or not.
    pub telemetry_rows: bool,
    /// Tool latency, errors, retries and busy time from telemetry.
    pub health: query::HealthView,
    /// Limits and burn rate. Empty (limit unknown) until `load_limits` reads
    /// them.
    pub limits: LimitsView,
    /// The whole store, set by `load`.
    pub store: StoreSpan,
    /// The account shown and the accounts to pick from, set by `load`.
    pub accounts: AccountsView,
    /// The machine shown and the machines to pick from, set by `load`.
    pub machines: MachinesView,
}

/// Days charted for today, since a single bar says nothing.
const TREND_DAYS: i64 = 7;

/// Hours the Hourly panel charts: a rolling day ending with the current hour.
pub const HOURS: usize = 24;
const SECONDS_PER_HOUR: i64 = 3600;

/// One point per hour of the last `HOURS`, from the events that match
/// `filter`. An hour starts on the whole hour of the chosen zone, so a zone
/// with a half hour offset still reads hours on its own clock.
fn hour_series(
    window: &[UsageEvent],
    now: i64,
    filter: &str,
    zone: crate::tz::Zone,
) -> Vec<DayPoint> {
    let offset = zone.offset_at(now);
    let current = now - (now + offset).rem_euclid(SECONDS_PER_HOUR);
    let first = current - (HOURS as i64 - 1) * SECONDS_PER_HOUR;
    let mut acc: Vec<DayAcc> = (0..HOURS).map(|_| DayAcc::default()).collect();
    for e in window {
        if e.timestamp < first || e.timestamp > now || !matches(e, filter) {
            continue;
        }
        let at = ((e.timestamp - first) / SECONDS_PER_HOUR) as usize;
        let slot = &mut acc[at.min(HOURS - 1)];
        slot.cost_usd += e.cost_usd;
        slot.tokens += query::total_tokens(e);
        slot.messages += 1;
        *slot.models.entry(e.model.clone()).or_default() += e.cost_usd;
    }
    acc.into_iter()
        .enumerate()
        .map(|(i, a)| DayPoint {
            day: first + i as i64 * SECONDS_PER_HOUR,
            cost_usd: a.cost_usd,
            tokens: a.tokens,
            messages: a.messages,
            models: a.models.into_iter().collect(),
        })
        .collect()
}

/// One point per day from the range start (or the first event, for all time)
/// to the end of the range (today for all time), so the graph axis is a real calendar.
fn day_series(events: &[UsageEvent], now: i64, range: Range) -> Vec<DayPoint> {
    let today = crate::tz::day_of(now);
    let mut per_day: BTreeMap<i64, DayAcc> = BTreeMap::new();
    for e in events {
        let slot = per_day.entry(query::day_of(e.timestamp)).or_default();
        slot.cost_usd += e.cost_usd;
        slot.tokens += query::total_tokens(e);
        slot.messages += 1;
        *slot.models.entry(e.model.clone()).or_default() += e.cost_usd;
    }
    // The series covers the whole range, so a week or month in progress shows
    // its days to come as empty bars. Today charts the last seven days. All
    // time stops at today.
    let (first, last) = match range.days(now) {
        Some((first, _)) if range == Range::Today => (first - TREND_DAYS + 1, first),
        Some((first, past)) => (first, (past - 1).max(first)),
        None => match per_day.keys().next() {
            Some(day) => (*day, today.max(*day)),
            None => return Vec::new(),
        },
    };
    (first..=last)
        .map(|day| match per_day.remove(&day) {
            Some(acc) => DayPoint {
                day,
                cost_usd: acc.cost_usd,
                tokens: acc.tokens,
                messages: acc.messages,
                models: acc.models.into_iter().collect(),
            },
            None => DayPoint {
                day,
                cost_usd: 0.0,
                tokens: 0,
                messages: 0,
                models: Vec::new(),
            },
        })
        .collect()
}

/// Whether an event matches the filter text: a case insensitive substring of
/// its model, repo, branch or session id. Empty text matches everything.
pub fn matches(event: &UsageEvent, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let needle = filter.to_lowercase();
    [&event.model, &event.repo, &event.branch, &event.session_id]
        .iter()
        .any(|field| field.to_lowercase().contains(&needle))
}

impl Data {
    /// Builds the snapshot from the events of the range. `window` is every
    /// event the range could contain (events outside it are dropped here),
    /// and for a period also the previous one and, for today, the days of the
    /// trend.
    pub fn build(
        window: &[UsageEvent],
        live: LiveView,
        now: i64,
        range: Range,
        filter: &str,
    ) -> Data {
        // Read first: a zone switched while this runs makes the snapshot stale.
        let zone = crate::tz::current();
        let (inside, _) = query::in_range(window, &[], now, range);
        let inside: Vec<UsageEvent> = inside.into_iter().filter(|e| matches(e, filter)).collect();
        let models = group_usage(&inside, Dimension::Model);
        let (comparison, models_before) = match Period::of_range(range) {
            Some(period) => {
                let windows = period.windows(now);
                let (current, previous) = windows.split(window);
                let keep = |events: Vec<UsageEvent>| -> Vec<UsageEvent> {
                    events.into_iter().filter(|e| matches(e, filter)).collect()
                };
                let (current, previous) = (keep(current), keep(previous));
                let before = group_usage(&previous, Dimension::Model);
                let compared = Comparison::new(period, windows, &current, &previous);
                (Some(compared), before)
            }
            None => (None, Vec::new()),
        };
        let series = match range {
            Range::Today => {
                let first = crate::tz::day_start(crate::tz::day_of(now) - TREND_DAYS + 1);
                let trend: Vec<UsageEvent> = window
                    .iter()
                    .filter(|e| e.timestamp >= first && matches(e, filter))
                    .cloned()
                    .collect();
                day_series(&trend, now, range)
            }
            _ => day_series(&inside, now, range),
        };
        Data {
            now,
            range,
            zone,
            filter: filter.to_string(),
            totals: Totals::of(&inside),
            days: group_usage(&inside, Dimension::Day),
            slots: Slots::of(&models, &models_before),
            models,
            models_before,
            repos: group_usage(&inside, Dimension::Repo),
            sessions: query::session_days(&inside),
            unpriced: unpriced_models(&inside).0,
            series,
            hours: hour_series(window, now, filter, zone),
            live,
            comparison,
            agents: query::AgentView::default(),
            skills: query::AgentView::default(),
            telemetry_rows: false,
            health: query::HealthView::default(),
            limits: LimitsView::default(),
            store: StoreSpan::default(),
            accounts: AccountsView::default(),
            machines: MachinesView::default(),
        }
    }
}

/// The oldest timestamp the snapshot needs, or `None` for every event: the
/// start of the range, the previous span of a period, and for today the days
/// of the trend, whichever is earliest.
fn window_start(range: Range, now: i64) -> Option<i64> {
    let (first_day, _) = range.days(now)?;
    let mut start = crate::tz::day_start(first_day);
    if let Some(period) = Period::of_range(range) {
        start = start.min(period.windows(now).previous.start);
    }
    if range == Range::Today {
        start = start.min(crate::tz::day_start(first_day - TREND_DAYS + 1));
    }
    // The Hourly panel charts the last day whatever the range.
    Some(start.min(now - HOURS as i64 * SECONDS_PER_HOUR))
}

impl Data {
    /// Narrows the tables of the machine panels to `events`, the window of
    /// the machine shown. The account panels keep every machine's rows.
    fn scope_machine_tables(&mut self, events: &[UsageEvent]) {
        let (inside, _) = query::in_range(events, &[], self.now, self.range);
        let inside: Vec<UsageEvent> = inside
            .into_iter()
            .filter(|e| matches(e, &self.filter))
            .collect();
        self.repos = group_usage(&inside, Dimension::Repo);
        self.sessions = query::session_days(&inside);
    }
}

/// Reads the store and builds the snapshot. Budget, projection, burnup, spend,
/// tokens and models count every machine of the account. Live, agents, skills,
/// tools, health, sessions and projects follow the machine.
pub fn load(
    conn: &Connection,
    range: Range,
    filter: &str,
    sel: &AccountSel,
    machine_sel: &MachineSel,
    now: i64,
) -> Result<Data, String> {
    let accounts = account::view(conn, sel)?;
    let machines = machine::view(conn, machine_sel)?;
    let machine = machines.key.as_deref();
    let mut window = match window_start(range, now) {
        Some(since) => db::load_account_events_since(conn, since)?,
        None => db::load_account_events(conn)?,
    };
    account::keep(&mut window, accounts.key.as_deref());
    let live = query::load_live(conn, now, machine)?;
    let mut data = Data::build(&window, live, now, range, filter);
    if machine.is_some() {
        machine::keep(&mut window, machine);
        data.scope_machine_tables(&window);
    }
    data.agents = query::load_agents(conn, range, now, machine)?;
    data.skills = query::load_skills(conn, range, now, machine)?;
    data.health = query::load_health(conn, range, now, machine)?;
    data.telemetry_rows =
        (!data.agents.is_empty() && !data.agents.from_transcripts) || !data.health.now.is_empty();
    data.store = crate::db::usage_span(conn)?;
    data.accounts = accounts;
    data.machines = machines;
    Ok(data)
}

/// Reads the plan limits the statusline kept and the manual budgets, and the
/// events the burn rate needs.
pub fn load_limits(
    conn: &Connection,
    paths: &Paths,
    range: Range,
    account: Option<&str>,
    now: i64,
) -> Result<LimitsView, String> {
    let week = crate::entry::configured_week_start();
    let since = limits_source::events_since(now, week);
    let mut events = db::load_account_events_since(conn, since)?;
    account::keep(&mut events, account);
    let sources = crate::run::limit_sources(paths, conn, since, now);
    let mut view = LimitsView::build(&sources, &events, now, week, range);
    view.all_machines = crate::sync_tables::has_remote_rows(conn);
    Ok(view)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_788_400_000;

    fn event(ts: i64, model: &str, repo: &str, cost: f64) -> UsageEvent {
        UsageEvent {
            event_id: format!("{model}{repo}{ts}"),
            timestamp: ts,
            session_id: "s1".into(),
            model: model.into(),
            repo: repo.into(),
            branch: "main".into(),
            input_tokens: 10,
            output_tokens: 5,
            cost_usd: cost,
            ..UsageEvent::default()
        }
    }

    /// Two accounts: `work` is the newer, `home` has the larger total.
    fn two_accounts() -> Connection {
        let conn = db::open_db(&crate::util::scratch_dir("data-accounts").join("u.db")).unwrap();
        for (ts, account, cost) in [
            (NOW - 300, "home", 4.0),
            (NOW - 200, "home", 8.0),
            (NOW - 100, "work", 2.0),
        ] {
            let mut e = event(ts, "claude-opus-5", "alpha", cost);
            e.account = account.to_string();
            e.input_tokens = 1_000_000;
            db::upsert_usage_event(&conn, &e).unwrap();
        }
        conn
    }

    #[test]
    fn totals_follow_the_account_picked_and_default_to_the_newest() {
        let conn = two_accounts();
        crate::account_store::set_name(&conn, "work", "Company").unwrap();
        let range = Range::LastDays(30);
        let shown = |sel: &AccountSel| {
            let d = load(&conn, range, "", sel, &MachineSel::default(), NOW).unwrap();
            (d.totals.messages, d.accounts.shown.clone())
        };

        assert_eq!(shown(&AccountSel::Latest), (1, "Company".to_string()));
        assert_eq!(
            shown(&AccountSel::Key("home".into())),
            (2, "home".to_string())
        );
        assert_eq!(shown(&AccountSel::All), (3, "all accounts".to_string()));
        let d = load(
            &conn,
            range,
            "",
            &AccountSel::Latest,
            &MachineSel::default(),
            NOW,
        )
        .unwrap();
        assert_eq!(d.accounts.choices.len(), 2);
        assert_eq!(d.accounts.choices[0].0, "work");
    }

    /// One event and one telemetry request per machine and account pair. The
    /// first machine is this one, the others are stamped with their own ids.
    fn fleet(machines: usize, accounts: usize) -> Connection {
        let dir = crate::util::scratch_dir(&format!("data-fleet-{machines}-{accounts}"));
        let conn = db::open_db(&dir.join("u.db")).unwrap();
        for m in 0..machines {
            for a in 0..accounts {
                let id = format!("m{m}a{a}");
                let mut e = event(NOW - 100 - (m * 10 + a) as i64, "claude-opus-5", &id, 1.0);
                e.event_id = id.clone();
                e.session_id = id.clone();
                e.account = format!("acct{a}");
                db::upsert_usage_event(&conn, &e).unwrap();
                let request = crate::telemetry_db::TelemetryRequest {
                    session_id: id.clone(),
                    request_id: id.clone(),
                    timestamp: NOW - 100,
                    agent_name: format!("agent-m{m}"),
                    ..Default::default()
                };
                crate::telemetry_db::insert_request(&conn, &request).unwrap();
                if m > 0 {
                    for table in ["usage_events", "telemetry_requests"] {
                        let key = if table == "usage_events" {
                            "event_id"
                        } else {
                            "request_id"
                        };
                        conn.execute(
                            &format!("UPDATE {table} SET machine = ?1 WHERE {key} = ?2"),
                            [&format!("other{m}"), &id],
                        )
                        .unwrap();
                    }
                }
            }
        }
        conn
    }

    #[test]
    fn account_panels_sum_every_machine_and_machine_panels_filter() {
        let range = Range::LastDays(30);
        for machines in 0..=2usize {
            for accounts in 0..=2usize {
                // Arrange
                let conn = fleet(machines, accounts);
                let one_account = AccountSel::Key("acct0".into());
                let events_of_acct0 = if accounts > 0 { machines } else { 0 };
                let on_this_machine = usize::from(machines > 0 && accounts > 0);
                let agents_here = usize::from(machines > 0 && accounts > 0);

                // Act
                let here = load(&conn, range, "", &one_account, &MachineSel::This, NOW).unwrap();
                let everywhere =
                    load(&conn, range, "", &one_account, &MachineSel::All, NOW).unwrap();

                // Assert
                let case = format!("{machines} machines, {accounts} accounts");
                for d in [&here, &everywhere] {
                    assert_eq!(d.totals.messages, events_of_acct0, "totals, {case}");
                    assert_eq!(
                        d.models.iter().map(|g| g.messages).sum::<u64>() as usize,
                        events_of_acct0,
                        "models, {case}"
                    );
                }
                assert_eq!(here.sessions.len(), on_this_machine, "sessions, {case}");
                assert_eq!(here.repos.len(), on_this_machine, "projects, {case}");
                assert_eq!(here.agents.rows.len(), agents_here, "agents, {case}");
                assert_eq!(
                    everywhere.sessions.len(),
                    events_of_acct0,
                    "sessions, {case}"
                );
                assert_eq!(
                    everywhere.agents.rows.len(),
                    if accounts > 0 { machines } else { 0 },
                    "agents, {case}"
                );
            }
        }
    }

    #[test]
    fn the_account_panels_sum_a_synced_row_of_another_machine() {
        // Arrange
        let conn = fleet(1, 1);
        conn.execute(
            "INSERT INTO usage_events_remote (machine, event_id, timestamp, account, model,
                 effort, input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
                 cost_usd, pushed_at)
             VALUES ('far', 'synced1', ?1, 'hash', 'claude-opus-5', '', 10, 5, 0, 0, 1.0, 1)",
            [NOW - 50],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO account_aliases (hash, key) VALUES ('hash', 'acct0')",
            [],
        )
        .unwrap();
        let paths = crate::util::scratch_paths("data-synced-limits");

        // Act
        let d = load(
            &conn,
            Range::LastDays(30),
            "",
            &AccountSel::Key("acct0".into()),
            &MachineSel::This,
            NOW,
        )
        .unwrap();
        let limits = load_limits(&conn, &paths, Range::LastDays(30), Some("acct0"), NOW).unwrap();

        // Assert
        assert_eq!(d.totals.messages, 2);
        assert!(limits.all_machines);
        assert!(limits.recent_cost_24h > 0.0);
    }

    #[test]
    fn the_limits_burn_uses_account_scope_and_counts_every_machine() {
        // Arrange
        let recent = |machines: usize| {
            let conn = fleet(machines, 1);
            let paths = crate::util::scratch_paths(&format!("data-limits-fleet-{machines}"));
            load_limits(&conn, &paths, Range::LastDays(30), Some("acct0"), NOW)
                .unwrap()
                .recent_cost_24h
        };

        // Act
        let (this_machine_only, both_machines) = (recent(1), recent(2));

        // Assert
        assert!(this_machine_only > 0.0, "{this_machine_only}");
        assert!(
            (both_machines - 2.0 * this_machine_only).abs() < 1e-9,
            "{this_machine_only} {both_machines}"
        );
    }

    #[test]
    fn one_other_machine_can_be_picked_by_its_id() {
        let conn = fleet(2, 1);
        let d = load(
            &conn,
            Range::LastDays(30),
            "",
            &AccountSel::All,
            &MachineSel::Key("other1".into()),
            NOW,
        )
        .unwrap();

        assert_eq!(d.sessions.len(), 1);
        assert_eq!(d.agents.rows[0].key, "agent-m1");
        assert_eq!(d.machines.choices.len(), 2);
        assert_eq!(d.machines.shown, "other1");
    }

    #[test]
    fn the_burn_rate_uses_only_the_picked_account() {
        let conn = two_accounts();
        let paths = crate::util::scratch_paths("data-accounts-limits");
        let recent = |account: Option<&str>| {
            load_limits(&conn, &paths, Range::LastDays(30), account, NOW)
                .unwrap()
                .recent_cost_24h
        };

        let (work, home, all) = (recent(Some("work")), recent(Some("home")), recent(None));
        assert!(work > 0.0 && home > work, "{work} {home}");
        assert!((all - (work + home)).abs() < 1e-9, "{work} {home} {all}");
    }

    #[test]
    fn the_filter_matches_model_repo_branch_and_session_case_insensitively() {
        let e = event(NOW, "claude-Opus", "playbook", 1.0);
        for hit in ["opus", "PLAYBOOK", "main", "s1", ""] {
            assert!(matches(&e, hit), "{hit}");
        }
        assert!(!matches(&e, "sonnet"));
    }

    #[test]
    fn the_recent_cost_counts_the_last_24_hours_and_stops_at_now() {
        let events = vec![
            event(NOW - 86_400, "opus", "a", 1.0),
            event(NOW - 86_399, "opus", "a", 2.0),
            event(NOW, "opus", "a", 4.0),
            event(NOW + 1, "opus", "a", 8.0),
        ];

        let view = LimitsView::build(
            &Sources::default(),
            &events,
            NOW,
            WeekStart::default(),
            Range::Today,
        );

        assert_eq!(view.recent_cost_24h, 6.0);
    }

    #[test]
    fn build_groups_the_range_and_applies_the_filter() {
        let events = vec![
            event(NOW - 60, "opus", "a", 2.0),
            event(NOW - 30, "sonnet", "b", 1.0),
            event(NOW - 100 * 86_400, "opus", "a", 50.0),
        ];
        let all = Data::build(&events, LiveView::default(), NOW, Range::LastDays(30), "");
        assert_eq!(all.totals.messages, 2);
        assert!((all.totals.cost_usd - 3.0).abs() < 1e-9);
        assert_eq!(all.models.len(), 2);
        assert_eq!(all.repos.len(), 2);

        let opus = Data::build(
            &events,
            LiveView::default(),
            NOW,
            Range::LastDays(30),
            "opus",
        );
        assert_eq!(opus.totals.messages, 1);
        assert_eq!(opus.models[0].key, "opus");
    }

    // 2026-09-02 23:46:40 UTC is already 09-03 on a clock five hours east.
    #[test]
    fn an_event_near_midnight_lands_on_the_day_of_the_chosen_zone() {
        use crate::tz::{self, Zone};
        let events = vec![event(NOW - 7_200, "opus", "a", 1.0)];
        let first_day = |zone: Zone| {
            tz::set(zone);
            let series = day_series(&events, NOW, Range::All);
            crate::aggregate::date_key(series[0].day)
        };

        let utc = first_day(Zone::Utc);
        let east = first_day(Zone::Fixed(5 * 3600));
        let back = first_day(Zone::Utc);

        assert_eq!(utc, "2026-09-02");
        assert_eq!(east, "2026-09-03");
        assert_eq!(
            back, "2026-09-02",
            "switching back regroups the same events"
        );
    }

    #[test]
    fn a_snapshot_remembers_the_zone_its_days_were_grouped_in() {
        use crate::tz::{self, Zone};
        let events = vec![event(NOW - 60, "opus", "a", 1.0)];

        tz::set(Zone::Fixed(3600));
        let east = Data::build(&events, LiveView::default(), NOW, Range::All, "");
        tz::set(Zone::Utc);
        let utc = Data::build(&events, LiveView::default(), NOW, Range::All, "");

        assert_eq!(east.zone, Zone::Fixed(3600));
        assert_eq!(utc.zone, Zone::Utc);
    }

    #[test]
    fn the_series_fills_idle_days_up_to_today() {
        let events = vec![
            event(NOW - 2 * 86_400, "opus", "a", 2.0),
            event(NOW, "opus", "a", 1.0),
        ];
        let d = Data::build(&events, LiveView::default(), NOW, Range::LastDays(5), "");
        assert_eq!(d.series.len(), 5);
        let costs: Vec<f64> = d.series.iter().map(|p| p.cost_usd).collect();
        assert_eq!(costs, vec![0.0, 0.0, 2.0, 0.0, 1.0]);
        let all = Data::build(&events, LiveView::default(), NOW, Range::All, "");
        assert_eq!(all.series.len(), 3);
        let none = Data::build(&[], LiveView::default(), NOW, Range::All, "");
        assert!(none.series.is_empty());
    }

    #[test]
    fn sessions_are_grouped_by_day_inside_the_range_and_follow_the_filter() {
        let mut other = event(NOW - 30, "sonnet", "b", 1.0);
        other.session_id = "s2".into();
        let events = vec![
            event(NOW - 60, "opus", "a", 2.0),
            other,
            event(NOW - 100 * 86_400, "opus", "a", 50.0),
        ];
        let d = Data::build(&events, LiveView::default(), NOW, Range::LastDays(30), "");
        assert_eq!(d.sessions.len(), 2);
        assert_eq!(d.sessions[0].id, "s1", "the dearer session first");
        let sonnet = Data::build(
            &events,
            LiveView::default(),
            NOW,
            Range::LastDays(30),
            "sonnet",
        );
        assert_eq!(sonnet.sessions.len(), 1);
        assert_eq!(sonnet.sessions[0].id, "s2");
    }

    // Thursday 2026-09-03 01:46:40 UTC.
    const DAY: i64 = 86_400;
    const WEEK: Range = Range::ThisWeek(crate::query::WeekStart::Monday);

    #[test]
    fn a_period_range_carries_the_comparison_and_the_others_do_not() {
        let events = vec![
            event(NOW - 60, "opus", "a", 2.0),
            event(NOW - 30, "sonnet", "b", 1.0),
            event(NOW - DAY - 60, "opus", "a", 4.0),
            // Yesterday after the same time of day is outside the span.
            event(NOW - DAY + 600, "opus", "a", 8.0),
        ];
        let build =
            |range, filter: &str| Data::build(&events, LiveView::default(), NOW, range, filter);
        let c = build(Range::Today, "").comparison.expect("today compares");
        assert_eq!((c.current.messages, c.previous.messages), (2, 1));
        assert!((c.cost().amount - -1.0).abs() < 1e-9);
        // The filter narrows both spans.
        let opus = build(Range::Today, "opus").comparison.unwrap();
        assert_eq!((opus.current.messages, opus.previous.messages), (1, 1));
        for range in [
            Range::LastWeek(crate::query::WeekStart::Monday),
            Range::LastDays(30),
            Range::All,
        ] {
            assert!(build(range, "").comparison.is_none(), "{range:?}");
        }
        assert!(build(WEEK, "").comparison.is_some());
        assert!(build(Range::CurrentMonth, "").comparison.is_some());
    }

    #[test]
    fn today_charts_the_last_seven_days() {
        let events = vec![
            event(NOW - 2 * DAY, "opus", "a", 2.0),
            event(NOW, "opus", "a", 1.0),
            event(NOW - 9 * DAY, "opus", "a", 9.0),
        ];
        let d = Data::build(&events, LiveView::default(), NOW, Range::Today, "");
        let costs: Vec<f64> = d.series.iter().map(|p| p.cost_usd).collect();
        assert_eq!(costs, [0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 1.0]);
        // The totals are today's alone.
        assert_eq!(d.totals.messages, 1);
    }

    #[test]
    fn the_store_is_read_back_to_the_start_of_the_previous_period() {
        let today = NOW.div_euclid(DAY);
        // Seven trend days for today, last week's start for this week.
        assert_eq!(window_start(Range::Today, NOW), Some((today - 6) * DAY));
        assert_eq!(window_start(WEEK, NOW), Some((today - 10) * DAY));
        // 2026-08-01.
        assert_eq!(window_start(Range::CurrentMonth, NOW), Some(1_785_542_400));
        // The other ranges read what they show, and all time reads everything.
        assert_eq!(
            window_start(Range::LastDays(30), NOW),
            Some((today - 29) * DAY)
        );
        assert_eq!(window_start(Range::All, NOW), None);
    }

    #[test]
    fn unpriced_messages_are_counted() {
        let mut e = event(NOW, "mystery", "a", 0.0);
        e.unpriced = true;
        let d = Data::build(&[e], LiveView::default(), NOW, Range::All, "");
        assert_eq!(d.unpriced, 1);
    }

    fn group(key: &str, cost: f64) -> Group {
        Group {
            key: key.into(),
            cost_usd: cost,
            ..Group::default()
        }
    }

    #[test]
    fn a_model_keeps_its_color_whatever_else_is_in_view() {
        let models = [
            group("claude-sonnet-5", 5.0),
            group("claude-opus-5", 9.0),
            group("claude-haiku-4-5", 1.0),
        ];
        let slots = Slots::of(&models, &[]);
        assert_eq!(
            slots.order,
            ["claude-opus-5", "claude-sonnet-5", "claude-haiku-4-5"]
        );
        let mut seen: Vec<usize> = models.iter().map(|g| slots.slot(&g.key)).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 3, "three families, three colors");
        // Neither the arrival order nor the other models change a color.
        let mut reversed = models.to_vec();
        reversed.reverse();
        let again = Slots::of(&reversed, &[]);
        let extra = [group("claude-opus-4-6", 2.0), group("gpt-5-codex", 1.0)];
        let crowded = Slots::of(&extra, &models);
        for g in &models {
            assert_eq!(slots.slot(&g.key), again.slot(&g.key));
            assert_eq!(slots.slot(&g.key), crowded.slot(&g.key));
            assert_eq!(
                slots.slot(&g.key),
                Slots::of(std::slice::from_ref(g), &[]).slot(&g.key)
            );
        }
    }

    #[test]
    fn versions_of_a_family_stay_in_its_slots_and_other_models_use_the_rest() {
        let opus_5 = slot_for("claude-opus-5");
        let opus_46 = slot_for("claude-opus-4-6");
        assert!(opus_5 < 2 && opus_46 < 2, "opus keeps slots 0 and 1");
        assert_ne!(opus_5, opus_46, "adjacent versions tell apart");
        assert!((2..4).contains(&slot_for("claude-sonnet-5")));
        assert!((4..6).contains(&slot_for("claude-haiku-4-5")));
        assert!((OTHER_BASE..SERIES).contains(&slot_for("gpt-5-codex")));
        assert_eq!(Slots::default().slot("not-in-the-range"), 0);
        assert!(Slots::of(&[], &[]).order.is_empty());
    }

    #[test]
    fn a_day_names_its_top_model_or_none_when_it_cost_nothing() {
        let day = |models: &[(&str, f64)]| DayPoint {
            day: 1,
            cost_usd: models.iter().map(|m| m.1).sum(),
            tokens: 0,
            messages: 1,
            models: models.iter().map(|(n, c)| (n.to_string(), *c)).collect(),
        };
        assert_eq!(day(&[("a", 1.0), ("b", 3.0)]).top_model(), Some("b"));
        assert_eq!(
            day(&[("a", 2.0), ("b", 2.0)]).top_model(),
            Some("a"),
            "a tie goes to the name"
        );
        assert_eq!(day(&[("a", 0.0)]).top_model(), None);
        assert_eq!(day(&[]).top_model(), None);
    }

    #[test]
    fn the_series_keeps_the_cost_of_each_model_per_day() {
        let events = vec![
            event(NOW, "opus", "a", 2.0),
            event(NOW - 10, "sonnet", "a", 1.0),
            event(NOW - 20, "opus", "a", 0.5),
        ];
        let d = Data::build(&events, LiveView::default(), NOW, Range::Today, "");
        let today = d.series.last().expect("today is charted");
        assert_eq!(today.messages, 3);
        assert_eq!(
            today.models,
            vec![("opus".to_string(), 2.5), ("sonnet".to_string(), 1.0)]
        );
        assert_eq!(d.slots.order, ["opus", "sonnet"]);
    }

    #[test]
    fn the_hours_are_the_last_24_of_the_clock_with_the_current_hour_last() {
        // Arrange
        let events = vec![
            event(NOW, "opus", "a", 4.0),
            event(NOW - 3_600, "sonnet", "a", 2.0),
            event(NOW - 23 * 3_600, "opus", "a", 1.0),
            event(NOW - 25 * 3_600, "opus", "a", 100.0),
            event(NOW + 1, "opus", "a", 100.0),
        ];

        // Act
        let hours = hour_series(&events, NOW, "", crate::tz::Zone::Utc);

        // Assert
        assert_eq!(hours.len(), HOURS);
        let current = NOW - NOW % 3_600;
        assert_eq!(hours[HOURS - 1].day, current);
        assert_eq!(hours[0].day, current - 23 * 3_600);
        assert!(hours.windows(2).all(|w| w[1].day - w[0].day == 3_600));
        let costs: Vec<f64> = hours.iter().map(|h| h.cost_usd).collect();
        assert_eq!(costs.iter().sum::<f64>(), 4.0 + 2.0 + 1.0);
        assert_eq!(costs[HOURS - 1], 4.0);
        assert_eq!(hours[HOURS - 1].messages, 1);
    }

    #[test]
    fn an_hour_starts_on_the_whole_hour_of_a_zone_with_a_half_hour_offset() {
        // Arrange
        let india = crate::tz::Zone::Fixed(5 * 3_600 + 1_800);
        let events = vec![event(NOW, "opus", "a", 1.0)];

        // Act
        let hours = hour_series(&events, NOW, "", india);

        // Assert
        let start = hours[HOURS - 1].day;
        assert_eq!((start + 5 * 3_600 + 1_800) % 3_600, 0);
        assert!(start <= NOW && NOW < start + 3_600);
        assert_eq!(hours[HOURS - 1].cost_usd, 1.0);
    }

    #[test]
    fn the_hours_follow_the_filter_and_idle_hours_are_zero() {
        // Arrange
        let events = vec![
            event(NOW, "opus", "keep", 4.0),
            event(NOW, "opus", "drop", 8.0),
        ];

        // Act
        let hours = hour_series(&events, NOW, "keep", crate::tz::Zone::Utc);

        // Assert
        assert_eq!(hours.iter().map(|h| h.cost_usd).sum::<f64>(), 4.0);
        assert_eq!(hours.iter().filter(|h| h.messages == 0).count(), HOURS - 1);
    }

    #[test]
    fn the_hours_ignore_the_range_and_the_snapshot_carries_them() {
        // Arrange
        let events = vec![event(NOW - 3_600, "opus", "a", 2.0)];

        // Act
        let week = Data::build(&events, LiveView::default(), NOW, Range::LastDays(7), "");
        let today = Data::build(&events, LiveView::default(), NOW, Range::Today, "");

        // Assert
        assert_eq!(week.hours, today.hours);
        assert_eq!(week.hours.iter().map(|h| h.cost_usd).sum::<f64>(), 2.0);
    }

    #[test]
    fn the_window_reaches_back_a_day_for_the_hours_whatever_the_range() {
        // Arrange
        let past = Range::LastDays(1);

        // Act
        let since = window_start(past, NOW);

        // Assert
        assert!(since.is_some_and(|s| s <= NOW - HOURS as i64 * 3_600));
    }
}
