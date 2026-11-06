// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Where limits come from. Measured limits come from the `rate_limits` object
//! Claude Code passes to the statusline command on stdin (documented at
//! code.claude.com/docs/en/statusline). The statusline keeps the latest copy
//! in a small private file next to the usage database, and the usage views read
//! it back. A short history of the two window percentages sits beside it (see
//! `limits_history`). Only percentages, reset times and, behind a Claude apps gateway,
//! the dollar spend limit are kept: no account, no token, no session id. The
//! rate limit fields exist only for Pro and Max subscribers (or a gateway), so
//! on an API key there is nothing to read and the file is never written.
//!
//! Manual limits come from the `usage.budget.*` config keys.

use super::burnup::burnups_for;
use super::limits::{Burn, CrossState, Crossing, Limit, LimitPeriod, Origin, State, Unit};
use super::period::Period;
use super::plan::Account;
use super::query::{utc_month_bounds, WeekStart};
use super::spend::{BilledComparison, NotCompared, SpendFigure};
use super::UsageEvent;
use crate::util::atomic::write_atomic_private;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// The most days the plain burn-up table prints: a month is at most 31.
const BURNUP_TABLE_ROWS: usize = 31;

/// A 429 older than this no longer changes the reason or adds a summary
/// line. The chart still marks every 429 inside the period it shows.
pub const RATE_LIMIT_RECENT_SECONDS: i64 = 7 * 24 * 3600;

/// A snapshot older than this is shown as stale rather than hidden.
pub const STALE_SECONDS: i64 = 6 * 3600;

/// One usage window, as Claude Code reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub used_percentage: f64,
    pub resets_at: i64,
}

/// The gateway spend limit. The dollar fields are optional in Claude Code.
#[derive(Debug, Clone, PartialEq)]
pub struct Spend {
    pub used_percentage: f64,
    pub resets_at: i64,
    pub used_usd: Option<f64>,
    pub limit_usd: Option<f64>,
    pub period: Option<LimitPeriod>,
}

/// The latest `rate_limits` seen by the statusline.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    pub captured_at: i64,
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
    pub spend_limit: Option<Spend>,
    /// When each part was read, where that differs from `captured_at`: a
    /// reading that carries one window keeps the other as it was stored.
    pub read: ReadTimes,
}

/// When each part of a [`Snapshot`] was read, UTC seconds. `None` means
/// `captured_at`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReadTimes {
    pub five_hour: Option<i64>,
    pub seven_day: Option<i64>,
    pub spend_limit: Option<i64>,
}

fn window(v: Option<&Value>) -> Option<Window> {
    let v = v?;
    let pct = v.get("used_percentage")?.as_f64()?;
    let at = v.get("resets_at")?.as_i64()?;
    (pct.is_finite() && pct >= 0.0).then_some(Window {
        used_percentage: pct,
        resets_at: at,
    })
}

fn spend(v: Option<&Value>) -> Option<Spend> {
    let w = window(v)?;
    let v = v?;
    let usd = |k: &str| {
        v.get(k)
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite() && *n >= 0.0)
    };
    Some(Spend {
        used_percentage: w.used_percentage,
        resets_at: w.resets_at,
        used_usd: usd("used_usd"),
        limit_usd: usd("limit_usd"),
        period: v
            .get("period")
            .and_then(Value::as_str)
            .and_then(LimitPeriod::parse),
    })
}

/// The window percentages a reading changed or confirmed: what the history
/// records. `None` for a part the reading lacked or that was stale.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct Accepted {
    five_hour: Option<f64>,
    seven_day: Option<f64>,
}

/// A part of a [`Snapshot`] that has a usage figure and a reset time.
trait Part {
    fn resets_at(&self) -> i64;
    fn used(&self) -> f64;
    /// Whether this reading, of the same window as `stored`, is not behind it.
    fn is_not_behind(&self, stored: &Self) -> bool {
        self.used() >= stored.used()
    }
}

impl Part for Window {
    fn resets_at(&self) -> i64 {
        self.resets_at
    }
    fn used(&self) -> f64 {
        self.used_percentage
    }
}

impl Part for Spend {
    fn resets_at(&self) -> i64 {
        self.resets_at
    }
    fn used(&self) -> f64 {
        self.used_percentage
    }
    // Spend only grows within a period, but the percentage also moves when the
    // gateway limit changes, so the dollars decide when both readings have them.
    fn is_not_behind(&self, stored: &Self) -> bool {
        match (self.used_usd, stored.used_usd) {
            (Some(new), Some(old)) => new >= old,
            _ => self.used_percentage >= stored.used_percentage,
        }
    }
}

/// Settles one part: the part held after this reading, the time it was read
/// and whether the reading was accepted. Reset times are compared as given:
/// Claude Code passes the same epoch second for every reading of one window,
/// so no tolerance is needed.
fn settle<P: Part>(
    incoming: Option<P>,
    stored: Option<P>,
    stored_read: i64,
    read_at: i64,
    now: i64,
) -> (Option<P>, Option<i64>, bool) {
    let live = stored.filter(|s| s.resets_at() > now);
    // A reading whose window has already ended describes nothing current.
    let incoming = incoming.filter(|n| n.resets_at() > now);
    match (incoming, live) {
        (None, None) => (None, None, false),
        (None, Some(s)) => (Some(s), Some(stored_read), false),
        (Some(n), None) => (Some(n), Some(read_at), true),
        (Some(n), Some(s)) => {
            let newer = n.resets_at() > s.resets_at();
            let same_window_not_lower = n.resets_at() == s.resets_at() && n.is_not_behind(&s);
            if newer || same_window_not_lower {
                (Some(n), Some(read_at), true)
            } else {
                (Some(s), Some(stored_read), false)
            }
        }
    }
}

impl Snapshot {
    /// Reads `rate_limits` from the statusline input. `None` when the input is
    /// not JSON or has no usable window (API key accounts, or before the first
    /// response of a session).
    pub fn from_statusline(input: &str, now: i64) -> Option<Snapshot> {
        let root: Value = serde_json::from_str(input).ok()?;
        let r = root.get("rate_limits")?;
        let snap = Snapshot {
            captured_at: now,
            five_hour: window(r.get("five_hour")),
            seven_day: window(r.get("seven_day")),
            spend_limit: spend(r.get("spend_limit")),
            read: ReadTimes::default(),
        };
        (snap.five_hour.is_some() || snap.seven_day.is_some() || snap.spend_limit.is_some())
            .then_some(snap)
    }

    fn to_value(&self) -> Value {
        let w = |w: &Option<Window>| {
            w.as_ref()
                .map(|w| json!({"used_percentage": w.used_percentage, "resets_at": w.resets_at}))
        };
        json!({
            "captured_at": self.captured_at,
            "five_hour": w(&self.five_hour),
            "seven_day": w(&self.seven_day),
            "spend_limit": self.spend_limit.as_ref().map(|s| json!({
                "used_percentage": s.used_percentage,
                "resets_at": s.resets_at,
                "used_usd": s.used_usd,
                "limit_usd": s.limit_usd,
                "period": s.period.map(LimitPeriod::key),
            })),
            "read": {
                "five_hour": self.read.five_hour,
                "seven_day": self.read.seven_day,
                "spend_limit": self.read.spend_limit,
            },
        })
    }

    fn from_value(v: &Value) -> Snapshot {
        Snapshot {
            captured_at: v.get("captured_at").and_then(Value::as_i64).unwrap_or(0),
            five_hour: window(v.get("five_hour")),
            seven_day: window(v.get("seven_day")),
            spend_limit: spend(v.get("spend_limit")),
            read: ReadTimes {
                five_hour: v
                    .pointer("/read/five_hour")
                    .and_then(Value::as_i64)
                    .filter(|t| *t >= 0),
                seven_day: v
                    .pointer("/read/seven_day")
                    .and_then(Value::as_i64)
                    .filter(|t| *t >= 0),
                spend_limit: v
                    .pointer("/read/spend_limit")
                    .and_then(Value::as_i64)
                    .filter(|t| *t >= 0),
            },
        }
    }

    /// When the oldest part held was read. A snapshot is stale as soon as one
    /// part is.
    pub fn oldest_read(&self) -> i64 {
        [
            (self.five_hour.is_some(), self.read.five_hour),
            (self.seven_day.is_some(), self.read.seven_day),
            (self.spend_limit.is_some(), self.read.spend_limit),
        ]
        .into_iter()
        .filter(|(held, _)| *held)
        .map(|(_, at)| at.unwrap_or(self.captured_at))
        .min()
        .unwrap_or(self.captured_at)
    }

    pub fn is_stale(&self, now: i64) -> bool {
        now.saturating_sub(self.oldest_read()) > STALE_SECONDS
    }

    /// When the part for `period` was read: its own read time, else when the
    /// file was captured.
    pub fn read_of(&self, period: LimitPeriod) -> i64 {
        match period {
            LimitPeriod::FiveHour => self.read.five_hour,
            LimitPeriod::SevenDay => self.read.seven_day,
            _ => self.read.spend_limit,
        }
        .unwrap_or(self.captured_at)
    }

    /// This reading laid over the one stored before it, with the percentages of
    /// the windows it changed or confirmed. Every Claude Code session sends the
    /// numbers of its own last reply, so an idle session sends old ones. Within
    /// one window usage only grows, so per part (see [`Part`]): a higher or equal
    /// figure replaces the stored part and sets its read time, a lower one and
    /// an earlier reset time are stale and leave the stored part and its read
    /// time alone, a later reset time is a new window and replaces it. A part
    /// this reading lacks is kept as stored, with its own read time. A stored
    /// part whose reset time has passed is replaced or dropped, and a reading
    /// whose reset time has passed is ignored.
    fn merged_over(self, stored: Option<Snapshot>, now: i64) -> (Snapshot, Accepted) {
        let old = stored.unwrap_or_default();
        let read_at = self.captured_at;
        let stored_read = |at: Option<i64>| at.unwrap_or(old.captured_at);
        let (five_hour, five_read, took_five) = settle(
            self.five_hour,
            old.five_hour.clone(),
            stored_read(old.read.five_hour),
            read_at,
            now,
        );
        let (seven_day, seven_read, took_seven) = settle(
            self.seven_day,
            old.seven_day.clone(),
            stored_read(old.read.seven_day),
            read_at,
            now,
        );
        let (spend_limit, spend_read, _) = settle(
            self.spend_limit,
            old.spend_limit.clone(),
            stored_read(old.read.spend_limit),
            read_at,
            now,
        );
        let accepted = Accepted {
            five_hour: five_hour.as_ref().filter(|_| took_five).map(Part::used),
            seven_day: seven_day.as_ref().filter(|_| took_seven).map(Part::used),
        };
        let merged = Snapshot {
            captured_at: read_at,
            five_hour,
            seven_day,
            spend_limit,
            read: ReadTimes {
                five_hour: five_read,
                seven_day: seven_read,
                spend_limit: spend_read,
            },
        };
        (merged, accepted)
    }

    /// The measured limits that still apply at `now`. A window whose reset time
    /// has passed is dropped: Claude Code does the same, and its percentage no
    /// longer describes the current window.
    pub fn limits(&self, now: i64) -> Vec<Limit> {
        let mut out = Vec::new();
        let pct = |w: &Window, period: LimitPeriod| {
            let (start, end) = period.ending_at(w.resets_at);
            Limit {
                unit: Unit::Percent,
                period,
                origin: Origin::Measured,
                used: w.used_percentage,
                limit: 100.0,
                start,
                end,
            }
        };
        if let Some(w) = self.five_hour.as_ref().filter(|w| w.resets_at > now) {
            out.push(pct(w, LimitPeriod::FiveHour));
        }
        if let Some(w) = self.seven_day.as_ref().filter(|w| w.resets_at > now) {
            out.push(pct(w, LimitPeriod::SevenDay));
        }
        if let Some(s) = self.spend_limit.as_ref().filter(|s| s.resets_at > now) {
            match (s.used_usd, s.limit_usd, s.period) {
                (Some(used), Some(limit), Some(period)) => {
                    let (start, end) = period.ending_at(s.resets_at);
                    out.push(Limit {
                        unit: Unit::Dollars,
                        period,
                        origin: Origin::Measured,
                        used,
                        limit,
                        start,
                        end,
                    });
                }
                // The dollar fields come on a slower schedule and can be
                // absent, so fall back to the percent that always comes.
                _ => {
                    let w = Window {
                        used_percentage: s.used_percentage,
                        resets_at: s.resets_at,
                    };
                    out.push(pct(&w, s.period.unwrap_or(LimitPeriod::Monthly)));
                }
            }
        }
        out
    }
}

