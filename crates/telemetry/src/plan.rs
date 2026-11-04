// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The plan and the cached usage numbers Claude Code keeps in its own
//! `~/.claude.json`. [`read`] takes only the plan tier, the usage windows and
//! the spend limit, and keeps nothing that names a person or an organization.
//! [`report`] also lists the names of the account fields and, behind `raw`,
//! the values of six plan fields that hold no identity. Nothing here is
//! documented by Anthropic, so every read can come back empty and every
//! caller treats empty as "unknown".
//!
//! The plan is a guess from the rate limit tier names. Only the two tiers seen
//! on real Max accounts are recognized. Any other value, including every
//! Enterprise value, maps to [`Plan::EnterpriseOrUnknown`] instead of being
//! guessed at.

use crate::claude_code::parse_iso8601_utc;
use crate::limits::{LimitPeriod, Unit};
use crate::limits_source::{self, Window};
use crate::monthly_use;
use crate::period::elapsed_text;
use crate::spend::SpendFigure;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

/// The account fields that describe a plan and carry no identity. Only these
/// are ever shown with their values, and only behind `--raw`.
const PLAN_FIELDS: [&str; 6] = [
    "billingType",
    "organizationType",
    "seatTier",
    "organizationRateLimitTier",
    "userRateLimitTier",
    "hasExtraUsageEnabled",
];

/// How far ahead of this clock a fetch time may be before it is not believed.
const CLOCK_SKEW_SECONDS: i64 = 300;

/// What the account looks like from the tier names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    Max5x,
    Max20x,
    /// Anything else: Enterprise, Team, Pro, an API account or a value that
    /// this version does not know.
    EnterpriseOrUnknown,
}

impl Plan {
    /// A stable name for scripts.
    pub fn key(self) -> &'static str {
        match self {
            Plan::Max5x => "max_5x",
            Plan::Max20x => "max_20x",
            Plan::EnterpriseOrUnknown => "enterprise_or_unknown",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Plan::Max5x => "Max 5x",
            Plan::Max20x => "Max 20x",
            Plan::EnterpriseOrUnknown => "Enterprise or unknown plan",
        }
    }

    /// Whether this plan is known to have the 5 hour and weekly windows.
    pub fn has_windows(self) -> bool {
        !matches!(self, Plan::EnterpriseOrUnknown)
    }

    fn from_tiers(tiers: &[&str]) -> Plan {
        let has = |needle: &str| tiers.iter().any(|t| t.to_lowercase().contains(needle));
        if has("max_20x") {
            Plan::Max20x
        } else if has("max_5x") {
            Plan::Max5x
        } else {
            Plan::EnterpriseOrUnknown
        }
    }
}

/// One line a view draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisplayLine {
    /// A rolling window, by the name the source gave it.
    Window(String),
    MonthlyCap,
    /// The estimate from this machine's own history. Always drawn.
    LocalEstimate,
}

/// What the sources carry. The lines a view draws follow this set and never
/// the plan name, since only some response shapes have been observed and no
/// table of plans and limits exists.
#[derive(Debug, Clone, PartialEq)]
pub struct Capabilities {
    pub rolling_windows: Vec<(String, Window)>,
    pub monthly_cap: Option<crate::spend::Money>,
    /// Nothing from Claude is known, so only the local estimate is shown.
    pub local_only: bool,
}

impl Capabilities {
    fn build(
        rolling_windows: Vec<(String, Window)>,
        monthly_cap: Option<crate::spend::Money>,
    ) -> Capabilities {
        let local_only = rolling_windows.is_empty() && monthly_cap.is_none();
        Capabilities {
            rolling_windows,
            monthly_cap,
            local_only,
        }
    }

    /// What the statusline reading and the usage cache carry at `now`, by the
    /// same rules as [`limits_source::all_limits`]: a window already reset is
    /// not there, only a fresh cache counts, and a cache newer than the
    /// reading replaces its window. The cap follows the monthly limit that
    /// [`limits_source::monthly`] picks, so a manual budget is no cap here.
    pub fn from_sources(s: &limits_source::Sources, now: i64) -> Capabilities {
        let live = |w: &Option<Window>| w.clone().filter(|w| w.resets_at > now);
        let cache = s.account.cache.as_ref().filter(|c| c.is_fresh(now));
        let mut windows = Vec::new();
        for (name, period, from_snapshot, from_cache) in [
            (
                "five_hour",
                LimitPeriod::FiveHour,
                s.snapshot.as_ref().map(|sn| &sn.five_hour),
                cache.map(|c| &c.five_hour),
            ),
            (
                "seven_day",
                LimitPeriod::SevenDay,
                s.snapshot.as_ref().map(|sn| &sn.seven_day),
                cache.map(|c| &c.seven_day),
            ),
        ] {
            let measured = from_snapshot.and_then(live);
            let cached = from_cache.and_then(live);
            let newer = cache
                .zip(s.snapshot.as_ref())
                .is_none_or(|(c, sn)| c.fetched_at > sn.read_of(period));
            if let Some(w) = if newer {
                cached.or(measured)
            } else {
                measured.or(cached)
            } {
                windows.push((name.to_string(), w));
            }
        }
        if let Some(c) = cache {
            windows.extend(
                c.extra_windows
                    .iter()
                    .filter(|(_, w)| w.resets_at > now)
                    .cloned(),
            );
        }
        let cap = match limits_source::monthly(s, now) {
            limits_source::Monthly::Measured => s
                .snapshot
                .as_ref()
                .and_then(|sn| sn.spend_limit.as_ref())
                .and_then(|l| l.limit_usd)
                .filter(|n| *n > 0.0)
                .and_then(crate::spend::Money::from_usd),
            limits_source::Monthly::Cached { .. } => s
                .account
                .cache
                .as_ref()
                .and_then(|c| c.billed.as_ref())
                .and_then(|b| b.limit.clone())
                .filter(|m| m.minor > 0),
            _ => None,
        };
        Capabilities::build(windows, cap)
    }

    /// The lines to draw, in order: the windows, the monthly cap, then the
    /// local estimate.
    pub fn lines(&self) -> Vec<DisplayLine> {
        let mut out: Vec<DisplayLine> = self
            .rolling_windows
            .iter()
            .map(|(name, _)| DisplayLine::Window(name.clone()))
            .collect();
        if self.monthly_cap.is_some() {
            out.push(DisplayLine::MonthlyCap);
        }
        out.push(DisplayLine::LocalEstimate);
        out
    }
}

