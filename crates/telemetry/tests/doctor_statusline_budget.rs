// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook usage doctor` on an Enterprise seat that is billed by usage: the
//! cache holds a monthly dollar limit and no windows. The doctor says what the
//! statusline budget line will show and why, and `--raw` adds the numbers.

use playbook_telemetry::limits::LimitPeriod;
use playbook_telemetry::plan::report;
use std::path::PathBuf;

/// `fetchedAtMs` of the fixture, in seconds.
const FETCHED: i64 = 1_788_400_000;
const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;

const FIXTURE: &str = include_str!("fixtures/spend/enterprise-usage-based.json");

struct Home(PathBuf);

impl Home {
    fn new(name: &str, body: &str) -> Home {
        let dir = std::env::temp_dir().join(format!("pb-doctor-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".claude.json"), body).unwrap();
        Home(dir)
    }

    fn report(&self, budgets: &[(LimitPeriod, f64)], age: i64, raw: bool) -> String {
        report(
            &self.0.join(".claude.json"),
            &self.0,
            budgets,
            FETCHED + age,
            raw,
        )
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const OWN: [(LimitPeriod, f64); 1] = [(LimitPeriod::Monthly, 1000.0)];

#[test]
fn a_fresh_cache_and_a_lower_own_budget_explain_which_source_wins() {
    let text = Home::new("fresh", FIXTURE).report(&OWN, HOUR, false);
    for line in [
        "What the statusline budget line will show",
        "  monthly: from the Claude Code cache, 1h 0m old (fresh)",
        "  limit in force: $1000.00, your usage.budget.monthlyUsd, which is lower than the $1500.00 Claude reports",
        "  order: statusline reading, then the Claude Code cache from this month, then nothing. Your monthly budget replaces the limit only when it is lower, and never supplies an amount used.",
    ] {
        assert!(text.contains(line), "{line}\n{text}");
    }
    assert!(
        !text.contains("986"),
        "the plain report hides the amount\n{text}"
    );
}

#[test]
fn a_higher_own_budget_leaves_the_reported_limit_in_force() {
    let budgets = [(LimitPeriod::Monthly, 3000.0)];
    let text = Home::new("higher", FIXTURE).report(&budgets, HOUR, false);
    assert!(
        text.contains("  limit in force: $1500.00 as Claude reports it"),
        "{text}"
    );
}

#[test]
fn a_stale_cache_is_named_stale_with_its_age_and_how_to_refresh_it() {
    let text = Home::new("stale", FIXTURE).report(&OWN, 10 * DAY + 14 * HOUR, false);
    assert!(
        text.contains(
            "  monthly: from the Claude Code cache, 10d 14h old (stale: shown with its age and no projection; Claude Code writes the cache when it fetches usage, which the /usage view does)"
        ),
        "{text}"
    );
}

#[test]
fn no_figure_this_month_says_nothing_is_shown() {
    let text = Home::new("old", FIXTURE).report(&OWN, 40 * DAY, false);
    assert!(
        text.contains(
            "  monthly: nothing to show (no dollar limit with an amount used this month)"
        ),
        "{text}"
    );
}

#[test]
fn an_absent_five_hour_window_is_left_out_without_a_not_known_note() {
    let text = Home::new("five", FIXTURE).report(&OWN, HOUR, false);
    assert!(
        text.contains("  5 hour window: absent in the data, so the line leaves it out"),
        "{text}"
    );
}

#[test]
fn raw_adds_the_numbers_and_field_types_and_the_plain_report_stays_short() {
    let home = Home::new("raw", FIXTURE);
    let plain = home.report(&OWN, HOUR, false);
    let raw = home.report(&OWN, HOUR, true);
    assert!(plain.contains("Add --raw to also print"), "{plain}");
    assert!(!raw.contains("Add --raw to also print"), "{raw}");
    for line in [
        "    raw: fetchedAtMs = 1788400000000",
        "    raw: field types: extra_usage object, five_hour null, limits array, member_dashboard_available bool, seven_day null, spend object",
        "    raw: monthly used $986.40 of reported limit $1500.00, own budget $1000.00, limit in force $1000.00",
    ] {
        assert!(raw.contains(line), "{line}\n{raw}");
        assert!(!plain.contains(line), "{line}\n{plain}");
    }
    assert!(raw.lines().count() > plain.lines().count());
}

#[test]
fn a_fresh_statusline_snapshot_with_dollars_is_named_as_the_source() {
    let home = Home::new("gateway", FIXTURE);
    let now = FETCHED + HOUR;
    let input = format!(
        r#"{{"rate_limits":{{"five_hour":{{"used_percentage":10,"resets_at":{}}},"spend_limit":{{"used_percentage":82,"resets_at":{},"used_usd":410,"limit_usd":500,"period":"monthly"}}}}}}"#,
        now + HOUR,
        now + 10 * DAY
    );
    playbook_telemetry::limits_source::remember(&home.0, &input, now);
    let text = home.report(&[], HOUR, true);
    assert!(
        text.contains("  monthly: from the statusline reading (fresh)"),
        "{text}"
    );
    assert!(
        text.contains("raw: monthly used $410.00 of reported limit $500.00, own budget none, limit in force $500.00"),
        "{text}"
    );
    assert!(!text.contains("5 hour window: absent"), "{text}");
}

#[test]
fn doctor_says_pbtop_keeps_the_claude_limit_when_your_budget_is_lower_and_where_to_compare() {
    let text = Home::new("pbtop", FIXTURE).report(&OWN, HOUR, false);
    for line in [
        "  your usage.budget.monthlyUsd is lower than that limit: pbtop keeps the Claude limit and the statusline budget line uses yours",
        "  to compare with this machine's own cost for the month, run `playbook usage --summary`",
    ] {
        assert!(text.contains(line), "{line}\n{text}");
    }
    let higher =
        Home::new("pbtop-higher", FIXTURE).report(&[(LimitPeriod::Monthly, 3000.0)], HOUR, false);
    assert!(!higher.contains("is lower than that limit"), "{higher}");
}
