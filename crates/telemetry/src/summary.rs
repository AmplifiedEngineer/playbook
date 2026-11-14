// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Plain-text usage report for the terminal. Structured on purpose: whatever
//! agent is driving the session can read it and give the insights itself.

use super::aggregate::{count_tools, date_key, group_usage, unpriced_models, Dimension, Group};
use super::period::{elapsed_text, Change, Comparison, Period};
use super::query::{day_of, in_range, Range};
use super::{ToolInvocationEvent, ToolKind, UsageEvent};
use std::fmt::Write;

const RECENT_DAYS: usize = 14;
const TOP_TOOLS: usize = 10;

const EMPTY_MESSAGE: &str =
    "No usage recorded yet. Run a Claude Code session, then run `playbook usage` again.";

fn group_table(out: &mut String, title: &str, groups: &[Group]) {
    let _ = writeln!(out, "\nBy {title}");
    let width = groups.iter().map(|g| g.key.len()).max().unwrap_or(0).max(8);
    let _ = writeln!(
        out,
        "  {:<width$}  {:>8}  {:>12}  {:>12}  {:>14}  {:>10}",
        "", "messages", "input", "output", "cache r/w", "cost USD"
    );
    for g in groups {
        let cache = format!("{}/{}", g.cache_read_tokens, g.cache_creation_tokens);
        let _ = writeln!(
            out,
            "  {:<width$}  {:>8}  {:>12}  {:>12}  {:>14}  {:>10.4}",
            g.key, g.messages, g.input_tokens, g.output_tokens, cache, g.cost_usd
        );
    }
}

fn tool_list(out: &mut String, title: &str, counts: &[(String, u64)]) {
    if counts.is_empty() {
        return;
    }
    let _ = writeln!(out, "\n{title}");
    for (name, n) in counts.iter().take(TOP_TOOLS) {
        let _ = writeln!(out, "  {n:>6}  {name}");
    }
}

/// The accounts as `(key, name)` pairs. Passing them to a render function
/// masks the account table the way the pbtop header does.
pub type MaskChoices<'a> = Option<&'a [(String, String)]>;

/// Rewrites each account key to its header text, so an unnamed email shows only
/// its first domain label. Named accounts keep their name.
fn mask_accounts(groups: &mut [Group], choices: &[(String, String)]) {
    for group in groups {
        let name = crate::account_store::display(choices, &group.key);
        group.key = crate::tui::account::header_label(&group.key, &name, choices);
    }
}

pub fn render(usage: &[UsageEvent], tools: &[ToolInvocationEvent], mask: MaskChoices) -> String {
    scoped(
        usage,
        tools,
        mask,
        "across all recorded sessions",
        crate::util::time::now_secs(),
    )
}

/// The report for the events inside `range`, with its dates in the heading.
pub fn render_range(
    usage: &[UsageEvent],
    tools: &[ToolInvocationEvent],
    range: Range,
    now: i64,
    mask: MaskChoices,
) -> String {
    let all_usage = usage;
    let (usage, tools) = in_range(usage, tools, now, range);
    let scope = match range.days(now) {
        Some((first, past)) => format!(
            "in {} ({} to {}, {})",
            range.label(),
            date_key(first),
            date_key(past - 1),
            crate::tz::label()
        ),
        None => "across all recorded sessions".to_string(),
    };
    if usage.is_empty() && tools.is_empty() {
        return format!(
            "No usage {}.\n",
            scope.replace("across all recorded sessions", "recorded")
        );
    }
    let mut out = scoped(&usage, &tools, mask, &scope, now);
    if let Some(period) = Period::of_range(range) {
        out.push_str(&comparison_text(&Comparison::of(all_usage, now, period)));
    }
    out
}

/// `+2.20 (+21.6%)`, or `+2.20 (n/a)` when the previous value was zero.
fn signed(change: Change, decimals: usize) -> String {
    let percent = change
        .percent
        .map_or("n/a".to_string(), |p| format!("{p:+.1}%"));
    format!("{:+.decimals$} ({percent})", change.amount)
}