/// The monthly spend limit of an account, in dollars.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpendLimit {
    pub limit_usd: f64,
    pub used_usd: Option<f64>,
}

/// The usage numbers Claude Code last fetched.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UsageCache {
    /// When Claude Code fetched them, UTC seconds.
    pub fetched_at: i64,
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
    pub spend: Option<SpendLimit>,
    /// The same `spend` block in any currency, for the comparison with the
    /// local estimate. `spend` above keeps only a usable dollar limit.
    pub billed: Option<SpendFigure>,
    /// Every other usage window the cache carries, by its own name, sorted by
    /// it. Only plain names are kept, since a name reaches a label.
    pub extra_windows: Vec<(String, Window)>,
}

impl UsageCache {
    /// Seconds since the fetch, never negative.
    pub fn age(&self, now: i64) -> i64 {
        (now - self.fetched_at).max(0)
    }

    /// Whether the fetch time is later than `now` by more than a clock can
    /// drift. Such a time is wrong, so the numbers are never called fresh.
    pub fn is_from_the_future(&self, now: i64) -> bool {
        self.fetched_at > now + CLOCK_SKEW_SECONDS
    }

    pub fn is_fresh(&self, now: i64) -> bool {
        !self.is_from_the_future(now) && self.age(now) <= limits_source::STALE_SECONDS
    }
}

/// What was read from the config file.
#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    pub plan: Plan,
    pub cache: Option<UsageCache>,
}

impl Default for Account {
    fn default() -> Self {
        Account {
            plan: Plan::EnterpriseOrUnknown,
            cache: None,
        }
    }
}

fn load(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// An ISO 8601 time with an optional fraction and an optional `Z` or
/// `+hh:mm` / `-hh:mm` offset, as UTC seconds. Claude Code writes
/// `2026-09-03T05:00:00.123456+00:00`.
fn parse_time(text: &str) -> Option<i64> {
    let (date, time) = text.split_once('T')?;
    let cut = time.find(['.', 'Z', '+', '-']).unwrap_or(time.len());
    let base = parse_iso8601_utc(&format!("{date}T{}Z", &time[..cut]))?;
    let rest = &time[cut..];
    let offset = match rest.find(['+', '-']) {
        None => 0,
        Some(i) => {
            let sign = if rest[i..].starts_with('-') { -1 } else { 1 };
            let (h, m) = rest[i + 1..].split_once(':')?;
            sign * (h.parse::<i64>().ok()? * 3600 + m.parse::<i64>().ok()? * 60)
        }
    };
    Some(base - offset)
}

fn window(v: Option<&Value>) -> Option<Window> {
    let v = v?;
    let pct = v.get("utilization")?.as_f64()?;
    let at = parse_time(v.get("resets_at")?.as_str()?)?;
    (pct.is_finite() && pct >= 0.0).then_some(Window {
        used_percentage: pct,
        resets_at: at,
    })
}

/// The usable dollar spend limit of a figure: USD, and a limit above zero.
fn spend(billed: Option<&SpendFigure>) -> Option<SpendLimit> {
    let billed = billed?;
    Some(SpendLimit {
        limit_usd: billed.limit_usd()?,
        used_usd: billed.used_usd(),
    })
}

/// Keys of the cached usage object that are not extra windows.
const KNOWN_KEYS: [&str; 3] = ["five_hour", "seven_day", "spend"];

/// The longest window name that is kept.
const MAX_NAME_LEN: usize = 32;

/// A window name made of letters, digits and underscores only.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn cache(root: &Value) -> Option<UsageCache> {
    let c = root.get("cachedUsageUtilization")?;
    let u = c.get("utilization")?;
    let fetched_at = c.get("fetchedAtMs")?.as_i64()? / 1000;
    let billed = u
        .get("spend")
        .and_then(|s| SpendFigure::from_cache(s, fetched_at));
    let mut extra_windows: Vec<(String, Window)> = u
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(name, _)| !KNOWN_KEYS.contains(&name.as_str()) && plain_name(name))
        .filter_map(|(name, v)| window(Some(v)).map(|w| (name.clone(), w)))
        .collect();
    extra_windows.sort_by(|a, b| a.0.cmp(&b.0));
    Some(UsageCache {
        fetched_at,
        five_hour: window(u.get("five_hour")),
        seven_day: window(u.get("seven_day")),
        spend: spend(billed.as_ref()),
        billed,
        extra_windows,
    })
}

fn account(root: &Value) -> Account {
    let tier = |k: &str| root.pointer(&format!("/oauthAccount/{k}"))?.as_str();
    let tiers: Vec<&str> = ["organizationRateLimitTier", "userRateLimitTier"]
        .iter()
        .filter_map(|k| tier(k))
        .collect();
    Account {
        plan: Plan::from_tiers(&tiers),
        cache: cache(root),
    }
}

/// Reads the plan and the usage cache from the config file at `path`. A
/// missing, unreadable or odd file gives the default: unknown plan, no cache.
pub fn read(path: &Path) -> Account {
    load(path).map_or_else(Account::default, |root| account(&root))
}

/// What the last read of the config file returned, and which file state it
/// came from.
struct Seen {
    path: PathBuf,
    modified: Option<SystemTime>,
    len: u64,
    account: Account,
}

static LAST: Mutex<Option<Seen>> = Mutex::new(None);

/// Like [`read`], for the terminal view that reads on every refresh. The file
/// is parsed again only when its size or modification time changed. A file
/// that exists but does not parse, as while Claude Code is rewriting it,
/// keeps the last good answer instead of resetting the plan to unknown.
pub fn read_cached(path: &Path) -> Account {
    let Ok(meta) = std::fs::metadata(path) else {
        return Account::default();
    };
    let (modified, len) = (meta.modified().ok(), meta.len());
    let Ok(mut last) = LAST.lock() else {
        return read(path);
    };
    let same_file = |s: &Seen| s.path == path;
    if let Some(seen) = last.as_ref().filter(|s| same_file(s)) {
        if seen.modified == modified && seen.len == len {
            return seen.account.clone();
        }
    }
    match load(path) {
        Some(root) => {
            let account = account(&root);
            *last = Some(Seen {
                path: path.to_path_buf(),
                modified,
                len,
                account: account.clone(),
            });
            account
        }
        None => last
            .as_ref()
            .filter(|s| same_file(s))
            .map_or_else(Account::default, |s| s.account.clone()),
    }
}