/// `rate_limits.json` in the usage folder.
pub fn snapshot_path(usage_dir: &Path) -> PathBuf {
    usage_dir.join("rate_limits.json")
}

/// Stores the snapshot. Best effort: the statusline must never fail on this.
pub fn save(usage_dir: &Path, snap: &Snapshot) {
    if std::fs::create_dir_all(usage_dir).is_err() {
        return;
    }
    let _ = write_atomic_private(&snapshot_path(usage_dir), snap.to_value().to_string());
}

/// Keeps what the statusline input carries: reads `rate_limits`, lays it over
/// the stored reading and saves the result. Does nothing when the input is not
/// JSON or has no usable part. Best effort, like [`save`].
pub fn remember(usage_dir: &Path, input: &str, now: i64) {
    if let Some(new) = Snapshot::from_statusline(input, now) {
        // The history gets the windows this reading changed or confirmed, not
        // the ones kept from an earlier reading and not a stale figure.
        let (merged, accepted) = new.merged_over(load(usage_dir), now);
        save(usage_dir, &merged);
        super::limits_history::record(usage_dir, accepted.five_hour, accepted.seven_day, now);
    }
}

/// The stored snapshot, or `None` when there is none or it is unreadable.
pub fn load(usage_dir: &Path) -> Option<Snapshot> {
    let text = std::fs::read_to_string(snapshot_path(usage_dir)).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    Some(Snapshot::from_value(&v))
}

/// The manual budgets from the config: `(period, usd)` for each key that is
/// set above zero.
pub fn manual_budgets(home: &Path) -> Vec<(LimitPeriod, f64)> {
    [
        ("usage.budget.monthlyUsd", LimitPeriod::Monthly),
        ("usage.budget.weeklyUsd", LimitPeriod::Weekly),
    ]
    .into_iter()
    .filter_map(|(key, period)| {
        let value = super::host::config_value(key, home)?;
        let usd = value.as_f64().filter(|n| n.is_finite() && *n > 0.0)?;
        Some((period, usd))
    })
    .collect()
}

/// Everything limits are built from: the statusline reading, the manual
/// budgets and what Claude Code keeps in its config file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Sources {
    pub snapshot: Option<Snapshot>,
    pub budgets: Vec<(LimitPeriod, f64)>,
    pub account: Account,
    /// When Claude Code logged an HTTP 429, in UTC seconds, from the usage
    /// store. Set by the caller that holds the connection (`run::limit_sources`);
    /// empty until then.
    pub rate_limited: Vec<i64>,
    /// The window readings of `rate_limits.jsonl` as `(window, time, percent)`,
    /// oldest first. Set by the caller that draws burn-ups
    /// (`run::limit_sources`); empty until then.
    pub history: Vec<(LimitPeriod, i64, f64)>,
}

impl Sources {
    /// Reads the sources. `usage_dir` is the folder of the usage database.
    pub fn load(usage_dir: Option<&Path>, claude_json: &Path, home: &Path) -> Sources {
        Sources::new(usage_dir, claude_json, manual_budgets(home), true)
    }

    /// The sources with the budgets given. The terminal view asks for the
    /// config file to be read through the cache (`cached`), since it reads on
    /// every refresh.
    pub fn new(
        usage_dir: Option<&Path>,
        claude_json: &Path,
        budgets: Vec<(LimitPeriod, f64)>,
        cached: bool,
    ) -> Sources {
        Sources {
            snapshot: usage_dir.and_then(load),
            budgets,
            account: if cached {
                super::plan::read_cached(claude_json)
            } else {
                super::plan::read(claude_json)
            },
            rate_limited: Vec::new(),
            history: Vec::new(),
        }
    }

    /// The plan limits were read over six hours ago.
    pub fn stale(&self, now: i64) -> bool {
        self.snapshot.as_ref().is_some_and(|s| s.is_stale(now))
    }
}

/// Where the monthly dollar limit comes from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Monthly {
    /// The statusline reported one (behind a Claude apps gateway). The cache
    /// and the manual budget are not used then.
    Measured,
    /// The spend limit in the Claude Code cache. `fresh` is false once the
    /// cache is over six hours old.
    Cached {
        limit_usd: f64,
        used_usd: Option<f64>,
        fetched_at: i64,
        age: i64,
        fresh: bool,
    },
    /// `usage.budget.monthlyUsd`.
    Manual(f64),
    Unset,
}

/// The monthly limit to use. A detected budget (the statusline reading, or a
/// cache fetched after it, else the cache however old) always beats
/// `usage.budget.monthlyUsd`, which is only for when nothing can be detected.
pub fn monthly(s: &Sources, now: i64) -> Monthly {
    let cached = s
        .account
        .cache
        .as_ref()
        .filter(|c| !c.is_from_the_future(now))
        .and_then(|c| {
            c.spend.map(|spend| Monthly::Cached {
                limit_usd: spend.limit_usd,
                used_usd: spend.used_usd,
                fetched_at: c.fetched_at,
                age: c.age(now),
                fresh: c.is_fresh(now),
            })
        });
    let measured = s.snapshot.as_ref().is_some_and(|sn| {
        sn.limits(now)
            .iter()
            .any(|l| l.period == LimitPeriod::Monthly)
    });
    // A cache fetched after the statusline last read the spend is the newer
    // word from Claude Code, whatever its age: the six hour rule only decides
    // whether its used amount is trusted.
    let cache_is_newer = matches!(cached, Some(Monthly::Cached { fetched_at, .. })
        if s.snapshot.as_ref().is_none_or(|sn| fetched_at > sn.read_of(LimitPeriod::Monthly)));
    if measured && !cache_is_newer {
        return Monthly::Measured;
    }
    // What Claude Code reports, however old, comes before the manual budget,
    // which is only for when nothing can be detected.
    match (
        cached,
        s.budgets.iter().find(|(p, _)| *p == LimitPeriod::Monthly),
    ) {
        (Some(c), _) => c,
        (None, Some((_, usd))) => Monthly::Manual(*usd),
        (None, None) => Monthly::Unset,
    }
}

/// Every known limit at `now`: measured first, then cached, then manual.
/// Empty when nothing is known, which the views show as the "limit unknown"
/// empty state.
pub fn all_limits(s: &Sources, events: &[UsageEvent], now: i64, week: WeekStart) -> Vec<Limit> {
    let mut out = s
        .snapshot
        .as_ref()
        .map(|s| s.limits(now))
        .unwrap_or_default();
    // A cached window fills in a window the statusline has not reported, or
    // replaces one the statusline read before the cache was fetched. Only a
    // fresh cache is used: a percentage changes within minutes.
    if let Some(c) = s.account.cache.as_ref().filter(|c| c.is_fresh(now)) {
        for (period, w) in [
            (LimitPeriod::FiveHour, &c.five_hour),
            (LimitPeriod::SevenDay, &c.seven_day),
        ] {
            let Some(w) = w.as_ref().filter(|w| w.resets_at > now) else {
                continue;
            };
            // The cache replaces a reading of the same window only when it was
            // fetched after that window was read.
            let newer = s
                .snapshot
                .as_ref()
                .is_none_or(|sn| c.fetched_at > sn.read_of(period));
            let known = out.iter().any(|l| l.period == period);
            if known && !newer {
                continue;
            }
            out.retain(|l| l.period != period);
            let (start, end) = period.ending_at(w.resets_at);
            out.push(Limit {
                unit: Unit::Percent,
                period,
                origin: Origin::Cached,
                used: w.used_percentage,
                limit: 100.0,
                start,
                end,
            });
        }
    }
    let choice = monthly(s, now);
    // The configured monthly budget is only for when nothing is detected.
    let detected = matches!(choice, Monthly::Cached { .. } | Monthly::Measured);
    let cached_wins = matches!(choice, Monthly::Cached { .. });
    if cached_wins {
        out.retain(|l| l.period != LimitPeriod::Monthly);
    }
    for (period, usd) in &s.budgets {
        if *period == LimitPeriod::Monthly && detected {
            continue;
        }
        out.push(Limit::manual_budget(*period, *usd, events, now, week));
    }
    if let Monthly::Cached {
        limit_usd,
        used_usd,
        fetched_at,
        fresh,
        ..
    } = choice
    {
        let mut l = Limit::manual_budget(LimitPeriod::Monthly, limit_usd, events, now, week);
        l.origin = Origin::Cached;
        // The cap is a plan property that holds across months, so even a cache
        // from last month gives it; only the used amount is month-guarded.
        // A fresh cache knows the spend of every machine on the account, so
        // it replaces this machine's estimate, unless it was fetched before
        // this month began.
        let (month_start, month_end) = utc_month_bounds(now);
        if let Some(used) = used_usd.filter(|_| fresh && fetched_at >= month_start) {
            // The figure counts the UTC month, so the limit's window is that
            // month too, not the local one.
            l.start = month_start;
            l.end = month_end;
            l.used = used;
        }
        out.push(l);
    }
    // On Enterprise the whole limit is the monthly extra usage budget, so a
    // window reading has nothing to measure.
    if s.account.plan == crate::plan::Plan::Enterprise {
        out.retain(|l| !matches!(l.period, LimitPeriod::FiveHour | LimitPeriod::SevenDay));
    }
    out
}

/// The figure Claude reports for this UTC month with an amount used: the
/// statusline gateway amount when it is monthly, else the usage cache. A
/// gateway figure counts its own period, so only a monthly one is comparable
/// with the local spend of the month.
fn billed_figure(s: &Sources, now: i64) -> Option<SpendFigure> {
    let month_start = utc_month_bounds(now).0;
    let usable = |f: &SpendFigure| f.used.is_some() && f.fetched_at >= month_start;
    let statusline = s
        .snapshot
        .as_ref()
        .and_then(|sn| {
            let sp = sn
                .spend_limit
                .as_ref()
                .filter(|sp| sp.resets_at > now && sp.period == Some(LimitPeriod::Monthly))?;
            SpendFigure::from_usd(
                sp.limit_usd,
                sp.used_usd,
                sn.read.spend_limit.unwrap_or(sn.captured_at),
            )
        })
        .filter(usable);
    statusline.or_else(|| {
        s.account
            .cache
            .as_ref()
            .filter(|c| !c.is_from_the_future(now))
            .and_then(|c| c.billed.clone())
            .filter(usable)
    })
}

/// When Claude read the amount used this month and what it was, in dollars,
/// for the burn-up chart.
pub fn billed_anchor(s: &Sources, now: i64) -> Option<(i64, f64)> {
    let f = billed_figure(s, now)?;
    f.used_usd().map(|used| (f.fetched_at, used))
}

/// Whether this is the monthly dollar limit the burn-up table follows.
pub(crate) fn is_monthly_dollars(l: &Limit) -> bool {
    l.period == LimitPeriod::Monthly && l.unit == Unit::Dollars
}

/// The amount Claude Code reports as used next to this machine's estimate for
/// the UTC month that holds `now`. The figure is the statusline gateway
/// `spend_limit` when there is one, else the `spend` block of the usage cache.
/// `None` when neither carries an amount used this month, or the cache is dated
/// in the future. The inner error says why the two were not compared, so a
/// figure in another currency or without one is reported and never dropped.
pub fn compare_billed(
    s: &Sources,
    events: &[UsageEvent],
    now: i64,
) -> Option<Result<BilledComparison, NotCompared>> {
    let month_start = utc_month_bounds(now).0;
    let figure = billed_figure(s, now)?;
    // Claude reported the amount at `fetched_at`, so only the local spend up
    // to then is comparable.
    let until = now.min(figure.fetched_at);
    let local = events
        .iter()
        .filter(|e| e.timestamp >= month_start && e.timestamp <= until)
        .map(|e| e.cost_usd)
        .sum::<f64>()
        + 0.0;
    Some(BilledComparison::new(figure, local))
}