/// The period against the one before it over the same elapsed span.
fn comparison_text(c: &Comparison) -> String {
    let w = c.windows;
    let mut out = format!(
        "\nCompared with {} (the same {}, {} to {} {})\n",
        c.period.previous_label(),
        elapsed_text(w.current.seconds()),
        date_key(day_of(w.previous.start)),
        date_key(day_of(w.previous.end - 1)),
        crate::tz::label(),
    );
    let rows = [
        (
            "cost USD",
            c.current.cost_usd,
            c.previous.cost_usd,
            4,
            c.cost(),
        ),
        (
            "tokens",
            c.current.tokens() as f64,
            c.previous.tokens() as f64,
            0,
            c.tokens(),
        ),
        (
            "messages",
            c.current.messages as f64,
            c.previous.messages as f64,
            0,
            c.messages(),
        ),
    ];
    for (name, now, before, decimals, change) in rows {
        let _ = writeln!(
            out,
            "  {name:<9} {now:>14.decimals$} vs {before:>14.decimals$}  {}",
            signed(change, decimals)
        );
    }
    out
}

fn scoped(
    usage: &[UsageEvent],
    tools: &[ToolInvocationEvent],
    mask: MaskChoices,
    scope: &str,
    now: i64,
) -> String {
    if usage.is_empty() && tools.is_empty() {
        return format!("{EMPTY_MESSAGE}\n");
    }
    let total_cost: f64 = usage.iter().map(|e| e.cost_usd).sum();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Usage: {} messages, ${:.4} estimated cost {scope}",
        usage.len(),
        total_cost
    );
    let _ = writeln!(out, "Times and days are in {}.", crate::tz::describe(now));
    let (unpriced, models) = unpriced_models(usage);
    if unpriced > 0 {
        let _ = writeln!(
            out,
            "Note: {unpriced} messages are unpriced (no known price for {}); they count as $0.",
            models.join(", ")
        );
    }

    let days = group_usage(usage, Dimension::Day);
    let recent = days.len().saturating_sub(RECENT_DAYS);
    group_table(
        &mut out,
        &format!("day ({}, last {RECENT_DAYS})", crate::tz::label()),
        &days[recent..],
    );
    for dim in [
        Dimension::Week,
        Dimension::Model,
        Dimension::Repo,
        Dimension::Branch,
        Dimension::Effort,
        Dimension::Account,
    ] {
        let mut groups = group_usage(usage, dim);
        if let (Dimension::Account, Some(choices)) = (dim, mask) {
            mask_accounts(&mut groups, choices);
        }
        group_table(&mut out, dim.label(), &groups);
    }
    tool_list(&mut out, "Top skills", &count_tools(tools, ToolKind::Skill));
    tool_list(&mut out, "Top agents", &count_tools(tools, ToolKind::Agent));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_prints_the_empty_message() {
        assert_eq!(render(&[], &[], None), format!("{EMPTY_MESSAGE}\n"));
    }

    #[test]
    fn report_has_a_row_with_the_hand_computed_model_total() {
        let event = UsageEvent {
            event_id: "e".into(),
            timestamp: 1788252682,
            session_id: "s".into(),
            account: "dev@example.com".into(),
            model: "claude-sonnet-5".into(),
            effort: "high".into(),
            repo: "proj".into(),
            branch: "main".into(),
            input_tokens: 2,
            output_tokens: 770,
            cache_creation_tokens: 29653,
            cache_read_tokens: 22178,
            cost_usd: 0.12940815,
            ..UsageEvent::default()
        };

        let text = render(&[event], &[], None);

        assert!(text.contains("1 messages, $0.1294 estimated cost"));
        let row = text
            .lines()
            .find(|l| l.contains("claude-sonnet-5"))
            .unwrap();
        assert!(row.contains("770") && row.contains("22178/29653") && row.contains("0.1294"));
        assert!(text.contains("2026-09-01"));
        assert!(text.contains("dev@example.com"));
    }

    #[test]
    fn unpriced_messages_are_reported_honestly_under_the_header() {
        let mut event = UsageEvent {
            event_id: "e".into(),
            timestamp: 1788252682,
            model: "claude-future-9".into(),
            input_tokens: 5,
            ..UsageEvent::default()
        };
        event.apply_pricing();

        let text = render(&[event], &[], None);

        assert!(text.contains(
            "Note: 1 messages are unpriced (no known price for claude-future-9); they count as $0."
        ));
        assert!(!render(&[], &[], None).contains("unpriced"));
    }

    #[test]
    fn a_period_range_adds_the_comparison_with_the_previous_period() {
        let now = 1_788_436_800; // Thursday 2026-09-03 12:00 UTC
        let event = |ts: i64, cost: f64| UsageEvent {
            event_id: ts.to_string(),
            timestamp: ts,
            model: "m".into(),
            input_tokens: 10,
            cost_usd: cost,
            ..UsageEvent::default()
        };
        let events = [event(now - 60, 3.0), event(now - 86_400 - 60, 1.0)];

        let text = render_range(&events, &[], Range::Today, now, None);

        assert!(
            text.contains(
                "Compared with yesterday (the same 12h 0m, 2026-09-02 to 2026-09-02 UTC)"
            ),
            "{text}"
        );
        assert!(text.contains("+2.0000 (+200.0%)"), "{text}");
        assert!(text.contains("+0 (+0.0%)"), "{text}");
        // A range that is not a period has no comparison.
        let all = render_range(&events, &[], Range::LastDays(30), now, None);
        assert!(!all.contains("Compared with"), "{all}");
    }

    #[test]
    fn a_zero_previous_period_prints_n_a_instead_of_a_percent() {
        let now = 1_788_436_800;
        let events = [UsageEvent {
            event_id: "e".into(),
            timestamp: now - 60,
            input_tokens: 4,
            cost_usd: 1.0,
            ..UsageEvent::default()
        }];
        let text = render_range(&events, &[], Range::Today, now, None);
        assert!(text.contains("+1.0000 (n/a)"), "{text}");
    }

    fn account_event(key: &str) -> UsageEvent {
        UsageEvent {
            event_id: key.into(),
            account: key.into(),
            model: "claude-sonnet-5".into(),
            ..UsageEvent::default()
        }
    }

    fn named(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, n)| (k.to_string(), n.to_string()))
            .collect()
    }

    #[test]
    fn mask_on_cuts_an_unnamed_email_to_its_first_domain_label() {
        let events = [account_event("dev@example.com")];
        let choices = named(&[("dev@example.com", "dev@example.com")]);

        let text = render(&events, &[], Some(&choices));

        assert!(text.contains("@example"), "{text}");
        assert!(!text.contains("dev@example.com"), "{text}");
    }

    #[test]
    fn mask_on_numbers_two_keys_that_cut_to_the_same_label() {
        let events = [
            account_event("a@example.com"),
            account_event("b@example.org"),
        ];
        let choices = named(&[
            ("a@example.com", "a@example.com"),
            ("b@example.org", "b@example.org"),
        ]);

        let text = render(&events, &[], Some(&choices));

        assert!(
            text.contains("@example (1)") && text.contains("@example (2)"),
            "{text}"
        );
    }

    #[test]
    fn mask_off_keeps_the_full_email_key() {
        let text = render(&[account_event("dev@example.com")], &[], None);

        assert!(text.contains("dev@example.com"), "{text}");
    }

    #[test]
    fn mask_on_shows_a_named_account_in_full() {
        let events = [account_event("dev@example.com")];
        let choices = named(&[("dev@example.com", "Work")]);

        let text = render(&events, &[], Some(&choices));

        assert!(text.contains("Work"), "{text}");
        assert!(!text.contains("dev@example.com"), "{text}");
    }
}