fn names(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_object)
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}

/// A plan field value that is safe to print: a flag, a number or a short
/// lowercase tier name. Anything else could hold a name or an id.
fn plain_value(v: &Value) -> String {
    match v {
        Value::Bool(_) | Value::Number(_) => v.to_string(),
        Value::String(t)
            if !t.is_empty()
                && t.len() <= 40
                && t.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') =>
        {
            v.to_string()
        }
        _ => "<other value>".to_string(),
    }
}

fn list(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        items.join(", ")
    }
}

/// One window of the cache. `raw` is the field as stored: a value that is
/// there but did not parse is reported as such and not as absent.
fn window_line(name: &str, w: Option<&Window>, raw: Option<&Value>, now: i64) -> String {
    match w {
        None if raw.is_some_and(|v| !v.is_null()) => {
            format!("    {name}: present but not readable")
        }
        None => format!("    {name}: absent"),
        Some(w) if w.resets_at > now => format!(
            "    {name}: present, {:.0}% used, resets in {}",
            w.used_percentage,
            elapsed_text(w.resets_at - now)
        ),
        Some(w) => format!(
            "    {name}: present, its reset was {} ago, so it no longer applies",
            elapsed_text(now - w.resets_at)
        ),
    }
}

fn cache_lines(root: &Value, now: i64, raw: bool) -> Vec<String> {
    let mut out = Vec::new();
    let key = "  Usage cache (cachedUsageUtilization):";
    let Some(c) = cache(root) else {
        out.push(match root.get("cachedUsageUtilization") {
            None | Some(Value::Null) => format!("{key} not found"),
            Some(v) => format!(
                "{key} found but not readable (fields: {})",
                list(&names(Some(v)))
            ),
        });
        return out;
    };
    let state = if c.is_from_the_future(now) {
        "its time is in the future, so it is not trusted".to_string()
    } else if c.is_fresh(now) {
        "fresh".to_string()
    } else {
        "stale".to_string()
    };
    out.push(format!(
        "{key} found, {} old ({state})",
        elapsed_text(c.age(now))
    ));
    let u = |k: &str| root.pointer(&format!("/cachedUsageUtilization/utilization/{k}"));
    out.push(window_line(
        "five_hour",
        c.five_hour.as_ref(),
        u("five_hour"),
        now,
    ));
    out.push(window_line(
        "seven_day",
        c.seven_day.as_ref(),
        u("seven_day"),
        now,
    ));
    let s = u("spend").filter(|v| !v.is_null());
    out.push(match (&c.spend, s) {
        (Some(_), _) => "    monthly spend limit: present, in dollars".to_string(),
        (None, Some(s)) if s.get("enabled").and_then(Value::as_bool) == Some(false) => {
            "    monthly spend limit: the cache says it is off".to_string()
        }
        (None, Some(_)) => {
            "    monthly spend limit: present but not usable (zero or not in dollars)".to_string()
        }
        (None, None) => "    monthly spend limit: absent".to_string(),
    });
    if raw {
        if let Some(s) = &c.spend {
            let used = s
                .used_usd
                .map_or("unknown".to_string(), |n| format!("{n:.2}"));
            out.push(format!(
                "    raw: spend limit {:.2} USD, used {used} USD",
                s.limit_usd
            ));
        }
    }
    if raw {
        out.extend(raw_cache_lines(root));
    }
    let valued: Vec<String> = root
        .pointer("/cachedUsageUtilization/utilization")
        .and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, _)| k.clone())
                .collect()
        })
        .unwrap_or_default();
    out.push(format!("    fields with a value: {}", list(&valued)));
    out
}

/// The unprocessed facts about the cache: its fetch time as written and the
/// JSON type of each field, never a value that could identify anyone.
fn raw_cache_lines(root: &Value) -> Vec<String> {
    let kind = |v: &Value| match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    let mut out = Vec::new();
    if let Some(ms) = root.pointer("/cachedUsageUtilization/fetchedAtMs") {
        out.push(format!("    raw: fetchedAtMs = {}", plain_value(ms)));
    }
    if let Some(u) = root
        .pointer("/cachedUsageUtilization/utilization")
        .and_then(Value::as_object)
    {
        let mut types: Vec<String> = u
            .iter()
            .filter(|(k, _)| plain_name(k))
            .map(|(k, v)| format!("{k} {}", kind(v)))
            .collect();
        types.sort();
        out.push(format!("    raw: field types: {}", types.join(", ")));
    }
    out
}

/// What the statusline budget line will show, and which source decided it.
fn budget_lines(s: &limits_source::Sources, now: i64, raw: bool) -> Vec<String> {
    let mut out = vec!["What the statusline budget line will show".to_string()];
    let figure = monthly_use::monthly_use(s.snapshot.as_ref(), &s.account, &s.budgets, now);
    match &figure {
        None => out.push(
            "  monthly: nothing to show (no dollar limit with an amount used this month)"
                .to_string(),
        ),
        Some(m) => {
            out.push(match m.source {
                monthly_use::Source::Statusline => {
                    "  monthly: from the statusline reading (fresh)".to_string()
                }
                monthly_use::Source::Cache { age, fresh: true } => format!(
                    "  monthly: from the Claude Code cache, {} old (fresh)",
                    elapsed_text(age)
                ),
                monthly_use::Source::Cache { age, fresh: false } => format!(
                    "  monthly: from the Claude Code cache, {} old (stale: shown with its age and no projection; Claude Code writes the cache when it fetches usage, which the /usage view does)",
                    elapsed_text(age)
                ),
            });
            let in_force = limits_source::amount(Unit::Dollars, m.limit.limit);
            out.push(if m.own_budget {
                format!(
                    "  limit in force: {in_force}, your usage.budget.monthlyUsd, which is lower than the {} Claude reports",
                    limits_source::amount(Unit::Dollars, m.reported_usd)
                )
            } else {
                format!("  limit in force: {in_force} as Claude reports it")
            });
        }
    }
    let has_five_hour = s.snapshot.as_ref().is_some_and(|sn| sn.five_hour.is_some())
        || s.account
            .cache
            .as_ref()
            .is_some_and(|c| c.five_hour.is_some());
    if !has_five_hour {
        out.push("  5 hour window: absent in the data, so the line leaves it out".to_string());
    }
    out.push(
        "  order: statusline reading, then the Claude Code cache from this month, then nothing. Your monthly budget replaces the limit only when it is lower, and never supplies an amount used.".to_string(),
    );
    if let (true, Some(m)) = (raw, &figure) {
        let own = s
            .budgets
            .iter()
            .find(|(p, _)| *p == LimitPeriod::Monthly)
            .map_or("none".to_string(), |(_, usd)| format!("${usd:.2}"));
        out.push(format!(
            "    raw: monthly used ${:.2} of reported limit ${:.2}, own budget {own}, limit in force ${:.2}",
            m.limit.used, m.reported_usd, m.limit.limit
        ));
    }
    out
}

