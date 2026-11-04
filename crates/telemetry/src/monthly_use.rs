// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The monthly dollar figure the statusline budget line reports, and the one
//! `playbook usage doctor` explains. One function decides it, so the two never
//! disagree.
//!
//! Order of sources for the amount used and the limit Claude reports:
//! 1. the statusline reading, when it carries dollars and was read within
//!    [`STALE_SECONDS`] (a Claude apps gateway sends it);
//! 2. the `spend` block of the Claude Code usage cache, fetched this UTC
//!    month and not dated in the future. Older than [`STALE_SECONDS`] it is
//!    still used but marked stale with its age, since spend only grows within
//!    a month and a last month figure says nothing about this one.
//!
//! The manual `usage.budget.monthlyUsd` is a limit only. It has no amount used
//! (that needs the local usage database), so it never supplies a figure. It
//! does replace the reported limit when it is lower: your own budget may be
//! stricter than the organization's.

use crate::limits::{Limit, LimitPeriod, Origin, Unit};
use crate::limits_source::{Snapshot, STALE_SECONDS};
use crate::plan::Account;
use crate::query::utc_month_bounds;

/// Where the amount used came from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Source {
    /// The statusline reading.
    Statusline,
    /// The Claude Code usage cache, `age` seconds old. `fresh` is false past
    /// [`STALE_SECONDS`].
    Cache { age: i64, fresh: bool },
}

/// The monthly dollar figure and where it stands.
#[derive(Debug, Clone, PartialEq)]
pub struct MonthlyUse {
    /// Dollars used against the limit in force.
    pub limit: Limit,
    pub source: Source,
    /// The limit Claude reports, before the manual budget is considered.
    pub reported_usd: f64,
    /// The manual budget is lower than the reported limit and is the one in
    /// force.
    pub own_budget: bool,
}

/// The dollars Claude reports for this month and where they came from, with
/// no manual budget applied. `None` when no source has a limit and an amount
/// used above zero for this month.
pub fn detect(snapshot: Option<&Snapshot>, account: &Account, now: i64) -> Option<(Limit, Source)> {
    let from_statusline = snapshot
        .filter(|sn| now - sn.read_of(LimitPeriod::Monthly) <= STALE_SECONDS)
        .and_then(|sn| {
            sn.limits(now)
                .into_iter()
                .find(|l| l.period == LimitPeriod::Monthly && l.unit == Unit::Dollars)
        })
        .map(|l| (l, Source::Statusline));
    let from_cache = || {
        let c = account
            .cache
            .as_ref()
            .filter(|c| !c.is_from_the_future(now))?;
        let spend = c.spend?;
        let (start, end) = utc_month_bounds(now);
        (c.fetched_at >= start).then_some(())?;
        let source = Source::Cache {
            age: c.age(now),
            fresh: c.is_fresh(now),
        };
        let limit = Limit {
            unit: Unit::Dollars,
            period: LimitPeriod::Monthly,
            origin: Origin::Cached,
            used: spend.used_usd?,
            limit: spend.limit_usd,
            start,
            end,
        };
        Some((limit, source))
    };
    let usable = |f: &(Limit, Source)| f.0.used > 0.0 && f.0.limit > 0.0;
    from_statusline
        .filter(usable)
        .or_else(|| from_cache().filter(usable))
}