/// The line for the 5 hour and weekly windows when none is known. A plan
/// known to have them asks for a reading. Enterprise is told the whole limit
/// is the monthly extra usage budget, and always gets that line. Any other
/// plan is told they are not available and where Pro and Max plans get them.
/// `None` when a window is known on a plan that has them.
pub fn windows_note(s: &Sources, limits: &[Limit]) -> Option<&'static str> {
    if s.account.plan == crate::plan::Plan::Enterprise {
        return Some("5 hour and weekly windows: not used on Enterprise, the whole limit is the monthly extra usage budget");
    }
    if limits
        .iter()
        .any(|l| matches!(l.period, LimitPeriod::FiveHour | LimitPeriod::SevenDay))
    {
        return None;
    }
    Some(if s.account.plan.has_windows() {
        "5 hour and weekly windows: no recent reading, they appear after a session with the playbook statusline"
    } else {
        "5 hour and weekly windows: not available for this plan (Pro and Max get them from the playbook statusline)"
    })
}

/// The plan label, for views that name it.
pub fn plan_label(s: &Sources) -> &'static str {
    s.account.plan.label()
}

/// The empty state shown where a limit would be.
pub const UNKNOWN_HINT: &str = "limit unknown: set usage.budget.monthlyUsd or usage.budget.weeklyUsd, or sign in with a Pro or Max plan and use the playbook statusline to read the plan limits";

/// The oldest event the limits report needs: the start of the previous month,
/// week and day, whichever is earliest.
pub fn events_since(now: i64, week: WeekStart) -> i64 {
    [Period::Today, Period::Week(week), Period::Month]
        .into_iter()
        .map(|p| p.windows(now).previous.start)
        .min()
        .unwrap_or(0)
}

/// Cost so far this period and in the previous one over the same elapsed span.
pub fn burn_pair(events: &[UsageEvent], period: Period, now: i64) -> (Burn, Burn) {
    let w = period.windows(now);
    let (current, previous) = w.split(events);
    // `+ 0.0` turns the -0.0 of an empty sum into 0.0.
    let cost = |e: &[UsageEvent]| e.iter().map(|e| e.cost_usd).sum::<f64>() + 0.0;
    (
        Burn::new(cost(&current), w.current.seconds()),
        Burn::new(cost(&previous), w.previous.seconds()),
    )
}

/// The limits and burn rates as JSON, for `--json`.
pub fn report_json(s: &Sources, events: &[UsageEvent], now: i64, week: WeekStart) -> Value {
    let snapshot = s.snapshot.as_ref();
    let limits = all_limits(s, events, now, week);
    let burnups = burnups_for(s, &limits, events, now);
    let burn: serde_json::Map<String, Value> = [Period::Today, Period::Week(week), Period::Month]
        .into_iter()
        .map(|p| {
            let (current, previous) = burn_pair(events, p, now);
            (
                p.key().to_string(),
                json!({"current": current.to_json(), "previous": previous.to_json()}),
            )
        })
        .collect();
    json!({
        "known": !limits.is_empty(),
        "hint": limits.is_empty().then_some(UNKNOWN_HINT),
        "snapshot_captured_at": snapshot.map(|s| s.captured_at),
        "snapshot_stale": snapshot.map(|s| s.is_stale(now)),
        "plan": s.account.plan.key(),
        "plan_label": plan_label(s),
        "windows_note": windows_note(s, &limits),
        "reason": reason(s, now).map(Reason::key),
        "comparison": comparison_json(s, events, now),
        "cache": s.account.cache.as_ref().map(|c| json!({
            "fetched_at": c.fetched_at,
            "age_seconds": c.age(now),
            "fresh": c.is_fresh(now),
        })),
        "limits": limits.iter().zip(burnups).map(|(l, b)| {
            let mut v = l.to_json(now);
            if let (Some(map), Some(b)) = (v.as_object_mut(), b) {
                map.insert("series".into(), b.series_json());
            }
            v
        }).collect::<Vec<_>>(),
        "burn": burn,
    })
}

/// Why no limit from Claude is shown. The words are the ones the output uses,
/// in `--summary` and in the `reason` of `--json`. None of them is an error:
/// every one has a line that says what to do, or that nothing can be done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// No statusline reading and no usable cache yet.
    NoReading,
    /// The only reading is a cache over six hours old, which is not used.
    Stale,
    /// Nothing identifies a limit on this login and no source carries one.
    CapUnknown,
    /// The last reading held windows, and they have all reset.
    WindowsReset,
    /// Claude Code was rate limited (HTTP 429) and no limit is reported.
    RateLimitedSeen,
}

impl Reason {
    pub fn key(self) -> &'static str {
        match self {
            Reason::NoReading => "no-reading",
            Reason::Stale => "stale",
            Reason::CapUnknown => "cap-unknown",
            Reason::WindowsReset => "windows-reset",
            Reason::RateLimitedSeen => "rate-limited-seen",
        }
    }

    /// The one line that stands where the limit would be.
    pub fn line(self) -> &'static str {
        match self {
            Reason::NoReading => {
                "no limit reading yet: run a session with the playbook statusline"
            }
            Reason::Stale => {
                "the last limit reading is over 6 hours old and is not used: run a session with the playbook statusline"
            }
            Reason::CapUnknown => "cap not reported by Claude Code on this login",
            Reason::WindowsReset => {
                "the windows of the last reading have reset: run a session with the playbook statusline for a new one"
            }
            Reason::RateLimitedSeen => {
                "Claude Code was rate limited, yet it reports no limit: run a session with the playbook statusline"
            }
        }
    }
}

/// Why no limit from Claude is shown, `None` when one is. A limit from Claude
/// is any that did not come from a manual budget.
pub fn reason(s: &Sources, now: i64) -> Option<Reason> {
    let has_limit = all_limits(s, &[], now, WeekStart::Monday)
        .iter()
        .any(|l| l.origin != Origin::Manual);
    if has_limit {
        return None;
    }
    // Enterprise drops the windows, so a held window is not a reset one there.
    let windows = s.account.plan != crate::plan::Plan::Enterprise;
    let held = s.snapshot.as_ref().is_some_and(|sn| {
        (windows && (sn.five_hour.is_some() || sn.seven_day.is_some())) || sn.spend_limit.is_some()
    });
    Some(if held {
        Reason::WindowsReset
    } else if s.account.cache.as_ref().is_some_and(|c| !c.is_fresh(now)) {
        Reason::Stale
    } else if s.account.plan.has_windows() {
        if newest_rate_limit(s, now).is_some() {
            Reason::RateLimitedSeen
        } else {
            Reason::NoReading
        }
    } else {
        Reason::CapUnknown
    })
}

/// The newest 429 within [`RATE_LIMIT_RECENT_SECONDS`] of `now`.
fn newest_rate_limit(s: &Sources, now: i64) -> Option<i64> {
    s.rate_limited
        .iter()
        .copied()
        .filter(|t| *t <= now && now - *t <= RATE_LIMIT_RECENT_SECONDS)
        .max()
}

/// The sentence for a 429 at `at`, in the chosen zone like the crossings.
pub(crate) fn rate_limited_text(at: i64) -> String {
    format!(
        "Claude Code was rate limited at {} (HTTP 429)",
        &stamp(at)[..16]
    )
}

/// How old a reading is, with the stale marker once it is over six hours old.
fn read_text(fetched_at: i64, now: i64) -> String {
    let age = (now - fetched_at).max(0);
    let ago = super::period::elapsed_text(age);
    if age > STALE_SECONDS {
        format!("stale, read {ago} ago")
    } else {
        format!("read {ago} ago")
    }
}

/// The comparison of the amount Claude reports with the local estimate as
/// JSON: the numbers, or why they were not compared, or `null`.
fn comparison_json(s: &Sources, events: &[UsageEvent], now: i64) -> Value {
    match compare_billed(s, events, now) {
        Some(Ok(c)) => json!({
            "billed_usd": c.billed_usd,
            "local_estimate_usd": c.local_estimate_usd,
            "delta_usd": c.delta_usd,
            "delta_pct": c.delta_pct,
            "disagrees": c.disagrees,
            "read_at": c.billed.fetched_at,
            "stale": now - c.billed.fetched_at > STALE_SECONDS,
        }),
        Some(Err(why)) => json!({"skipped": why.to_string()}),
        None => Value::Null,
    }
}

/// An instant as text, in the chosen zone.
fn stamp(at: i64) -> String {
    crate::tz::iso(at).replace('T', " ")
}

/// One threshold as a phrase.
pub(crate) fn crossing_text(c: &Crossing) -> String {
    let pct = c.pct;
    match c.state {
        CrossState::Crossed { at } => format!("{pct}% crossed about {}", stamp(at)),
        CrossState::Projected { at } => format!("{pct}% projected about {}", stamp(at)),
        CrossState::Beyond { period_end } => {
            format!("{pct}% not before the period ends {}", stamp(period_end))
        }
        CrossState::Unknown => format!("{pct}% too early to say"),
    }
}

/// The lines about what Claude reports for `--summary`: the amount used
/// beside the local estimate, with its age, when 50, 80 and 100 percent are
/// crossed for each limit, the newest HTTP 429 of the last seven days, and the
/// one line saying why there is no limit from Claude when there is none.
pub fn limit_lines(s: &Sources, limits: &[Limit], events: &[UsageEvent], now: i64) -> Vec<String> {
    let mut out = Vec::new();
    match compare_billed(s, events, now) {
        Some(Ok(c)) => out.push(format!(
            "{} ({})",
            c.line(),
            read_text(c.billed.fetched_at, now)
        )),
        Some(Err(why)) => out.push(format!("local estimate not compared: {why}")),
        None => {}
    }
    for l in limits {
        if let Some(c) = l.thresholds(now) {
            out.push(format!(
                "{} crossings ({}): {}",
                l.period.key().replace('_', " "),
                l.origin.key(),
                c.iter().map(crossing_text).collect::<Vec<_>>().join(", ")
            ));
        }
    }
    out.extend(newest_rate_limit(s, now).map(rate_limited_text));
    out.extend(reason(s, now).map(|r| r.line().to_string()));
    out
}

/// A dollar or percent amount with its unit.
pub fn amount(unit: Unit, n: f64) -> String {
    match unit {
        Unit::Dollars => format!("${n:.2}"),
        Unit::Percent => format!("{n:.1}%"),
    }
}

/// One limit as a plain sentence for `--summary`.
pub fn describe(l: &Limit, now: i64) -> String {
    let source = match l.origin {
        Origin::Measured => "measured from Claude Code",
        Origin::Manual => "manual limit",
        Origin::Cached => "cached by Claude Code",
    };
    let head = format!(
        "{} {}: {} of {} used ({source})",
        l.period.key().replace('_', " "),
        match l.unit {
            Unit::Dollars => "budget",
            Unit::Percent => "window",
        },
        amount(l.unit, l.used),
        amount(l.unit, l.limit),
    );
    let Some(h) = l.headroom(now) else {
        return head;
    };
    let per = |n: Option<f64>| n.map_or("n/a".to_string(), |n| amount(l.unit, n));
    let state = match h.state {
        State::Exceeded => "limit reached".to_string(),
        State::Unknown => "too early to project".to_string(),
        State::Under => "under".to_string(),
        State::OnTrack => "on track".to_string(),
        State::WillExceed { at } => format!(
            "will exceed at about {}",
            crate::tz::iso(at).replace('T', " ")
        ),
    };
    format!(
        "{head}; {} left, avg {}/h {}/day, allowed {}/h {}/day, projected {}, {state}",
        amount(l.unit, h.remaining),
        per(h.avg_per_hour),
        per(h.avg_per_day),
        per(h.allowed_per_hour),
        per(h.allowed_per_day),
        per(h.projected),
    )
}