fn statusline_lines(usage_dir: &Path, now: i64) -> Vec<String> {
    let Some(s) = limits_source::load(usage_dir) else {
        return vec![
            "  Statusline reading (rate_limits.json): not found".to_string(),
            "    it appears after a session that uses `playbook statusline` on a plan with usage windows".to_string(),
        ];
    };
    let age = (now - s.oldest_read()).max(0);
    let state = if s.is_stale(now) { "stale" } else { "fresh" };
    let yes = |b: bool| if b { "present" } else { "absent" };
    vec![
        format!(
            "  Statusline reading (rate_limits.json): found, {} old ({state})",
            elapsed_text(age)
        ),
        format!("    five_hour: {}", yes(s.five_hour.is_some())),
        format!("    seven_day: {}", yes(s.seven_day.is_some())),
        format!("    spend_limit: {}", yes(s.spend_limit.is_some())),
    ]
}

/// What the panels will use, given the sources above.
fn verdict_lines(s: &limits_source::Sources, now: i64) -> Vec<String> {
    use crate::limits::Origin;
    use crate::query::WeekStart;
    let limits = limits_source::all_limits(s, &[], now, WeekStart::default());
    let mut out = vec!["What pbtop will show".to_string()];
    for (period, name) in [
        (LimitPeriod::FiveHour, "5 hour window"),
        (LimitPeriod::SevenDay, "weekly window"),
    ] {
        let from = match limits.iter().find(|l| l.period == period) {
            Some(l) if l.origin == Origin::Cached => "from the Claude Code cache",
            Some(_) => "from the statusline reading",
            None => "not known",
        };
        out.push(format!("  {name}: {from}"));
    }
    if let Some(note) = limits_source::windows_note(s, &limits) {
        out.push(format!("  {note}"));
    }
    out.push(match limits_source::monthly(s, now) {
        limits_source::Monthly::Measured => {
            "  monthly limit: from the statusline reading".to_string()
        }
        limits_source::Monthly::Cached {
            limit_usd,
            age,
            fresh,
            ..
        } => format!(
            "  monthly limit: {} from the Claude Code cache, {} old{}",
            limits_source::amount(Unit::Dollars, limit_usd),
            elapsed_text(age),
            if fresh { "" } else { " (stale)" }
        ),
        limits_source::Monthly::Manual(usd) => {
            format!(
                "  monthly limit: {} from usage.budget.monthlyUsd",
                limits_source::amount(Unit::Dollars, usd)
            )
        }
        limits_source::Monthly::Unset => {
            "  monthly limit: none (set usage.budget.monthlyUsd)".to_string()
        }
    });
    out
}