/// [`detect`], then the manual monthly budget in `budgets` if it is lower.
pub fn monthly_use(
    snapshot: Option<&Snapshot>,
    account: &Account,
    budgets: &[(LimitPeriod, f64)],
    now: i64,
) -> Option<MonthlyUse> {
    let (mut limit, source) = detect(snapshot, account, now)?;
    let reported_usd = limit.limit;
    let own = budgets
        .iter()
        .find(|(p, usd)| *p == LimitPeriod::Monthly && *usd > 0.0 && *usd < reported_usd);
    if let Some((_, usd)) = own {
        limit.limit = *usd;
    }
    Some(MonthlyUse {
        limit,
        source,
        reported_usd,
        own_budget: own.is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan;

    /// `fetchedAtMs` of the fixture, in seconds: 2026-09-03 in UTC.
    const FETCHED: i64 = 1_788_400_000;
    const HOUR: i64 = 3600;
    const DAY: i64 = 24 * HOUR;

    /// The Enterprise usage cache shape: both windows null, a spend block with
    /// 1500.00 USD limit and 986.40 used, and the extra fields beside it.
    fn account() -> Account {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/spend/enterprise-usage-based.json");
        plan::read(&path)
    }

    fn gateway(now: i64, used: f64, limit: f64) -> Snapshot {
        Snapshot::from_statusline(
            &format!(
                r#"{{"rate_limits":{{"spend_limit":{{"used_percentage":10,"resets_at":{},"used_usd":{used},"limit_usd":{limit},"period":"monthly"}}}}}}"#,
                now + 10 * DAY
            ),
            now,
        )
        .unwrap()
    }

    fn used_of(m: &MonthlyUse) -> (f64, f64) {
        (m.limit.used, m.limit.limit)
    }

    #[test]
    fn a_fresh_cache_gives_the_amount_and_limit_marked_fresh() {
        let now = FETCHED + HOUR;
        let m = monthly_use(None, &account(), &[], now).unwrap();
        assert_eq!(used_of(&m), (986.4, 1500.0));
        assert_eq!(
            m.source,
            Source::Cache {
                age: HOUR,
                fresh: true
            }
        );
        assert!(!m.own_budget);
        assert_eq!(m.limit.unit, Unit::Dollars);
        assert_eq!(m.limit.period, LimitPeriod::Monthly);
        assert_eq!(m.limit.origin, Origin::Cached);
    }

    #[test]
    fn trust_ends_at_the_stale_constant_and_the_figure_keeps_its_age() {
        let at = |age: i64| monthly_use(None, &account(), &[], FETCHED + age).unwrap();
        assert_eq!(
            at(STALE_SECONDS).source,
            Source::Cache {
                age: STALE_SECONDS,
                fresh: true
            }
        );
        let stale = 10 * DAY + 14 * HOUR;
        assert_eq!(
            at(stale).source,
            Source::Cache {
                age: stale,
                fresh: false
            }
        );
        assert_eq!(used_of(&at(stale)), (986.4, 1500.0));
    }

    #[test]
    fn a_figure_from_last_month_or_the_future_is_not_used() {
        assert!(monthly_use(None, &account(), &[], FETCHED + 40 * DAY).is_none());
        assert!(monthly_use(None, &account(), &[], FETCHED - DAY).is_none());
    }

    #[test]
    fn nothing_is_reported_without_a_cache_or_with_nothing_used() {
        assert!(monthly_use(None, &Account::default(), &[], FETCHED).is_none());
        let mut a = account();
        a.cache.as_mut().unwrap().spend.as_mut().unwrap().used_usd = Some(0.0);
        assert!(monthly_use(None, &a, &[], FETCHED).is_none());
        a.cache.as_mut().unwrap().spend.as_mut().unwrap().used_usd = None;
        assert!(monthly_use(None, &a, &[], FETCHED).is_none());
        a.cache.as_mut().unwrap().spend = None;
        assert!(monthly_use(None, &a, &[], FETCHED).is_none());
    }

    #[test]
    fn a_manual_budget_alone_supplies_no_figure() {
        let budgets = [(LimitPeriod::Monthly, 1000.0)];
        assert!(monthly_use(None, &Account::default(), &budgets, FETCHED).is_none());
    }

    #[test]
    fn a_lower_manual_budget_is_the_limit_in_force_and_keeps_the_reported_one() {
        let budgets = [(LimitPeriod::Monthly, 1000.0)];
        let m = monthly_use(None, &account(), &budgets, FETCHED + HOUR).unwrap();
        assert_eq!(used_of(&m), (986.4, 1000.0));
        assert!(m.own_budget);
        assert_eq!(m.reported_usd, 1500.0);
    }

    #[test]
    fn a_higher_or_other_period_manual_budget_changes_nothing() {
        for budgets in [
            [(LimitPeriod::Monthly, 2000.0)],
            [(LimitPeriod::Weekly, 10.0)],
        ] {
            let m = monthly_use(None, &account(), &budgets, FETCHED + HOUR).unwrap();
            assert_eq!(used_of(&m), (986.4, 1500.0));
            assert!(!m.own_budget);
        }
    }

    #[test]
    fn a_fresh_statusline_figure_beats_a_fresh_cache() {
        let now = FETCHED + HOUR;
        let m = monthly_use(Some(&gateway(now, 410.0, 500.0)), &account(), &[], now).unwrap();
        assert_eq!(used_of(&m), (410.0, 500.0));
        assert_eq!(m.source, Source::Statusline);
    }

    #[test]
    fn a_stale_statusline_figure_gives_way_to_the_cache() {
        let now = FETCHED + HOUR;
        let old = gateway(now - STALE_SECONDS - 1, 410.0, 500.0);
        let m = monthly_use(Some(&old), &account(), &[], now).unwrap();
        assert_eq!(used_of(&m), (986.4, 1500.0));
    }

    #[test]
    fn a_statusline_figure_with_nothing_used_falls_back_to_the_cache() {
        let now = FETCHED + HOUR;
        let m = monthly_use(Some(&gateway(now, 0.0, 500.0)), &account(), &[], now).unwrap();
        assert_eq!(used_of(&m), (986.4, 1500.0));
    }

    #[test]
    fn a_statusline_figure_without_dollars_is_skipped() {
        let now = FETCHED + HOUR;
        let pct = Snapshot::from_statusline(
            &format!(
                r#"{{"rate_limits":{{"spend_limit":{{"used_percentage":62.8,"resets_at":{}}}}}}}"#,
                now + DAY
            ),
            now,
        )
        .unwrap();
        let m = monthly_use(Some(&pct), &account(), &[], now).unwrap();
        assert_eq!(
            m.source,
            Source::Cache {
                age: HOUR,
                fresh: true
            }
        );
    }
}