/// The limits section of `--summary`.
pub fn summary_section(s: &Sources, events: &[UsageEvent], now: i64, week: WeekStart) -> String {
    let limits = all_limits(s, events, now, week);
    let mut out = format!("\nLimits and burn rate ({})\n", crate::tz::label());
    if limits.is_empty() {
        out.push_str(&format!("  {UNKNOWN_HINT}\n"));
    }
    for l in &limits {
        out.push_str(&format!("  {}\n", describe(l, now)));
    }
    if s.stale(now) {
        out.push_str("  note: the plan limits were read over 6 hours ago\n");
    }
    if let Some(c) = s
        .account
        .cache
        .as_ref()
        .filter(|_| limits.iter().any(|l| l.origin == Origin::Cached))
    {
        out.push_str(&format!(
            "  note: the cached limits were fetched by Claude Code {} ago{}\n",
            super::period::elapsed_text(c.age(now)),
            if c.is_fresh(now) { "" } else { " (stale)" }
        ));
    }
    // The reason line below says the same, so the note stays for the case where
    // some limit is known but one window is not.
    if let Some(note) = windows_note(s, &limits).filter(|_| reason(s, now).is_none()) {
        out.push_str(&format!("  {note}\n"));
    }
    for line in limit_lines(s, &limits, events, now) {
        out.push_str(&format!("  {line}\n"));
    }
    let burnups = burnups_for(s, &limits, events, now);
    if let Some(b) = limits
        .iter()
        .zip(burnups)
        .find_map(|(l, b)| b.filter(|_| is_monthly_dollars(l)))
    {
        out.push_str("  burn-up, cumulative spend this period\n");
        for line in b.table(BURNUP_TABLE_ROWS) {
            out.push_str(&format!("    {line}\n"));
        }
    }
    for p in [Period::Today, Period::Week(week), Period::Month] {
        let (c, v) = burn_pair(events, p, now);
        let rate = |b: &Burn| match (b.per_hour, b.per_day) {
            (Some(h), Some(d)) => format!("${h:.2}/h ${d:.2}/day"),
            _ => "n/a".to_string(),
        };
        out.push_str(&format!(
            "  {}: {} so far, {}; {} {}\n",
            p.label(),
            amount(Unit::Dollars, c.cost_usd),
            rate(&c),
            p.previous_label(),
            rate(&v)
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn input(extra: &str) -> String {
        format!(r#"{{"model":{{"id":"m"}},"rate_limits":{{{extra}}}}}"#)
    }

    #[test]
    fn reads_both_subscription_windows() {
        let s = Snapshot::from_statusline(
            &input(&format!(
                r#""five_hour":{{"used_percentage":23.5,"resets_at":{}}},
                   "seven_day":{{"used_percentage":41.2,"resets_at":{}}}"#,
                NOW + 3600,
                NOW + 86_400
            )),
            NOW,
        )
        .unwrap();
        let limits = s.limits(NOW);
        assert_eq!(limits.len(), 2);
        assert_eq!(limits[0].period, LimitPeriod::FiveHour);
        assert_eq!(limits[0].used, 23.5);
        assert_eq!(limits[0].origin, Origin::Measured);
        assert_eq!(limits[0].end, NOW + 3600);
        assert_eq!(limits[1].period, LimitPeriod::SevenDay);
    }

    #[test]
    fn one_missing_window_is_fine() {
        let s = Snapshot::from_statusline(
            &input(&format!(
                r#""seven_day":{{"used_percentage":1,"resets_at":{}}}"#,
                NOW + 5
            )),
            NOW,
        )
        .unwrap();
        assert!(s.five_hour.is_none());
        assert_eq!(s.limits(NOW).len(), 1);
    }

    #[test]
    fn api_key_input_has_nothing() {
        assert!(Snapshot::from_statusline(r#"{"cost":{"total_cost_usd":1}}"#, NOW).is_none());
        assert!(Snapshot::from_statusline("not json", NOW).is_none());
        assert!(Snapshot::from_statusline(&input(""), NOW).is_none());
        assert!(Snapshot::from_statusline(
            &input(r#""five_hour":{"used_percentage":"x","resets_at":1}"#),
            NOW
        )
        .is_none());
    }

    #[test]
    fn an_expired_window_is_dropped() {
        let s = Snapshot::from_statusline(
            &input(&format!(
                r#""five_hour":{{"used_percentage":90,"resets_at":{}}}"#,
                NOW - 1
            )),
            NOW - 100,
        )
        .unwrap();
        assert!(s.limits(NOW).is_empty());
    }

    #[test]
    fn a_gateway_spend_limit_reads_dollars_when_present() {
        let s = Snapshot::from_statusline(
            &input(&format!(
                r#""spend_limit":{{"used_percentage":62.8,"resets_at":{},"used_usd":314.12,"limit_usd":500,"period":"monthly"}}"#,
                NOW + 86_400
            )),
            NOW,
        )
        .unwrap();
        let l = &s.limits(NOW)[0];
        assert_eq!((l.unit, l.period), (Unit::Dollars, LimitPeriod::Monthly));
        assert_eq!((l.used, l.limit), (314.12, 500.0));
    }

    #[test]
    fn a_gateway_spend_limit_falls_back_to_percent() {
        let s = Snapshot::from_statusline(
            &input(&format!(
                r#""spend_limit":{{"used_percentage":62.8,"resets_at":{}}}"#,
                NOW + 86_400
            )),
            NOW,
        )
        .unwrap();
        let l = &s.limits(NOW)[0];
        assert_eq!((l.unit, l.used, l.limit), (Unit::Percent, 62.8, 100.0));
    }

    #[test]
    fn the_file_round_trips_and_holds_no_identity() {
        let dir = std::env::temp_dir().join(format!("pb-limits-{}", std::process::id()));
        let s = Snapshot::from_statusline(
            &input(&format!(
                r#""five_hour":{{"used_percentage":5,"resets_at":{}}}"#,
                NOW + 60
            )),
            NOW,
        )
        .unwrap();
        save(&dir, &s);
        assert_eq!(load(&dir), Some(s));
        let text = std::fs::read_to_string(snapshot_path(&dir)).unwrap();
        assert!(!text.contains("model"));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(load(&dir), None);
    }

    #[test]
    fn a_snapshot_goes_stale_after_six_hours() {
        let s = Snapshot {
            captured_at: NOW,
            ..Snapshot::default()
        };
        assert!(!s.is_stale(NOW + STALE_SECONDS));
        assert!(s.is_stale(NOW + STALE_SECONDS + 1));
    }

    #[test]
    fn nothing_known_is_an_empty_list() {
        let l = all_limits(&Sources::default(), &[], NOW, WeekStart::Monday);
        assert!(l.is_empty());
    }

    #[test]
    fn manual_budgets_follow_the_snapshot() {
        let s = Sources {
            budgets: vec![(LimitPeriod::Monthly, 100.0), (LimitPeriod::Weekly, 30.0)],
            ..Sources::default()
        };

        let l = all_limits(&s, &[], NOW, WeekStart::Monday);

        assert_eq!(l.len(), 2);
        assert!(l.iter().all(|x| x.origin == Origin::Manual));
    }

    fn cache(age: i64, spend: Option<SpendLimit>, five_hour: Option<Window>) -> Account {
        cache_at(NOW - age, spend, five_hour, None)
    }

    fn cache_at(
        fetched_at: i64,
        spend: Option<SpendLimit>,
        five_hour: Option<Window>,
        seven_day: Option<Window>,
    ) -> Account {
        Account {
            plan: crate::plan::Plan::Unknown,
            cache: Some(crate::plan::UsageCache {
                fetched_at,
                five_hour,
                seven_day,
                spend,
                billed: None,
                extra_windows: Vec::new(),
            }),
        }
    }

    fn window_at(used_percentage: f64, resets_at: i64) -> Window {
        Window {
            used_percentage,
            resets_at,
        }
    }

    fn cost(timestamp: i64, cost_usd: f64) -> UsageEvent {
        UsageEvent {
            timestamp,
            cost_usd,
            ..UsageEvent::default()
        }
    }

    fn gateway_snapshot() -> Snapshot {
        Snapshot {
            captured_at: NOW - 60,
            spend_limit: Some(Spend {
                used_percentage: 10.0,
                resets_at: NOW + 86_400,
                used_usd: Some(10.0),
                limit_usd: Some(100.0),
                period: Some(LimitPeriod::Monthly),
            }),
            ..Snapshot::default()
        }
    }

    use crate::plan::SpendLimit;

    fn spend(limit_usd: f64, used_usd: Option<f64>) -> Option<SpendLimit> {
        Some(SpendLimit {
            limit_usd,
            used_usd,
        })
    }

    #[test]
    fn a_fresh_cached_spend_limit_wins_over_the_manual_monthly_budget() {
        let s = Sources {
            budgets: vec![(LimitPeriod::Monthly, 100.0), (LimitPeriod::Weekly, 30.0)],
            account: cache(600, spend(500.0, Some(120.0)), None),
            ..Sources::default()
        };

        let l = all_limits(&s, &[], NOW, WeekStart::Monday);

        let monthly: Vec<_> = l
            .iter()
            .filter(|x| x.period == LimitPeriod::Monthly)
            .collect();
        assert_eq!(monthly.len(), 1);
        assert_eq!(monthly[0].origin, Origin::Cached);
        assert_eq!(monthly[0].limit, 500.0);
        assert_eq!(monthly[0].used, 120.0);
        assert!(l
            .iter()
            .any(|x| x.period == LimitPeriod::Weekly && x.origin == Origin::Manual));
    }

    #[test]
    fn a_stale_cached_spend_limit_still_beats_the_manual_budget() {
        let s = Sources {
            budgets: vec![(LimitPeriod::Monthly, 100.0)],
            account: cache(48 * 3600, spend(500.0, Some(120.0)), None),
            ..Sources::default()
        };

        let l = all_limits(&s, &[], NOW, WeekStart::Monday);

        assert_eq!(l.len(), 1);
        assert_eq!(l[0].origin, Origin::Cached);
        assert_eq!(l[0].limit, 500.0);
        // A stale used amount is not trusted, the local estimate stands.
        assert_eq!(l[0].used, 0.0);
    }

    #[test]
    fn a_fresh_cache_newer_than_the_statusline_spend_reading_wins() {
        // The statusline last read the spend ten days ago.
        let mut sn = gateway_snapshot();
        sn.captured_at = NOW - 10 * 86_400;
        let s = Sources {
            snapshot: Some(sn),
            account: cache(600, spend(500.0, Some(120.0)), None),
            ..Sources::default()
        };

        let l = all_limits(&s, &[], NOW, WeekStart::Monday);

        let monthly: Vec<_> = l
            .iter()
            .filter(|x| x.period == LimitPeriod::Monthly)
            .collect();
        assert_eq!(monthly.len(), 1);
        assert_eq!(monthly[0].origin, Origin::Cached);
        assert_eq!(monthly[0].limit, 500.0);
        assert_eq!(monthly[0].used, 120.0);
    }

    #[test]
    fn a_stale_cached_spend_limit_is_used_when_no_budget_is_set_and_its_spend_is_local() {
        let s = Sources {
            account: cache(48 * 3600, spend(500.0, Some(120.0)), None),
            ..Sources::default()
        };

        let l = all_limits(&s, &[], NOW, WeekStart::Monday);

        assert_eq!(l.len(), 1);
        assert_eq!(l[0].origin, Origin::Cached);
        assert_eq!(l[0].limit, 500.0);
        assert_eq!(l[0].used, 0.0);
    }

    #[test]
    fn a_fresh_cached_window_fills_in_but_never_replaces_a_statusline_window() {
        let cached = Window {
            used_percentage: 70.0,
            resets_at: NOW + 3600,
        };
        let mut s = Sources {
            account: cache(600, None, Some(cached)),
            ..Sources::default()
        };

        let filled = all_limits(&s, &[], NOW, WeekStart::Monday);
        s.snapshot = Some(Snapshot {
            captured_at: NOW,
            five_hour: Some(Window {
                used_percentage: 10.0,
                resets_at: NOW + 7200,
            }),
            ..Snapshot::default()
        });
        let kept = all_limits(&s, &[], NOW, WeekStart::Monday);

        assert_eq!(
            (filled.len(), filled[0].origin, filled[0].used),
            (1, Origin::Cached, 70.0)
        );
        assert_eq!(
            (kept.len(), kept[0].origin, kept[0].used),
            (1, Origin::Measured, 10.0)
        );
    }

    #[test]
    fn a_cached_window_is_ignored_once_the_cache_is_stale() {
        let cached = Window {
            used_percentage: 70.0,
            resets_at: NOW + 3600,
        };
        let s = Sources {
            account: cache(7 * 3600, None, Some(cached)),
            ..Sources::default()
        };

        let l = all_limits(&s, &[], NOW, WeekStart::Monday);

        assert!(l.is_empty());
    }

    #[test]
    fn the_windows_note_says_not_available_for_a_plan_without_windows() {
        let s = Sources::default();

        let note = windows_note(&s, &[]);

        assert_eq!(
            note,
            Some("5 hour and weekly windows: not available for this plan (Pro and Max get them from the playbook statusline)")
        );
    }

    #[test]
    fn the_windows_note_asks_for_a_reading_on_a_max_plan() {
        let s = Sources {
            account: Account {
                plan: crate::plan::Plan::Max5x,
                cache: None,
            },
            ..Sources::default()
        };

        let note = windows_note(&s, &[]).unwrap();

        assert!(note.contains("no recent reading"), "{note}");
    }

    #[test]
    fn there_is_no_windows_note_when_a_window_is_known() {
        let snap = Snapshot::from_statusline(
            &input(&format!(
                r#""five_hour":{{"used_percentage":1.0,"resets_at":{}}}"#,
                NOW + 3600
            )),
            NOW,
        );
        let s = Sources {
            snapshot: snap,
            ..Sources::default()
        };
        let limits = all_limits(&s, &[], NOW, WeekStart::Monday);

        assert_eq!(windows_note(&s, &limits), None);
    }

    #[test]
    fn the_json_names_the_plan_and_the_cache_age() {
        let s = Sources {
            account: cache(600, spend(500.0, None), None),
            ..Sources::default()
        };

        let v = report_json(&s, &[], NOW, WeekStart::Monday);

        assert_eq!(v["plan"], "unknown");
        assert_eq!(v["plan_label"], "Unknown plan");
        assert_eq!(v["cache"]["age_seconds"], 600);
        assert_eq!(v["limits"][0]["origin"], "cached");
        assert!(v["windows_note"]
            .as_str()
            .unwrap()
            .contains("not available"));
    }

    #[test]
    fn a_measured_monthly_limit_hides_the_cache_and_the_manual_monthly_budget() {
        let s = Sources {
            snapshot: Some(gateway_snapshot()),
            budgets: vec![(LimitPeriod::Monthly, 100.0), (LimitPeriod::Weekly, 30.0)],
            account: cache(600, spend(500.0, Some(120.0)), None),
            rate_limited: Vec::new(),
            history: Vec::new(),
        };

        let l = all_limits(&s, &[], NOW, WeekStart::Monday);

        let found: Vec<_> = l.iter().map(|x| (x.period, x.origin)).collect();
        assert_eq!(
            found,
            vec![
                (LimitPeriod::Monthly, Origin::Measured),
                (LimitPeriod::Weekly, Origin::Manual)
            ]
        );
        assert_eq!(monthly(&s, NOW), Monthly::Measured);
    }

    #[test]
    fn a_newer_cache_beats_an_older_statusline_reading_even_when_the_cache_is_stale() {
        // The statusline read the spend ten days ago, the cache 7 hours ago.
        let mut sn = gateway_snapshot();
        sn.captured_at = NOW - 10 * 86_400;
        let s = Sources {
            snapshot: Some(sn),
            account: cache(7 * 3600, spend(500.0, Some(120.0)), None),
            ..Sources::default()
        };

        let l = all_limits(&s, &[], NOW, WeekStart::Monday);

        assert_eq!(l.len(), 1);
        assert_eq!((l[0].origin, l[0].limit), (Origin::Cached, 500.0));
        // The stale used amount is still not trusted.
        assert_eq!(l[0].used, 0.0);
    }

    #[test]
    fn a_cache_from_last_month_still_gives_the_cap_but_not_the_used_amount() {
        // 2027-01-01 01:00 UTC, with the cache fetched in December.
        let now = 1_798_761_600 + 3600;
        let s = Sources {
            account: cache_at(now - 3 * 86_400, spend(500.0, Some(480.0)), None, None),
            ..Sources::default()
        };

        let l = all_limits(&s, &[], now, WeekStart::Monday);

        assert_eq!(
            (l[0].origin, l[0].limit, l[0].used),
            (Origin::Cached, 500.0, 0.0)
        );
    }

    #[test]
    fn a_fresh_cached_used_amount_replaces_the_local_estimate() {
        let s = Sources {
            account: cache(600, spend(500.0, Some(10.0)), None),
            ..Sources::default()
        };
        let events = [cost(NOW - 3600, 50.0)];

        let l = all_limits(&s, &events, NOW, WeekStart::Monday);

        assert_eq!(l[0].used, 10.0);
    }

    #[test]
    fn a_fresh_cache_without_a_used_amount_uses_the_local_estimate() {
        let s = Sources {
            account: cache(600, spend(500.0, None), None),
            ..Sources::default()
        };
        let events = [cost(NOW - 3600, 50.0)];

        let l = all_limits(&s, &events, NOW, WeekStart::Monday);

        assert_eq!(l[0].used, 50.0);
    }

    #[test]
    fn a_cached_used_amount_fetched_before_the_month_began_is_ignored() {
        // 2027-01-01 01:00 UTC, with the cache fetched two hours earlier.
        let now = 1_798_761_600 + 3600;
        let s = Sources {
            account: cache_at(now - 7200, spend(500.0, Some(480.0)), None, None),
            ..Sources::default()
        };

        let l = all_limits(&s, &[], now, WeekStart::Monday);

        assert_eq!(l[0].used, 0.0);
    }

    #[test]
    fn a_cache_fetched_in_the_future_is_not_used_for_the_monthly_limit() {
        let mut s = Sources {
            account: cache(-7200, spend(500.0, Some(1.0)), None),
            ..Sources::default()
        };

        let none = monthly(&s, NOW);
        s.budgets = vec![(LimitPeriod::Monthly, 100.0)];
        let manual = monthly(&s, NOW);

        assert_eq!(none, Monthly::Unset);
        assert_eq!(manual, Monthly::Manual(100.0));
    }

    fn enterprise() -> Account {
        Account {
            plan: crate::plan::Plan::Enterprise,
            cache: None,
        }
    }

    #[test]
    fn an_enterprise_account_shows_no_five_hour_or_weekly_window() {
        let s = Sources {
            snapshot: Some(window_snapshot(3600)),
            account: enterprise(),
            budgets: vec![(LimitPeriod::Monthly, 1000.0)],
            ..Sources::default()
        };

        let l = all_limits(&s, &[], NOW, WeekStart::Monday);

        assert!(l
            .iter()
            .all(|x| !matches!(x.period, LimitPeriod::FiveHour | LimitPeriod::SevenDay)));
        assert!(l.iter().any(|x| x.period == LimitPeriod::Monthly));
    }

    #[test]
    fn an_enterprise_reading_of_only_windows_is_not_called_a_reset() {
        let s = Sources {
            snapshot: Some(window_snapshot(3600)),
            account: enterprise(),
            ..Sources::default()
        };

        assert_eq!(reason(&s, NOW), Some(Reason::CapUnknown));
    }

    #[test]
    fn an_enterprise_account_says_the_whole_limit_is_the_extra_usage_budget() {
        let s = Sources {
            account: enterprise(),
            ..Sources::default()
        };

        let note = windows_note(&s, &[]);

        assert_eq!(
            note,
            Some("5 hour and weekly windows: not used on Enterprise, the whole limit is the monthly extra usage budget")
        );
    }

    #[test]
    fn a_cached_weekly_window_is_used_and_an_expired_window_is_skipped() {
        let s = Sources {
            account: cache_at(
                NOW - 60,
                None,
                Some(window_at(30.0, NOW - 1)),
                Some(window_at(60.0, NOW + 86_400)),
            ),
            ..Sources::default()
        };

        let l = all_limits(&s, &[], NOW, WeekStart::Monday);

        assert_eq!(l.len(), 1);
        assert_eq!(
            (l[0].period, l[0].origin, l[0].used),
            (LimitPeriod::SevenDay, Origin::Cached, 60.0)
        );
    }

    #[test]
    fn a_cache_fetched_after_the_statusline_reading_replaces_its_window() {
        let snap = Snapshot {
            captured_at: NOW - 2 * 86_400,
            seven_day: Some(window_at(20.0, NOW + 86_400)),
            ..Snapshot::default()
        };
        let newer = Sources {
            snapshot: Some(snap.clone()),
            account: cache_at(NOW - 60, None, None, Some(window_at(85.0, NOW + 86_400))),
            ..Sources::default()
        };
        let older = Sources {
            snapshot: Some(Snapshot {
                captured_at: NOW,
                ..snap
            }),
            account: cache_at(NOW - 60, None, None, Some(window_at(85.0, NOW + 86_400))),
            ..Sources::default()
        };

        let replaced = all_limits(&newer, &[], NOW, WeekStart::Monday);
        let kept = all_limits(&older, &[], NOW, WeekStart::Monday);

        assert_eq!(
            (replaced.len(), replaced[0].origin, replaced[0].used),
            (1, Origin::Cached, 85.0)
        );
        assert_eq!(
            (kept.len(), kept[0].origin, kept[0].used),
            (1, Origin::Measured, 20.0)
        );
    }

    #[test]
    fn the_summary_names_the_cache_age_and_the_missing_windows() {
        let s = Sources {
            account: cache(600, spend(500.0, Some(120.0)), None),
            ..Sources::default()
        };

        let text = summary_section(&s, &[], NOW, WeekStart::Monday);

        assert!(
            text.contains("monthly budget: $120.00 of $500.00 used (cached by Claude Code)"),
            "{text}"
        );
        assert!(text.contains("fetched by Claude Code 10m ago"), "{text}");
        assert!(text.contains("not available for this plan"), "{text}");
    }

    #[test]
    fn the_summary_has_no_cache_note_when_a_manual_budget_won() {
        // A cache dated in the future loses to the manual budget.
        let s = Sources {
            budgets: vec![(LimitPeriod::Monthly, 100.0)],
            account: cache(-7200, spend(500.0, Some(1.0)), None),
            ..Sources::default()
        };

        let text = summary_section(&s, &[], NOW, WeekStart::Monday);

        assert!(!text.contains("fetched by Claude Code"), "{text}");
        assert!(text.contains("manual limit"), "{text}");
    }

    #[test]
    fn the_cache_used_amount_is_compared_with_the_local_estimate_of_the_utc_month() {
        let mut account = cache(60, spend(1500.0, Some(1059.5)), None);
        let fetched = NOW - 60;
        let c = account.cache.as_mut().unwrap();
        c.billed = SpendFigure::from_usd(Some(1500.0), Some(1059.5), fetched);
        let s = Sources {
            account,
            ..Sources::default()
        };
        let month_start = utc_month_bounds(NOW).0;
        let events = [
            cost(month_start - 1, 50.0),
            cost(month_start, 3.0),
            cost(fetched, 4.0),
            cost(fetched + 1, 100.0),
        ];

        let c = compare_billed(&s, &events, NOW).unwrap().unwrap();

        assert_eq!(c.billed_usd, 1059.5);
        assert_eq!(c.local_estimate_usd, 7.0);
    }

    #[test]
    fn a_figure_in_another_currency_or_without_one_is_reported_not_dropped() {
        let money = |currency: Option<&str>| {
            Some(crate::spend::Money {
                minor: 105_950,
                decimals: 2,
                currency: currency.map(str::to_string),
            })
        };
        for (currency, text) in [
            (Some("EUR"), "currency EUR, not compared"),
            (None, "currency not reported or not readable, not compared"),
        ] {
            let mut account = cache(60, None, None);
            account.cache.as_mut().unwrap().billed = Some(SpendFigure {
                limit: None,
                used: money(currency),
                fetched_at: NOW - 60,
            });
            let s = Sources {
                account,
                ..Sources::default()
            };

            let r = compare_billed(&s, &[], NOW).unwrap();

            assert_eq!(r.unwrap_err().to_string(), text);
        }
    }

    #[test]
    fn nothing_is_compared_without_an_amount_used_this_utc_month() {
        let fig = |fetched_at, used: Option<f64>| {
            let mut account = cache_at(fetched_at, None, None, None);
            account.cache.as_mut().unwrap().billed =
                SpendFigure::from_usd(Some(100.0), used, fetched_at);
            Sources {
                account,
                ..Sources::default()
            }
        };

        assert!(compare_billed(&Sources::default(), &[], NOW).is_none());
        assert!(compare_billed(&fig(NOW - 60, None), &[], NOW).is_none());
        assert!(compare_billed(&fig(utc_month_bounds(NOW).0 - 1, Some(5.0)), &[], NOW).is_none());
        assert!(compare_billed(&fig(NOW + 3600, Some(5.0)), &[], NOW).is_none());
    }

    #[test]
    fn the_statusline_gateway_amount_is_compared_too() {
        let s = Sources {
            snapshot: Some(gateway_snapshot()),
            ..Sources::default()
        };

        let c = compare_billed(&s, &[cost(NOW - 120, 4.0)], NOW)
            .unwrap()
            .unwrap();

        assert_eq!(c.billed_usd, 10.0);
        assert_eq!(c.local_estimate_usd, 4.0);
    }

    #[test]
    fn the_cached_used_amount_counts_from_the_utc_month_not_the_local_one() {
        let month_start = utc_month_bounds(NOW).0;
        let s = Sources {
            account: cache_at(month_start, spend(500.0, Some(120.0)), None, None),
            ..Sources::default()
        };

        let l = all_limits(&s, &[], month_start + 60, WeekStart::Monday);

        let m: Vec<_> = l
            .iter()
            .filter(|x| x.period == LimitPeriod::Monthly)
            .collect();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].used, 120.0);
        assert_eq!((m[0].start, m[0].end), utc_month_bounds(NOW));
    }

    #[test]
    fn a_figure_fetched_at_the_utc_month_start_is_compared_and_the_clock_clamps() {
        let at = |fetched_at: i64| {
            let mut account = cache_at(fetched_at, None, None, None);
            account.cache.as_mut().unwrap().billed =
                SpendFigure::from_usd(Some(100.0), Some(5.0), fetched_at);
            Sources {
                account,
                ..Sources::default()
            }
        };
        let start = utc_month_bounds(NOW).0;

        assert!(compare_billed(&at(start), &[], NOW).is_some());

        // Fetched 120 s ahead of this clock, inside the skew allowance: spend
        // after `now` is not counted.
        let c = compare_billed(
            &at(NOW + 120),
            &[cost(NOW - 1, 1.0), cost(NOW, 2.0), cost(NOW + 60, 4.0)],
            NOW,
        )
        .unwrap()
        .unwrap();
        assert_eq!(c.local_estimate_usd, 3.0);
    }

    #[test]
    fn the_statusline_amount_wins_over_the_cache_only_when_monthly_and_usable() {
        let cached = |s: &mut Sources| {
            s.account = cache(60, None, None);
            s.account.cache.as_mut().unwrap().billed =
                SpendFigure::from_usd(Some(500.0), Some(77.0), NOW - 60);
        };
        let mut both = Sources {
            snapshot: Some(gateway_snapshot()),
            ..Sources::default()
        };
        cached(&mut both);
        let billed = |s: &Sources| compare_billed(s, &[], NOW).unwrap().unwrap().billed_usd;

        assert_eq!(billed(&both), 10.0);

        let weekly = both
            .snapshot
            .as_mut()
            .unwrap()
            .spend_limit
            .as_mut()
            .unwrap();
        weekly.period = Some(LimitPeriod::Weekly);
        assert_eq!(billed(&both), 77.0);

        let mut no_used = both.clone();
        let sp = no_used
            .snapshot
            .as_mut()
            .unwrap()
            .spend_limit
            .as_mut()
            .unwrap();
        sp.period = Some(LimitPeriod::Monthly);
        sp.used_usd = None;
        assert_eq!(billed(&no_used), 77.0);

        let mut ended = both.clone();
        let sp = ended
            .snapshot
            .as_mut()
            .unwrap()
            .spend_limit
            .as_mut()
            .unwrap();
        sp.period = Some(LimitPeriod::Monthly);
        sp.resets_at = NOW;
        assert_eq!(billed(&ended), 77.0);

        let mut old = both.clone();
        let snap = old.snapshot.as_mut().unwrap();
        snap.captured_at = utc_month_bounds(NOW).0 - 1;
        snap.spend_limit.as_mut().unwrap().period = Some(LimitPeriod::Monthly);
        assert_eq!(billed(&old), 77.0);
    }

    // The merge of a statusline reading over the stored one.

    fn merge_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pb-limits-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn reading(five: Option<(f64, i64)>, seven: Option<(f64, i64)>) -> String {
        let part = |name: &str, v: Option<(f64, i64)>| {
            v.map(|(pct, at)| format!(r#""{name}":{{"used_percentage":{pct},"resets_at":{at}}}"#))
        };
        let parts: Vec<String> = [part("five_hour", five), part("seven_day", seven)]
            .into_iter()
            .flatten()
            .collect();
        format!(r#"{{"rate_limits":{{{}}}}}"#, parts.join(","))
    }

    fn stored_in(dir: &Path) -> Snapshot {
        load(dir).unwrap()
    }

    #[test]
    fn a_reading_with_one_window_keeps_the_other_as_stored() {
        let dir = merge_dir("keep");
        remember(
            &dir,
            &reading(Some((10.0, NOW + 3600)), Some((20.0, NOW + 86_400))),
            NOW,
        );

        remember(&dir, &reading(None, Some((25.0, NOW + 86_400))), NOW + 60);

        let s = stored_in(&dir);
        assert_eq!(s.five_hour.unwrap().used_percentage, 10.0);
        assert_eq!(s.seven_day.unwrap().used_percentage, 25.0);
        assert_eq!(s.read.five_hour, Some(NOW));
        assert_eq!(s.read.seven_day, Some(NOW + 60));
        assert_eq!(s.captured_at, NOW + 60);
    }

    #[test]
    fn each_window_keeps_its_own_reading_time_and_one_old_part_makes_it_stale() {
        let dir = merge_dir("stale");
        let later = NOW + 7 * HOUR;
        remember(&dir, &reading(Some((10.0, NOW + 10 * HOUR)), None), NOW);
        remember(&dir, &reading(None, Some((20.0, later + 86_400))), later);

        let s = stored_in(&dir);

        // Both are held, each with the time it was read.
        assert_eq!(s.read.five_hour, Some(NOW));
        assert_eq!(s.read.seven_day, Some(later));
        assert_eq!(s.captured_at, later);
        assert_eq!(s.oldest_read(), NOW);
        // The old five hour window, still live, makes the reading stale.
        assert!(s.is_stale(later));
        // Without it only the fresh part is left and the reading is fresh.
        assert!(!Snapshot {
            five_hour: None,
            ..s
        }
        .is_stale(later));
    }

    #[test]
    fn a_dropped_window_leaves_no_read_time_behind() {
        let dir = merge_dir("dropped");
        let later = NOW + 7 * HOUR;
        remember(&dir, &reading(Some((10.0, NOW + 5 * HOUR)), None), NOW);
        remember(&dir, &reading(None, Some((20.0, later + 86_400))), later);

        let s = stored_in(&dir);

        assert!(s.five_hour.is_none());
        assert_eq!(s.read.five_hour, None);
        assert!(!s.is_stale(later));
    }

    #[test]
    fn a_stored_window_is_dropped_when_its_reset_time_has_come_and_kept_one_second_before() {
        for (resets_at, kept) in [(NOW, false), (NOW + 1, true), (NOW - 1, false)] {
            let dir = merge_dir(&format!("edge{kept}{}", resets_at - NOW));
            remember(&dir, &reading(Some((10.0, resets_at)), None), NOW - 60);

            remember(&dir, &reading(None, Some((20.0, NOW + 86_400))), NOW);

            assert_eq!(stored_in(&dir).five_hour.is_some(), kept, "{resets_at}");
        }
    }

    #[test]
    fn an_input_with_no_window_or_that_is_not_json_changes_nothing() {
        let dir = merge_dir("noop");
        remember(&dir, &reading(Some((10.0, NOW + 3600)), None), NOW);
        let before = std::fs::read_to_string(snapshot_path(&dir)).unwrap();

        for bad in [r#"{"rate_limits":{}}"#, "not json", r#"{"model":"x"}"#, ""] {
            remember(&dir, bad, NOW + 60);
        }

        assert_eq!(
            std::fs::read_to_string(snapshot_path(&dir)).unwrap(),
            before
        );
    }

    #[test]
    fn an_unreadable_stored_file_is_replaced_by_the_new_reading_alone() {
        let dir = merge_dir("broken");
        std::fs::write(snapshot_path(&dir), "{{{").unwrap();

        remember(&dir, &reading(Some((10.0, NOW + 3600)), None), NOW);

        let s = stored_in(&dir);
        assert_eq!(s.five_hour.unwrap().used_percentage, 10.0);
        assert!(s.seven_day.is_none());
    }

    #[test]
    fn two_writers_that_both_loaded_first_leave_the_last_writers_file() {
        let dir = merge_dir("race");
        remember(
            &dir,
            &reading(Some((10.0, NOW + 3600)), Some((20.0, NOW + 86_400))),
            NOW,
        );
        // Both load the same stored reading, then save in turn.
        let base = load(&dir);
        let a = Snapshot::from_statusline(&reading(Some((11.0, NOW + 3600)), None), NOW + 1)
            .unwrap()
            .merged_over(base.clone(), NOW + 1)
            .0;
        let b = Snapshot::from_statusline(&reading(None, Some((21.0, NOW + 86_400))), NOW + 2)
            .unwrap()
            .merged_over(base, NOW + 2)
            .0;

        save(&dir, &a);
        save(&dir, &b);

        let s = stored_in(&dir);
        assert_eq!(s.five_hour.unwrap().used_percentage, 10.0);
        assert_eq!(s.seven_day.unwrap().used_percentage, 21.0);
    }

    #[test]
    fn the_gateway_spend_limit_is_kept_the_same_way() {
        let dir = merge_dir("spend");
        let with_spend = format!(
            r#"{{"rate_limits":{{"spend_limit":{{"used_percentage":10,"resets_at":{},"used_usd":1.0,"limit_usd":10.0,"period":"monthly"}}}}}}"#,
            NOW + 86_400
        );
        remember(&dir, &with_spend, NOW);

        remember(&dir, &reading(Some((5.0, NOW + 3600)), None), NOW + 60);

        let s = stored_in(&dir);
        assert_eq!(s.spend_limit.unwrap().limit_usd, Some(10.0));
        assert_eq!(s.read.spend_limit, Some(NOW));
        assert_eq!(s.five_hour.unwrap().used_percentage, 5.0);
    }

    // Several sessions share the file and an idle one keeps sending the
    // numbers from its last reply: within one window only a higher figure may
    // replace the stored one.

    const WINDOW_END: i64 = NOW + 3 * HOUR;
    const WEEK_END: i64 = NOW + 86_400;

    fn five_hour_after(dir: &Path, readings: &[f64]) -> Vec<f64> {
        readings
            .iter()
            .enumerate()
            .map(|(i, pct)| {
                let at = NOW + i as i64 * 60;
                remember(dir, &reading(Some((*pct, WINDOW_END)), None), at);
                stored_in(dir).five_hour.unwrap().used_percentage
            })
            .collect()
    }

    fn history_rows(dir: &Path) -> Vec<(Option<f64>, Option<f64>)> {
        let text =
            std::fs::read_to_string(crate::limits_history::history_path(dir)).unwrap_or_default();
        text.lines()
            .map(|line| {
                let v: Value = serde_json::from_str(line).unwrap();
                (v["five_hour"].as_f64(), v["seven_day"].as_f64())
            })
            .collect()
    }

    #[test]
    fn the_gateway_spend_limit_follows_the_same_newest_reading_rule() {
        let dir = merge_dir("spend-stale");
        let spend_reading = |pct: f64, at: i64| {
            format!(
                r#"{{"rate_limits":{{"spend_limit":{{"used_percentage":{pct},"resets_at":{at},"used_usd":{pct},"limit_usd":100.0,"period":"monthly"}}}}}}"#
            )
        };
        let month_end = NOW + 20 * 86_400;
        remember(&dir, &spend_reading(40.0, month_end), NOW);

        remember(&dir, &spend_reading(10.0, month_end), NOW + 60);
        assert_eq!(stored_in(&dir).spend_limit.unwrap().used_usd, Some(40.0));
        assert_eq!(stored_in(&dir).read.spend_limit, Some(NOW));

        remember(&dir, &spend_reading(55.0, month_end), NOW + 120);
        assert_eq!(stored_in(&dir).spend_limit.unwrap().used_usd, Some(55.0));
    }

    fn spend_json(pct: f64, used: f64, limit: f64, resets_at: i64) -> String {
        format!(
            r#"{{"rate_limits":{{"spend_limit":{{"used_percentage":{pct},"resets_at":{resets_at},"used_usd":{used},"limit_usd":{limit},"period":"monthly"}}}}}}"#
        )
    }

    #[test]
    fn the_spend_limit_compares_dollars_when_both_readings_have_them() {
        let dir = merge_dir("spend-dollars");
        let month_end = NOW + 20 * 86_400;
        remember(&dir, &spend_json(40.0, 40.4, 100.0, month_end), NOW);

        // The same rounded percentage with fewer dollars is a stale reading.
        remember(&dir, &spend_json(40.0, 40.1, 100.0, month_end), NOW + 60);
        let s = stored_in(&dir);
        assert_eq!(s.spend_limit.unwrap().used_usd, Some(40.4));
        assert_eq!(s.read.spend_limit, Some(NOW));

        remember(&dir, &spend_json(40.0, 40.7, 100.0, month_end), NOW + 120);
        assert_eq!(stored_in(&dir).spend_limit.unwrap().used_usd, Some(40.7));
    }

    #[test]
    fn a_raised_spend_limit_in_the_same_period_replaces_the_stored_one() {
        let dir = merge_dir("spend-limit-raised");
        let month_end = NOW + 20 * 86_400;
        remember(&dir, &spend_json(50.0, 50.0, 100.0, month_end), NOW);

        remember(&dir, &spend_json(25.0, 50.0, 200.0, month_end), NOW + 60);

        let s = stored_in(&dir);
        let spend = s.spend_limit.unwrap();
        assert_eq!((spend.used_usd, spend.limit_usd), (Some(50.0), Some(200.0)));
        assert_eq!(s.read.spend_limit, Some(NOW + 60));
    }

    #[test]
    fn the_spend_limit_falls_back_to_the_percentage_without_dollars() {
        let dir = merge_dir("spend-percent");
        let month_end = NOW + 20 * 86_400;
        let percent_only = |pct: f64| {
            format!(
                r#"{{"rate_limits":{{"spend_limit":{{"used_percentage":{pct},"resets_at":{month_end}}}}}}}"#
            )
        };
        remember(&dir, &percent_only(30.0), NOW);

        remember(&dir, &percent_only(10.0), NOW + 60);
        assert_eq!(stored_in(&dir).spend_limit.unwrap().used_percentage, 30.0);

        remember(&dir, &percent_only(35.0), NOW + 120);
        assert_eq!(stored_in(&dir).spend_limit.unwrap().used_percentage, 35.0);
    }

    #[test]
    fn an_idle_sessions_lower_reading_never_lowers_the_stored_five_hour_value() {
        let dir = merge_dir("monotonic");

        let stored = five_hour_after(&dir, &[14.0, 5.0, 15.0, 2.0, 17.0]);

        assert_eq!(stored, [14.0, 14.0, 15.0, 15.0, 17.0]);
    }

    #[test]
    fn a_weekly_figure_at_the_cap_survives_an_idle_sessions_lower_one() {
        let dir = merge_dir("weekly");
        let mut stored = Vec::new();
        for (i, pct) in [25.0, 100.0, 25.0, 100.0, 25.0].into_iter().enumerate() {
            remember(
                &dir,
                &reading(None, Some((pct, WEEK_END))),
                NOW + i as i64 * 60,
            );
            stored.push(stored_in(&dir).seven_day.unwrap().used_percentage);
        }

        assert_eq!(stored, [25.0, 100.0, 100.0, 100.0, 100.0]);
    }

    #[test]
    fn a_later_reset_time_is_a_new_window_and_replaces_a_higher_value() {
        let dir = merge_dir("newer-window");
        remember(&dir, &reading(Some((90.0, WINDOW_END)), None), NOW);

        remember(
            &dir,
            &reading(Some((3.0, WINDOW_END + 5 * HOUR)), None),
            NOW + 60,
        );

        let w = stored_in(&dir).five_hour.unwrap();
        assert_eq!(w.used_percentage, 3.0);
        assert_eq!(w.resets_at, WINDOW_END + 5 * HOUR);
    }

    #[test]
    fn an_earlier_reset_time_is_a_stale_reading_and_is_dropped_while_the_stored_part_is_live() {
        let dir = merge_dir("older-window");
        remember(&dir, &reading(Some((20.0, WINDOW_END)), None), NOW);

        remember(
            &dir,
            &reading(Some((90.0, WINDOW_END - HOUR)), None),
            NOW + 60,
        );

        let s = stored_in(&dir);
        let w = s.five_hour.unwrap();
        assert_eq!((w.used_percentage, w.resets_at), (20.0, WINDOW_END));
        assert_eq!(s.read.five_hour, Some(NOW));
    }

    #[test]
    fn a_stored_part_past_its_reset_time_is_replaced_by_a_live_reading() {
        let dir = merge_dir("past-reset");
        remember(&dir, &reading(Some((90.0, NOW + 60)), None), NOW);
        let at = NOW + 120;

        remember(&dir, &reading(Some((5.0, at + HOUR)), None), at);

        let s = stored_in(&dir);
        let w = s.five_hour.unwrap();
        assert_eq!((w.used_percentage, w.resets_at), (5.0, at + HOUR));
        assert_eq!(s.read.five_hour, Some(at));
    }

    #[test]
    fn a_stored_part_past_its_reset_time_is_dropped_when_the_reading_lacks_it() {
        let dir = merge_dir("past-reset-dropped");
        remember(
            &dir,
            &reading(Some((90.0, NOW + 60)), Some((40.0, WEEK_END))),
            NOW,
        );

        remember(&dir, &reading(None, Some((41.0, WEEK_END))), NOW + 120);

        let s = stored_in(&dir);
        assert_eq!(s.five_hour, None);
        assert_eq!(s.seven_day.unwrap().used_percentage, 41.0);
    }

    #[test]
    fn a_reading_whose_own_reset_time_has_passed_is_ignored_and_never_recorded() {
        for (name, incoming_end) in [("same", NOW + 60), ("earlier", NOW + 30)] {
            let dir = merge_dir(&format!("expired-incoming-{name}"));
            remember(&dir, &reading(Some((90.0, NOW + 60)), None), NOW);
            let before = history_rows(&dir);
            let at = NOW + 120;

            remember(&dir, &reading(Some((100.0, incoming_end)), None), at);

            let s = stored_in(&dir);
            assert_eq!(s.five_hour, None, "{name}");
            assert_eq!(s.read.five_hour, None, "{name}");
            assert_eq!(history_rows(&dir), before, "{name}");
        }
    }

    #[test]
    fn a_stale_reading_of_one_window_still_carries_the_other_window_forward() {
        let dir = merge_dir("carry");
        remember(
            &dir,
            &reading(Some((30.0, WINDOW_END)), Some((40.0, WEEK_END))),
            NOW,
        );

        remember(&dir, &reading(Some((10.0, WINDOW_END)), None), NOW + 60);

        let s = stored_in(&dir);
        assert_eq!(s.five_hour.unwrap().used_percentage, 30.0);
        assert_eq!(s.seven_day.unwrap().used_percentage, 40.0);
        assert_eq!(s.read.five_hour, Some(NOW));
        assert_eq!(s.read.seven_day, Some(NOW));
    }

    #[test]
    fn an_equal_figure_refreshes_the_read_time_and_a_lower_one_does_not() {
        let dir = merge_dir("read-time");
        remember(&dir, &reading(Some((30.0, WINDOW_END)), None), NOW);

        remember(&dir, &reading(Some((30.0, WINDOW_END)), None), NOW + 60);
        assert_eq!(stored_in(&dir).read.five_hour, Some(NOW + 60));

        remember(&dir, &reading(Some((12.0, WINDOW_END)), None), NOW + 120);
        let s = stored_in(&dir);
        assert_eq!(s.five_hour.unwrap().used_percentage, 30.0);
        assert_eq!(s.read.five_hour, Some(NOW + 60));
    }

    #[test]
    fn a_part_only_stale_readings_touch_goes_stale_on_its_own_clock() {
        let dir = merge_dir("goes-stale");
        remember(&dir, &reading(Some((30.0, NOW + 10 * HOUR)), None), NOW);
        let later = NOW + 7 * HOUR;

        remember(&dir, &reading(Some((10.0, NOW + 10 * HOUR)), None), later);

        assert!(stored_in(&dir).is_stale(later));
    }

    #[test]
    fn the_history_gets_only_the_figures_the_reading_changed_or_confirmed() {
        let dir = merge_dir("history");
        let gap = crate::limits_history::MIN_GAP_SECONDS;
        for (i, pct) in [14.0, 5.0, 15.0, 2.0, 17.0].into_iter().enumerate() {
            remember(
                &dir,
                &reading(Some((pct, WINDOW_END)), None),
                NOW + i as i64 * gap,
            );
        }

        assert_eq!(
            history_rows(&dir),
            [(Some(14.0), None), (Some(15.0), None), (Some(17.0), None)]
        );
    }

    #[test]
    fn a_lower_figure_of_a_new_window_is_recorded() {
        let dir = merge_dir("history-new-window");
        let gap = crate::limits_history::MIN_GAP_SECONDS;
        remember(&dir, &reading(Some((90.0, WINDOW_END)), None), NOW);

        remember(
            &dir,
            &reading(Some((3.0, WINDOW_END + 5 * HOUR)), None),
            NOW + gap,
        );

        assert_eq!(history_rows(&dir), [(Some(90.0), None), (Some(3.0), None)]);
    }

    #[test]
    fn a_reading_with_one_accepted_figure_records_the_other_as_missing() {
        let dir = merge_dir("history-partial");
        let gap = crate::limits_history::MIN_GAP_SECONDS;
        remember(
            &dir,
            &reading(Some((30.0, WINDOW_END)), Some((40.0, WEEK_END))),
            NOW,
        );

        remember(
            &dir,
            &reading(Some((10.0, WINDOW_END)), Some((45.0, WEEK_END))),
            NOW + gap,
        );

        assert_eq!(
            history_rows(&dir),
            [(Some(30.0), Some(40.0)), (None, Some(45.0))]
        );
    }

    #[test]
    fn a_fully_rejected_reading_adds_nothing_to_the_history() {
        let dir = merge_dir("history-rejected");
        let gap = crate::limits_history::MIN_GAP_SECONDS;
        remember(
            &dir,
            &reading(Some((30.0, WINDOW_END)), Some((40.0, WEEK_END))),
            NOW,
        );
        let before = history_rows(&dir);

        remember(
            &dir,
            &reading(Some((10.0, WINDOW_END)), Some((20.0, WEEK_END - HOUR))),
            NOW + gap,
        );

        assert_eq!(before.len(), 1);
        assert_eq!(history_rows(&dir), before);
    }

    // Why no limit is shown, the crossings and the comparison in the outputs.

    const HOUR: i64 = 3600;

    fn max_plan(cache: Option<crate::plan::UsageCache>) -> Account {
        Account {
            plan: crate::plan::Plan::Max5x,
            cache,
        }
    }

    fn window_snapshot(resets_in: i64) -> Snapshot {
        Snapshot {
            captured_at: NOW - 60,
            five_hour: Some(window_at(50.0, NOW + resets_in)),
            ..Snapshot::default()
        }
    }

    #[test]
    fn each_reason_word_has_its_case_and_its_line() {
        let stale_cache = crate::plan::UsageCache {
            fetched_at: NOW - 7 * HOUR,
            ..crate::plan::UsageCache::default()
        };
        let cases: [(Sources, Option<Reason>, &str); 11] = [
            (
                Sources { account: max_plan(None), ..Sources::default() },
                Some(Reason::NoReading),
                "no limit reading yet: run a session with the playbook statusline",
            ),
            (
                Sources::default(),
                Some(Reason::CapUnknown),
                "cap not reported by Claude Code on this login",
            ),
            (
                Sources { account: max_plan(Some(stale_cache)), ..Sources::default() },
                Some(Reason::Stale),
                "the last limit reading is over 6 hours old and is not used: run a session with the playbook statusline",
            ),
            (
                Sources { snapshot: Some(window_snapshot(0)), ..Sources::default() },
                Some(Reason::WindowsReset),
                "the windows of the last reading have reset: run a session with the playbook statusline for a new one",
            ),
            (
                Sources { account: max_plan(None), rate_limited: vec![NOW - 60], ..Sources::default() },
                Some(Reason::RateLimitedSeen),
                "Claude Code was rate limited, yet it reports no limit: run a session with the playbook statusline",
            ),
            (
                Sources { rate_limited: vec![NOW - 60], ..Sources::default() },
                Some(Reason::CapUnknown),
                "cap not reported by Claude Code on this login",
            ),
            (
                Sources { snapshot: Some(window_snapshot(0)), rate_limited: vec![NOW - 60], ..Sources::default() },
                Some(Reason::WindowsReset),
                "the windows of the last reading have reset: run a session with the playbook statusline for a new one",
            ),
            (
                Sources {
                    account: max_plan(Some(crate::plan::UsageCache {
                        fetched_at: NOW - 7 * HOUR,
                        ..crate::plan::UsageCache::default()
                    })),
                    rate_limited: vec![NOW - 60],
                    ..Sources::default()
                },
                Some(Reason::Stale),
                "the last limit reading is over 6 hours old and is not used: run a session with the playbook statusline",
            ),
            (
                Sources { account: max_plan(None), rate_limited: vec![NOW - RATE_LIMIT_RECENT_SECONDS - 1], ..Sources::default() },
                Some(Reason::NoReading),
                "no limit reading yet: run a session with the playbook statusline",
            ),
            (
                Sources { snapshot: Some(window_snapshot(3600)), ..Sources::default() },
                None,
                "",
            ),
            (
                Sources { snapshot: Some(gateway_snapshot()), ..Sources::default() },
                None,
                "",
            ),
        ];

        for (sources, want, line) in cases {
            let got = reason(&sources, NOW);

            assert_eq!(got, want);
            if let Some(r) = got {
                assert_eq!(r.line(), line);
                let text = summary_section(&sources, &[], NOW, WeekStart::Monday);
                assert!(text.contains(&format!("  {line}\n")), "{text}");
                assert_eq!(
                    report_json(&sources, &[], NOW, WeekStart::Monday)["reason"],
                    r.key()
                );
            } else {
                let json = report_json(&sources, &[], NOW, WeekStart::Monday);
                assert!(json["reason"].is_null());
            }
        }
        let keys = [
            Reason::NoReading,
            Reason::Stale,
            Reason::CapUnknown,
            Reason::WindowsReset,
            Reason::RateLimitedSeen,
        ]
        .map(Reason::key);
        assert_eq!(
            keys,
            [
                "no-reading",
                "stale",
                "cap-unknown",
                "windows-reset",
                "rate-limited-seen"
            ]
        );
    }

    #[test]
    fn a_rate_limit_time_gets_its_own_line_even_when_a_limit_is_known() {
        let s = Sources {
            snapshot: Some(window_snapshot(3600)),
            rate_limited: vec![NOW - 600, NOW - 60],
            ..Sources::default()
        };

        let lines = limit_lines(&s, &all_limits(&s, &[], NOW, WeekStart::Monday), &[], NOW);

        let want = format!(
            "Claude Code was rate limited at {} (HTTP 429)",
            &stamp(NOW - 60)[..16]
        );
        assert!(lines.contains(&want), "{lines:?}");
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.contains("rate limited at"))
                .count(),
            1
        );
        let old = Sources {
            rate_limited: vec![NOW - RATE_LIMIT_RECENT_SECONDS - 1],
            ..Sources::default()
        };
        assert!(limit_lines(&old, &[], &[], NOW)
            .iter()
            .all(|l| !l.contains("rate limited at")));
        assert_eq!(reason(&s, NOW), None);
        assert!(limit_lines(&Sources::default(), &[], &[], NOW)
            .iter()
            .all(|l| !l.contains("rate limited at")));
    }

    #[test]
    fn a_manual_budget_alone_still_says_no_limit_comes_from_claude() {
        let s = Sources {
            budgets: vec![(LimitPeriod::Monthly, 200.0)],
            ..Sources::default()
        };

        assert_eq!(reason(&s, NOW), Some(Reason::CapUnknown));
    }

    #[test]
    fn the_summary_lists_when_50_80_and_100_percent_are_crossed_for_each_limit() {
        crate::tz::set(crate::tz::Zone::Utc);
        let s = Sources {
            snapshot: Some(window_snapshot(3600)),
            ..Sources::default()
        };

        let text = summary_section(&s, &[], NOW, WeekStart::Monday);

        let line = text
            .lines()
            .find(|l| l.contains("five hour crossings (measured):"))
            .unwrap_or_else(|| panic!("{text}"));
        // The window began 4 hours before NOW and ends 1 hour after it, and 50
        // percent of it is used, so 50 percent is crossed at NOW.
        let at = |secs: i64| stamp(secs);
        assert_eq!(
            line.trim(),
            format!(
                "five hour crossings (measured): 50% crossed about {}, 80% not before the period ends {end}, 100% not before the period ends {end}",
                at(NOW),
                end = at(NOW + HOUR)
            )
        );
        // 50 percent after 4 of 5 hours projects 62.5 percent at the end.
    }

    #[test]
    fn the_comparison_shows_its_age_and_a_stale_marker_over_six_hours() {
        let billed = |age: i64| {
            let mut account = cache(age, spend(1500.0, Some(1059.5)), None);
            account.cache.as_mut().unwrap().billed =
                SpendFigure::from_usd(Some(1500.0), Some(1059.5), NOW - age);
            Sources {
                account,
                ..Sources::default()
            }
        };
        let events = [cost(NOW - 8 * HOUR, 5.0)];

        let fresh = summary_section(&billed(600), &events, NOW, WeekStart::Monday);
        let stale = summary_section(&billed(7 * HOUR), &events, NOW, WeekStart::Monday);

        assert!(
            fresh.contains("reported by Claude (read 10m ago)"),
            "{fresh}"
        );
        assert!(
            stale.contains("reported by Claude (stale, read 7h"),
            "{stale}"
        );
        assert!(stale.contains("(stale)"), "{stale}");
        let j = report_json(&billed(7 * HOUR), &events, NOW, WeekStart::Monday);
        assert_eq!(j["comparison"]["stale"], true);
        assert_eq!(j["comparison"]["billed_usd"], 1059.5);
        assert!(
            report_json(&Sources::default(), &[], NOW, WeekStart::Monday)["comparison"].is_null()
        );
    }

    #[test]
    fn a_figure_that_cannot_be_compared_says_why_in_text_and_json() {
        let mut account = cache(60, None, None);
        account.cache.as_mut().unwrap().billed = Some(SpendFigure {
            limit: None,
            used: Some(crate::spend::Money {
                minor: 100,
                decimals: 2,
                currency: Some("EUR".to_string()),
            }),
            fetched_at: NOW - 60,
        });
        let s = Sources {
            account,
            ..Sources::default()
        };

        let text = summary_section(&s, &[], NOW, WeekStart::Monday);
        let json = report_json(&s, &[], NOW, WeekStart::Monday);

        assert!(
            text.contains("local estimate not compared: currency EUR, not compared"),
            "{text}"
        );
        assert_eq!(json["comparison"]["skipped"], "currency EUR, not compared");
    }

    #[test]
    fn every_limit_in_the_json_carries_three_thresholds() {
        let s = Sources {
            snapshot: Some(window_snapshot(3600)),
            ..Sources::default()
        };

        let json = report_json(&s, &[], NOW, WeekStart::Monday);

        let limits = json["limits"].as_array().unwrap();
        assert!(!limits.is_empty());
        for l in limits {
            assert_eq!(l["thresholds"].as_array().unwrap().len(), 3);
        }
    }

    /// 2027-01-01 00:00 UTC, the month that holds NOW.
    const MONTH_START: i64 = 1_798_761_600;

    #[test]
    fn the_summary_prints_the_burn_up_table_for_a_monthly_dollar_limit_only() {
        crate::tz::set(crate::tz::Zone::Utc);
        let with = Sources {
            budgets: vec![(LimitPeriod::Monthly, 100.0)],
            ..Sources::default()
        };
        let weekly = Sources {
            budgets: vec![(LimitPeriod::Weekly, 100.0)],
            ..Sources::default()
        };
        let window = Sources {
            snapshot: Some(window_snapshot(3600)),
            ..Sources::default()
        };
        let events = [
            cost(MONTH_START + 10, 25.0),
            cost(MONTH_START + 3 * 86_400, 25.0),
        ];

        let text = summary_section(&with, &events, NOW, WeekStart::Monday);
        let other = summary_section(&weekly, &events, NOW, WeekStart::Monday);
        let win = summary_section(&window, &events, NOW, WeekStart::Monday);

        assert!(
            text.contains("  burn-up, cumulative spend this period\n"),
            "{text}"
        );
        assert!(
            text.contains("    2027-01-01        $25.00     25.0%\n"),
            "{text}"
        );
        assert!(
            text.contains("    2027-01-04        $50.00     50.0%\n"),
            "{text}"
        );
        assert!(!other.contains("burn-up"), "{other}");
        assert!(!win.contains("burn-up"), "{win}");
    }

    #[test]
    fn the_json_gives_every_limit_a_series_by_its_unit() {
        crate::tz::set(crate::tz::Zone::Utc);
        let s = Sources {
            snapshot: Some(window_snapshot(3600)),
            budgets: vec![(LimitPeriod::Monthly, 100.0), (LimitPeriod::Weekly, 40.0)],
            ..Sources::default()
        };
        let events = [cost(MONTH_START + 10, 25.0)];

        let v = report_json(&s, &events, NOW, WeekStart::Monday);

        let limits = v["limits"].as_array().unwrap();
        let by = |period: &str| limits.iter().find(|l| l["period"] == period).unwrap();
        let monthly = by("monthly");
        assert_eq!(monthly["series"]["basis"], "daily");
        assert_eq!(monthly["series"]["days"][0]["day"], "2027-01-01");
        assert_eq!(monthly["series"]["days"][0]["cumulative_usd"], 25.0);
        assert!(monthly["series"]["projected_end_usd"].is_number());
        assert!(by("weekly")["series"]["days"].is_array());
        let window = by("five_hour");
        assert_eq!(window["series"]["basis"], "line");
        assert_eq!(window["series"]["points"].as_array().unwrap().len(), 2);
        assert!(window["series"]["projected_end_percent"].is_number());
    }

    #[test]
    fn a_cached_dollar_limit_gets_the_billed_anchor_from_the_cache() {
        let mut account = cache(60, spend(500.0, Some(120.0)), None);
        account.cache.as_mut().unwrap().billed =
            SpendFigure::from_usd(Some(500.0), Some(120.0), NOW - 60);
        let s = Sources {
            account,
            ..Sources::default()
        };

        assert_eq!(billed_anchor(&s, NOW), Some((NOW - 60, 120.0)));
        assert_eq!(billed_anchor(&Sources::default(), NOW), None);
    }
}