/// The text of `playbook usage doctor`: which sources of limits exist on this
/// machine. It lists field names, ages, the plan label, window percentages and
/// manual budgets. Values of the plan fields and the cached spend limit appear
/// only with `raw`. Emails, names, ids and organization names are never read
/// into the text.
pub fn report(
    claude_json: &Path,
    usage_dir: &Path,
    budgets: &[(LimitPeriod, f64)],
    now: i64,
    raw: bool,
) -> String {
    let mut out = vec!["Where pbtop can read limits on this machine".to_string()];
    match load(claude_json) {
        None => out.push("  Claude config file: not found or not readable".to_string()),
        Some(root) => {
            out.push("  Claude config file: found".to_string());
            let fields = root.get("oauthAccount");
            out.push(format!(
                "    account fields present (names only): {}",
                list(&names(fields))
            ));
            out.push(format!("    plan: {}", account(&root).plan.label()));
            if raw {
                for key in PLAN_FIELDS {
                    let value = fields
                        .and_then(|a| a.get(key))
                        .map_or("absent".to_string(), plain_value);
                    out.push(format!("    raw: {key} = {value}"));
                }
            }
            out.extend(cache_lines(&root, now, raw));
        }
    }
    out.extend(statusline_lines(usage_dir, now));
    out.push("  Manual budgets (usage.budget.*):".to_string());
    if budgets.is_empty() {
        out.push("    none set".to_string());
    }
    for (period, usd) in budgets {
        out.push(format!("    {}: ${usd:.2}", period.key()));
    }
    let sources =
        limits_source::Sources::new(Some(usage_dir), claude_json, budgets.to_vec(), false);
    out.push(String::new());
    out.extend(verdict_lines(&sources, now));
    out.push(String::new());
    out.extend(budget_lines(&sources, now, raw));
    if !raw {
        out.push(String::new());
        out.push(
            "Add --raw to also print the plan field values and the cached spend limit (no email, name or id).".to_string(),
        );
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits_source::Snapshot;
    use std::path::PathBuf;

    const NOW: i64 = 1_788_400_000;
    // 2026-09-03T05:00:00Z, 3h 13m after NOW.
    const RESET: &str = "2026-09-03T05:00:00.123456+00:00";

    /// A folder holding one `.claude.json`, removed when it goes out of scope.
    struct Home(PathBuf);

    impl Home {
        fn new(name: &str, body: &str) -> Home {
            let dir = std::env::temp_dir().join(format!("pb-plan-{}-{name}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(".claude.json"), body).unwrap();
            Home(dir)
        }

        fn json(&self) -> PathBuf {
            self.0.join(".claude.json")
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn cache_body(fetched_ms: i64, spend: &str) -> String {
        format!(
            r#"{{"oauthAccount":{{"organizationRateLimitTier":"default_claude_max_5x",
              "emailAddress":"a@b.c","displayName":"Sentinel Name","accountUuid":"sentinel-uuid-1",
              "organizationName":"Sentinel Org","organizationUuid":"sentinel-uuid-2"}},
            "cachedUsageUtilization":{{"fetchedAtMs":{fetched_ms},"utilization":{{
              "five_hour":{{"utilization":40,"resets_at":"{RESET}"}},
              "seven_day":null,
              "spend":{spend}}}}}}}"#
        )
    }

    fn report_of(home: &Home, now: i64, raw: bool) -> String {
        report(&home.json(), &home.0, &[], now, raw)
    }

    #[test]
    fn a_max_tier_name_gives_that_plan() {
        let max5 = Plan::from_tiers(&["default_claude_max_5x"]);
        let max20 = Plan::from_tiers(&["x", "default_claude_max_20x"]);

        assert_eq!(max5, Plan::Max5x);
        assert_eq!(max20, Plan::Max20x);
    }

    #[test]
    fn any_other_tier_name_is_enterprise_or_unknown() {
        let plan = Plan::from_tiers(&["default_raven", "something_new"]);

        assert_eq!(plan, Plan::EnterpriseOrUnknown);
        assert_eq!(plan.label(), "Enterprise or unknown plan");
        assert!(!plan.has_windows());
    }

    #[test]
    fn a_missing_file_reads_as_an_unknown_plan_with_no_cache() {
        let account = read(Path::new("/nonexistent/.claude.json"));

        assert_eq!(account, Account::default());
    }

    #[test]
    fn a_file_that_is_not_json_reads_as_unknown() {
        let home = Home::new("garbage", "not json {");

        assert_eq!(read(&home.json()), Account::default());
    }

    #[test]
    fn the_cache_gives_windows_and_a_dollar_spend_limit() {
        let spend = r#"{"enabled":true,"limit":{"amount_minor":50000,"currency":"USD","exponent":2},"used":{"amount_minor":1234,"currency":"USD","exponent":2}}"#;
        let home = Home::new("full", &cache_body(NOW * 1000, spend));

        let account = read(&home.json());

        let c = account.cache.unwrap();
        assert_eq!(account.plan, Plan::Max5x);
        assert_eq!(c.fetched_at, NOW);
        let w = c.five_hour.unwrap();
        assert_eq!(w.used_percentage, 40.0);
        assert_eq!(w.resets_at, 1_788_411_600);
        assert!(c.seven_day.is_none());
        assert_eq!(
            c.spend,
            Some(SpendLimit {
                limit_usd: 500.0,
                used_usd: Some(12.34)
            })
        );
    }

    #[test]
    fn a_figure_in_another_currency_is_kept_as_billed_but_is_no_dollar_limit() {
        let eur = r#"{"enabled":true,"limit":{"amount_minor":50000,"currency":"EUR","exponent":2},"used":{"amount_minor":1234,"currency":"EUR","exponent":2}}"#;
        let home = Home::new("billed-eur", &cache_body(NOW * 1000, eur));

        let c = read(&home.json()).cache.unwrap();

        assert!(c.spend.is_none());
        let used = c.billed.unwrap().used.unwrap();
        assert_eq!(used.currency.as_deref(), Some("EUR"));
        assert_eq!(used.minor, 1234);
    }

    #[test]
    fn a_dollar_figure_is_kept_as_billed_too() {
        let usd = r#"{"enabled":true,"limit":{"amount_minor":50000,"currency":"USD","exponent":2},"used":{"amount_minor":1234,"currency":"USD","exponent":2}}"#;
        let home = Home::new("billed-usd", &cache_body(NOW * 1000, usd));

        let c = read(&home.json()).cache.unwrap();

        assert_eq!(
            c.billed,
            crate::spend::SpendFigure::from_usd(Some(500.0), Some(12.34), NOW)
        );
    }

    #[test]
    fn a_spend_limit_that_is_off_or_not_in_dollars_is_ignored() {
        let off =
            r#"{"enabled":false,"limit":{"amount_minor":50000,"currency":"USD","exponent":2}}"#;
        let eur =
            r#"{"enabled":true,"limit":{"amount_minor":50000,"currency":"EUR","exponent":2}}"#;
        let zero = r#"{"enabled":true,"limit":{"amount_minor":0,"currency":"USD","exponent":2}}"#;

        for (name, body) in [("off", off), ("eur", eur), ("zero", zero)] {
            let home = Home::new(name, &cache_body(NOW * 1000, body));

            assert!(read(&home.json()).cache.unwrap().spend.is_none(), "{name}");
        }
    }

    #[test]
    fn times_with_a_fraction_an_offset_or_a_z_all_parse() {
        let z = parse_time("2026-09-03T05:00:00Z");
        let fraction = parse_time("2026-09-03T05:00:00.5Z");
        let zero_offset = parse_time("2026-09-03T05:00:00+00:00");
        let plus = parse_time("2026-09-03T07:00:00+02:00");
        let minus = parse_time("2026-09-03T00:00:00-05:00");

        for t in [fraction, zero_offset, plus, minus] {
            assert_eq!(t, z);
        }
        assert_eq!(z, Some(1_788_411_600));
        assert_eq!(parse_time("soon"), None);
    }

    #[test]
    fn malformed_windows_and_amounts_are_none() {
        let windows = [
            r#"{"utilization":-1,"resets_at":"2026-09-03T05:00:00Z"}"#,
            r#"{"utilization":"40","resets_at":"2026-09-03T05:00:00Z"}"#,
            r#"{"utilization":40,"resets_at":"tomorrow"}"#,
            r#"{"utilization":40,"resets_at":1788411600}"#,
        ];
        let amounts = [
            r#"{"amount_minor":-5,"currency":"USD","exponent":2}"#,
            r#"{"amount_minor":5,"currency":"USD","exponent":9}"#,
            r#"{"amount_minor":5,"currency":"USD","exponent":-1}"#,
            r#"{"amount_minor":"5","currency":"USD","exponent":2}"#,
        ];

        for w in windows {
            assert!(
                window(serde_json::from_str(w).ok().as_ref()).is_none(),
                "{w}"
            );
        }
        for a in amounts {
            assert!(
                crate::spend::Money::from_json(serde_json::from_str(a).ok().as_ref()).is_none(),
                "{a}"
            );
        }
    }

    #[test]
    fn the_cache_is_fresh_for_six_hours_and_then_stale() {
        let c = UsageCache {
            fetched_at: NOW - 6 * 3600,
            ..UsageCache::default()
        };

        assert!(c.is_fresh(NOW));
        assert!(!c.is_fresh(NOW + 1));
    }

    #[test]
    fn a_fetch_time_in_the_future_is_never_fresh() {
        let c = UsageCache {
            fetched_at: NOW + 3600,
            ..UsageCache::default()
        };

        assert!(c.is_from_the_future(NOW));
        assert!(!c.is_fresh(NOW));
    }

    #[test]
    fn the_report_lists_field_names_and_ages_but_no_values() {
        let home = Home::new("report", &cache_body((NOW - 7200) * 1000, "null"));

        let text = report_of(&home, NOW, false);

        assert!(text.contains("organizationRateLimitTier"), "{text}");
        assert!(text.contains("plan: Max 5x"), "{text}");
        assert!(text.contains("found, 2h 0m old (fresh)"), "{text}");
        assert!(
            text.contains("five_hour: present, 40% used, resets in 3h 13m"),
            "{text}"
        );
        assert!(text.contains("seven_day: absent"), "{text}");
        assert!(text.contains("monthly spend limit: absent"), "{text}");
        assert!(!text.contains("default_claude_max_5x"), "{text}");
        assert!(!text.contains("raw:"), "{text}");
    }

    #[test]
    fn neither_report_prints_an_email_name_id_or_organization() {
        let home = Home::new("privacy", &cache_body(NOW * 1000, "null"));

        for raw in [false, true] {
            let text = report_of(&home, NOW, raw);

            for secret in [
                "a@b.c",
                "Sentinel Name",
                "sentinel-uuid-1",
                "Sentinel Org",
                "sentinel-uuid-2",
            ] {
                assert!(!text.contains(secret), "raw={raw} {secret}: {text}");
            }
        }
    }

    #[test]
    fn the_raw_report_adds_plan_field_values_and_the_spend_limit() {
        let spend = r#"{"enabled":true,"limit":{"amount_minor":50000,"currency":"USD","exponent":2},"used":{"amount_minor":1234,"currency":"USD","exponent":2}}"#;
        let home = Home::new("raw", &cache_body(NOW * 1000, spend));

        let text = report(
            &home.json(),
            &home.0,
            &[(LimitPeriod::Monthly, 100.0)],
            NOW,
            true,
        );

        assert!(
            text.contains(r#"organizationRateLimitTier = "default_claude_max_5x""#),
            "{text}"
        );
        assert!(text.contains("billingType = absent"), "{text}");
        assert!(
            text.contains("raw: spend limit 500.00 USD, used 12.34 USD"),
            "{text}"
        );
        assert!(text.contains("monthly: $100.00"), "{text}");
    }

    #[test]
    fn a_raw_plan_value_that_is_not_a_plain_tier_name_is_hidden() {
        let body = r#"{"oauthAccount":{"organizationType":"Acme Corp (id 7)","seatTier":{"name":"x"},"billingType":"stripe_subscription"}}"#;
        let home = Home::new("odd", body);

        let text = report_of(&home, NOW, true);

        assert!(text.contains("organizationType = <other value>"), "{text}");
        assert!(text.contains("seatTier = <other value>"), "{text}");
        assert!(
            text.contains(r#"billingType = "stripe_subscription""#),
            "{text}"
        );
        assert!(!text.contains("Acme"), "{text}");
    }

    #[test]
    fn a_window_past_its_reset_says_so() {
        let home = Home::new("past", &cache_body(NOW * 1000, "null"));

        let text = report_of(&home, NOW + 5 * 3600, false);

        assert!(
            text.contains("five_hour: present, its reset was 1h 46m ago"),
            "{text}"
        );
        assert!(!text.contains("% used"), "{text}");
    }

    #[test]
    fn a_window_that_does_not_parse_is_present_but_not_readable() {
        let body = r#"{"cachedUsageUtilization":{"fetchedAtMs":1788400000000,"utilization":{"five_hour":{"utilization":"lots"}}}}"#;
        let home = Home::new("unreadable-window", body);

        let text = report_of(&home, NOW, false);

        assert!(
            text.contains("five_hour: present but not readable"),
            "{text}"
        );
    }

    #[test]
    fn a_cache_without_a_fetch_time_is_found_but_not_readable() {
        let body = r#"{"cachedUsageUtilization":{"utilization":{"five_hour":null}}}"#;
        let home = Home::new("no-time", body);

        let text = report_of(&home, NOW, false);

        assert!(
            text.contains("found but not readable (fields: utilization)"),
            "{text}"
        );
    }

    #[test]
    fn a_fetch_time_in_the_future_is_called_out() {
        let home = Home::new("future", &cache_body((NOW + 7200) * 1000, "null"));

        let text = report_of(&home, NOW, false);

        assert!(
            text.contains("its time is in the future, so it is not trusted"),
            "{text}"
        );
    }

    #[test]
    fn each_spend_state_has_its_own_line() {
        let cases = [
            (
                "on",
                r#"{"enabled":true,"limit":{"amount_minor":100,"currency":"USD","exponent":2}}"#,
                "present, in dollars",
            ),
            (
                "off",
                r#"{"enabled":false,"limit":{"amount_minor":100,"currency":"USD","exponent":2}}"#,
                "the cache says it is off",
            ),
            (
                "eur",
                r#"{"enabled":true,"limit":{"amount_minor":100,"currency":"EUR","exponent":2}}"#,
                "present but not usable",
            ),
        ];

        for (name, spend, expected) in cases {
            let home = Home::new(&format!("spend-{name}"), &cache_body(NOW * 1000, spend));

            let text = report_of(&home, NOW, false);

            assert!(
                text.contains(&format!("monthly spend limit: {expected}")),
                "{name}: {text}"
            );
        }
    }

    #[test]
    fn a_statusline_reading_is_listed_with_its_age_and_windows() {
        let home = Home::new("statusline", &cache_body(NOW * 1000, "null"));
        let snap = Snapshot {
            captured_at: NOW - 600,
            five_hour: Some(Window {
                used_percentage: 10.0,
                resets_at: NOW + 3600,
            }),
            ..Snapshot::default()
        };
        limits_source::save(&home.0, &snap);

        let text = report_of(&home, NOW, false);

        assert!(
            text.contains("Statusline reading (rate_limits.json): found, 10m old (fresh)"),
            "{text}"
        );
        assert!(text.contains("five_hour: present"), "{text}");
        assert!(text.contains("spend_limit: absent"), "{text}");
    }

    #[test]
    fn the_report_ends_with_what_pbtop_will_show() {
        let spend =
            r#"{"enabled":true,"limit":{"amount_minor":50000,"currency":"USD","exponent":2}}"#;
        let home = Home::new("verdict", &cache_body((NOW - 600) * 1000, spend));

        let text = report_of(&home, NOW, false);

        assert!(text.contains("What pbtop will show"), "{text}");
        assert!(
            text.contains("5 hour window: from the Claude Code cache"),
            "{text}"
        );
        assert!(text.contains("weekly window: not known"), "{text}");
        assert!(
            text.contains("monthly limit: $500.00 from the Claude Code cache, 10m old"),
            "{text}"
        );
    }

    #[test]
    fn an_unknown_plan_with_no_windows_says_not_available() {
        let home = Home::new(
            "verdict-enterprise",
            r#"{"oauthAccount":{"organizationType":"x"}}"#,
        );

        let text = report(
            &home.json(),
            &home.0,
            &[(LimitPeriod::Monthly, 80.0)],
            NOW,
            false,
        );

        assert!(text.contains("5 hour window: not known"), "{text}");
        assert!(text.contains("not available for this plan"), "{text}");
        assert!(
            text.contains("monthly limit: $80.00 from usage.budget.monthlyUsd"),
            "{text}"
        );
    }

    #[test]
    fn a_missing_config_file_is_reported_without_failing() {
        let dir = std::env::temp_dir().join(format!("pb-plan-{}-none", std::process::id()));

        let text = report(&dir.join(".claude.json"), &dir, &[], NOW, false);

        assert!(text.contains("Claude config file: not found"), "{text}");
        assert!(
            text.contains("Statusline reading (rate_limits.json): not found"),
            "{text}"
        );
    }

    #[test]
    fn a_stale_cache_and_no_budget_say_so_in_the_verdict() {
        let spend =
            r#"{"enabled":true,"limit":{"amount_minor":50000,"currency":"USD","exponent":2}}"#;
        let stale = Home::new(
            "verdict-stale",
            &cache_body((NOW - 48 * 3600) * 1000, spend),
        );
        let none = Home::new("verdict-none", &cache_body(NOW * 1000, "null"));

        let stale_text = report_of(&stale, NOW, false);
        let none_text = report_of(&none, NOW, false);

        assert!(
            stale_text.contains("$500.00 from the Claude Code cache, 2d 0h old (stale)"),
            "{stale_text}"
        );
        assert!(
            none_text.contains("monthly limit: none (set usage.budget.monthlyUsd)"),
            "{none_text}"
        );
    }

    #[test]
    fn a_statusline_monthly_limit_is_named_in_the_verdict() {
        let home = Home::new("verdict-measured", &cache_body(NOW * 1000, "null"));
        let snap = Snapshot {
            captured_at: NOW - 60,
            spend_limit: Some(limits_source::Spend {
                used_percentage: 10.0,
                resets_at: NOW + 86_400,
                used_usd: Some(10.0),
                limit_usd: Some(100.0),
                period: Some(LimitPeriod::Monthly),
            }),
            ..Snapshot::default()
        };
        limits_source::save(&home.0, &snap);

        let text = report_of(&home, NOW, false);

        assert!(
            text.contains("monthly limit: from the statusline reading"),
            "{text}"
        );
    }

    #[test]
    fn a_file_that_stops_parsing_keeps_the_last_good_answer() {
        let home = Home::new("cached-read", &cache_body(NOW * 1000, "null"));
        let first = read_cached(&home.json());

        std::fs::write(home.json(), "{ half a file").unwrap();
        let during = read_cached(&home.json());
        std::fs::write(
            home.json(),
            r#"{"oauthAccount":{"organizationRateLimitTier":"default_claude_max_20x"}}"#,
        )
        .unwrap();
        let after = read_cached(&home.json());

        assert_eq!(first.plan, Plan::Max5x);
        assert_eq!(during, first);
        assert_eq!(after.plan, Plan::Max20x);
    }

    #[test]
    fn a_missing_file_reads_as_unknown_through_the_cache_too() {
        let missing =
            std::env::temp_dir().join(format!("pb-plan-{}-gone/.claude.json", std::process::id()));

        assert_eq!(read_cached(&missing), Account::default());
    }

    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/spend")
            .join(format!("synthetic-unverified-{name}.json"));
        std::fs::read_to_string(path).unwrap()
    }

    fn sources_of(name: &str) -> limits_source::Sources {
        let home = Home::new(&format!("caps-{name}"), &fixture(name));
        limits_source::Sources {
            account: read(&home.json()),
            ..limits_source::Sources::default()
        }
    }

    fn lines_of(name: &str) -> Vec<DisplayLine> {
        Capabilities::from_sources(&sources_of(name), NOW).lines()
    }

    fn win(name: &str) -> DisplayLine {
        DisplayLine::Window(name.to_string())
    }

    #[test]
    fn the_lines_follow_what_the_cache_carries_not_the_plan_name() {
        use DisplayLine::{LocalEstimate, MonthlyCap};
        let table = [
            (
                "windows-only",
                vec![win("five_hour"), win("seven_day"), LocalEstimate],
            ),
            (
                "windows-and-cap",
                vec![
                    win("five_hour"),
                    win("seven_day"),
                    MonthlyCap,
                    LocalEstimate,
                ],
            ),
            ("cap-only", vec![MonthlyCap, LocalEstimate]),
            ("empty", vec![LocalEstimate]),
            (
                "unknown-extra-window",
                vec![win("five_hour"), win("seven_day_opus"), LocalEstimate],
            ),
        ];

        for (name, want) in table {
            assert_eq!(lines_of(name), want, "{name}");
        }
    }

    #[test]
    fn a_cache_with_nothing_is_local_only_and_a_window_or_cap_is_not() {
        let caps = |name: &str| Capabilities::from_sources(&sources_of(name), NOW);

        assert!(caps("empty").local_only);
        assert!(!caps("cap-only").local_only);
        assert!(!caps("windows-only").local_only);
        assert_eq!(
            caps("windows-and-cap").monthly_cap.unwrap().format(),
            "500.00"
        );
    }

    #[test]
    fn a_zero_cap_is_no_cap() {
        let mut s = sources_of("cap-only");
        s.account.cache.as_mut().unwrap().billed =
            crate::spend::SpendFigure::from_usd(Some(0.0), Some(1.0), NOW);
        s.account.cache.as_mut().unwrap().spend = Some(SpendLimit {
            limit_usd: 0.0,
            used_usd: None,
        });

        assert_eq!(
            Capabilities::from_sources(&s, NOW).lines(),
            vec![DisplayLine::LocalEstimate]
        );
    }

    #[test]
    fn an_extra_window_needs_a_plain_name_and_a_readable_window() {
        let body = |name: &str| {
            format!(
                r#"{{"cachedUsageUtilization":{{"fetchedAtMs":{},"utilization":{{"{name}":{{"utilization":3,"resets_at":"{RESET}"}}}}}}}}"#,
                NOW * 1000
            )
        };

        for (name, kept) in [
            ("seven_day_opus", true),
            ("bad name", false),
            ("x\\u001b[31m", false),
        ] {
            let home = Home::new(&format!("extra-{}", name.len()), &body(name));

            let c = read(&home.json()).cache.unwrap();

            assert_eq!(!c.extra_windows.is_empty(), kept, "{name}");
        }
    }

    fn w(used: f64, resets_in: i64) -> Window {
        Window {
            used_percentage: used,
            resets_at: NOW + resets_in,
        }
    }

    fn with(
        snapshot: limits_source::Snapshot,
        cache: Option<UsageCache>,
    ) -> limits_source::Sources {
        limits_source::Sources {
            snapshot: Some(snapshot),
            account: Account {
                plan: Plan::EnterpriseOrUnknown,
                cache,
            },
            ..limits_source::Sources::default()
        }
    }

    #[test]
    fn the_windows_merge_like_the_limits_do_and_keep_one_order() {
        // The reading is newer than the cache, so it wins where it has a window
        // and the cache fills in the other one. Order stays five_hour first.
        let snapshot = limits_source::Snapshot {
            captured_at: NOW,
            seven_day: Some(w(20.0, 3600)),
            ..limits_source::Snapshot::default()
        };
        let cache = UsageCache {
            fetched_at: NOW - 60,
            five_hour: Some(w(99.0, 3600)),
            seven_day: Some(w(50.0, 3600)),
            ..UsageCache::default()
        };

        let caps = Capabilities::from_sources(&with(snapshot, Some(cache)), NOW);

        assert_eq!(
            caps.rolling_windows,
            vec![
                ("five_hour".to_string(), w(99.0, 3600)),
                ("seven_day".to_string(), w(20.0, 3600))
            ]
        );
    }

    #[test]
    fn a_newer_fresh_cache_replaces_the_reading_and_a_stale_or_reset_one_is_not_there() {
        let reading = |captured_at| limits_source::Snapshot {
            captured_at,
            five_hour: Some(w(10.0, 3600)),
            ..limits_source::Snapshot::default()
        };
        let cache = |fetched_at, resets_in| UsageCache {
            fetched_at,
            five_hour: Some(w(90.0, resets_in)),
            ..UsageCache::default()
        };
        let five = |s: limits_source::Sources| {
            Capabilities::from_sources(&s, NOW)
                .rolling_windows
                .first()
                .map(|(_, w)| w.used_percentage)
        };

        assert_eq!(
            five(with(reading(NOW - 100), Some(cache(NOW, 3600)))),
            Some(90.0)
        );
        assert_eq!(
            five(with(reading(NOW), Some(cache(NOW - 100, 3600)))),
            Some(10.0)
        );
        // A cache over six hours old is ignored.
        assert_eq!(
            five(with(reading(NOW - 100), Some(cache(NOW - 7 * 3600, 3600)))),
            Some(10.0)
        );
        // A window whose reset time has come is dropped, from either source.
        assert_eq!(
            five(with(reading(NOW - 100), Some(cache(NOW, 0)))),
            Some(10.0)
        );
        let mut gone = reading(NOW);
        gone.five_hour = Some(w(10.0, 0));
        assert_eq!(five(with(gone, None)), None);
    }

    #[test]
    fn extra_windows_come_sorted_and_a_cap_follows_the_monthly_limit_in_use() {
        let body = format!(
            r#"{{"cachedUsageUtilization":{{"fetchedAtMs":{},"utilization":{{
              "zeta":{{"utilization":1,"resets_at":"{RESET}"}},
              "alpha":{{"utilization":2,"resets_at":"{RESET}"}},
              "spend":{{"enabled":true,"limit":{{"amount_minor":50000,"currency":"USD","exponent":2}}}}}}}}}}"#,
            NOW * 1000
        );
        let home = Home::new("extras", &body);
        let mut s = limits_source::Sources {
            account: read(&home.json()),
            ..limits_source::Sources::default()
        };

        let caps = Capabilities::from_sources(&s, NOW);

        let names: Vec<_> = caps
            .rolling_windows
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        assert_eq!(names, ["alpha", "zeta"]);
        assert!(caps.monthly_cap.is_some());

        // A fresh statusline monthly limit is the one in use, and a weekly one
        // is no monthly cap.
        s.snapshot = Some(limits_source::Snapshot {
            captured_at: NOW,
            spend_limit: Some(limits_source::Spend {
                used_percentage: 10.0,
                resets_at: NOW + 3600,
                used_usd: Some(1.0),
                limit_usd: Some(100.0),
                period: Some(LimitPeriod::Monthly),
            }),
            ..limits_source::Snapshot::default()
        });
        assert_eq!(
            Capabilities::from_sources(&s, NOW)
                .monthly_cap
                .unwrap()
                .format(),
            "100.00"
        );
        s.snapshot
            .as_mut()
            .unwrap()
            .spend_limit
            .as_mut()
            .unwrap()
            .period = Some(LimitPeriod::Weekly);
        assert_eq!(
            Capabilities::from_sources(&s, NOW)
                .monthly_cap
                .unwrap()
                .format(),
            "500.00"
        );
        // A manual budget alone is no cap.
        let manual = limits_source::Sources {
            budgets: vec![(LimitPeriod::Monthly, 200.0)],
            ..limits_source::Sources::default()
        };
        assert!(Capabilities::from_sources(&manual, NOW).local_only);
    }
}
