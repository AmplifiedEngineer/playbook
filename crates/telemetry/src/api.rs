// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The JSON behind `playbook usage --json`: totals, every grouping, tool
//! counts, the live view and the period comparison. Built from the same
//! aggregation the terminal summary uses.

use super::aggregate::{
    count_tools, date_key, group_usage, learned_models, unpriced_models, Dimension, Group,
};
use super::period::{pair_groups, Change, Comparison, Period, Span};
pub use super::query::{active_session_ids, live_window_start, Range, FEED_LIMIT};
use super::query::{day_of, in_range, live_view, total_tokens, LiveView, Spend, Totals};
use super::{ToolInvocationEvent, ToolKind, UsageEvent};
use serde_json::{json, Value};

fn range_json(range: Range, now: i64, usage: &[UsageEvent]) -> Value {
    let (start, end) = match range.days(now) {
        Some((first, past)) => (Some(first), Some(past - 1)),
        None => (
            usage.iter().map(|e| day_of(e.timestamp)).min(),
            usage.iter().map(|e| day_of(e.timestamp)).max(),
        ),
    };
    json!({
        "key": range.key(),
        "start": start.map(date_key),
        "end": end.map(date_key),
    })
}

fn group_json(g: &Group) -> Value {
    json!({
        "key": g.key,
        "messages": g.messages,
        "input_tokens": g.input_tokens,
        "output_tokens": g.output_tokens,
        "cache_creation_tokens": g.cache_creation_tokens,
        "cache_read_tokens": g.cache_read_tokens,
        "cost_usd": g.cost_usd,
        "unpriced": g.unpriced,
    })
}

fn counts_json(counts: Vec<(String, u64)>) -> Value {
    Value::Array(
        counts
            .into_iter()
            .map(|(name, count)| json!({ "name": name, "count": count }))
            .collect(),
    )
}

pub fn data_json(
    usage: &[UsageEvent],
    tools: &[ToolInvocationEvent],
    now: i64,
    range: Range,
) -> Value {
    let (usage, tools) = in_range(usage, tools, now, range);
    let (usage, tools) = (usage.as_slice(), tools.as_slice());

    let totals = Totals::of(usage);
    let (unpriced_messages, unpriced_model_names) = unpriced_models(usage);

    let dims = [
        ("day", Dimension::Day),
        ("week", Dimension::Week),
        ("model", Dimension::Model),
        ("repo", Dimension::Repo),
        ("branch", Dimension::Branch),
        ("effort", Dimension::Effort),
        ("account", Dimension::Account),
        ("agent", Dimension::Agent),
    ];
    let mut groups = serde_json::Map::new();
    for (name, dim) in dims {
        let rows: Vec<Value> = group_usage(usage, dim).iter().map(group_json).collect();
        groups.insert(name.to_string(), Value::Array(rows));
    }

    json!({
        "generated_at": now,
        "time_zone": crate::tz::json(now),
        "range": range_json(range, now, usage),
        "totals": {
            "messages": totals.messages,
            "input_tokens": totals.input_tokens,
            "output_tokens": totals.output_tokens,
            "cache_creation_tokens": totals.cache_creation_tokens,
            "cache_read_tokens": totals.cache_read_tokens,
            "cost_usd": totals.cost_usd,
            "estimated_usd": totals.estimated_usd,
            "unpriced_messages": unpriced_messages,
            "unpriced_models": unpriced_model_names,
            "learned_models": learned_models(usage),
        },
        "groups": groups,
        "skills": counts_json(count_tools(tools, ToolKind::Skill)),
        "agents": counts_json(count_tools(tools, ToolKind::Agent)),
    })
}

fn change_json(c: Change) -> Value {
    json!({ "amount": c.amount, "percent": c.percent })
}

fn span_json(span: Span, totals: &Totals) -> Value {
    json!({
        "start": span.start,
        "end": span.end,
        "start_date": date_key(day_of(span.start)),
        "end_date": date_key(day_of(span.end - 1)),
        "messages": totals.messages,
        "tokens": totals.tokens(),
        "cost_usd": totals.cost_usd,
    })
}

/// A period against the one before it over the same elapsed span, with the
/// change and the models of each. `usage` may hold any span: events outside
/// both are ignored. Spans are UTC seconds, half open.
pub fn comparison_json(usage: &[UsageEvent], now: i64, period: Period) -> Value {
    let windows = period.windows(now);
    let (current, previous) = windows.split(usage);
    let c = Comparison::new(period, windows, &current, &previous);
    let models: Vec<Value> = pair_groups(
        &group_usage(&current, Dimension::Model),
        &group_usage(&previous, Dimension::Model),
    )
    .iter()
    .map(|p| {
        json!({
            "key": p.key,
            "current": group_json(&p.current),
            "previous": group_json(&p.previous),
            "cost_change": change_json(Change::of(p.current.cost_usd, p.previous.cost_usd)),
        })
    })
    .collect();
    json!({
        "period": period.key(),
        "span_seconds": windows.current.seconds(),
        "current": span_json(windows.current, &c.current),
        "previous": span_json(windows.previous, &c.previous),
        "change": {
            "cost_usd": change_json(c.cost()),
            "tokens": change_json(c.tokens()),
            "messages": change_json(c.messages()),
        },
        "models": models,
    })
}

fn spend_json(spend: &Spend) -> Value {
    json!({
        "messages": spend.messages,
        "tokens": spend.tokens,
        "cost_usd": spend.cost_usd,
        "unpriced": spend.unpriced,
    })
}

/// What the Live tab shows, as JSON. See `query::live_view` for the inputs.
pub fn live_json(
    window: &[UsageEvent],
    sessions: &[UsageEvent],
    latest: &[UsageEvent],
    now: i64,
) -> Value {
    let LiveView {
        generated_at,
        active,
        hour,
        today,
        burn: _,
        feed,
    } = live_view(window, sessions, latest, now);
    let active: Vec<Value> = active
        .iter()
        .map(|a| {
            json!({
                "id": a.id,
                "repo": a.repo,
                "agent": a.agent,
                "model": a.model,
                "last": a.last,
                "messages": a.messages,
                "cost_usd": a.cost_usd,
                "unpriced": a.unpriced,
            })
        })
        .collect();
    let feed: Vec<Value> = feed
        .iter()
        .map(|e| {
            json!({
                "time": e.timestamp,
                "session": e.session_id,
                "repo": e.repo,
                "agent": e.agent,
                "model": e.model,
                "tokens": total_tokens(e),
                "cost_usd": e.cost_usd,
                "unpriced": u64::from(e.unpriced),
            })
        })
        .collect();
    json!({
        "generated_at": generated_at,
        "active": active,
        "hour": spend_json(&hour),
        "today": spend_json(&today),
        "feed": feed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::ACTIVE_SECONDS;

    fn event(ts: i64, model: &str, cost: f64) -> UsageEvent {
        UsageEvent {
            event_id: format!("{ts}{model}"),
            timestamp: ts,
            session_id: "s".into(),
            account: "a".into(),
            model: model.into(),
            effort: "high".into(),
            repo: "r".into(),
            branch: "b".into(),
            input_tokens: 10,
            output_tokens: 20,
            cache_creation_tokens: 1,
            cache_read_tokens: 2,
            cost_usd: cost,
            ..UsageEvent::default()
        }
    }

    #[test]
    fn every_row_kind_carries_its_unpriced_count() {
        let now = 1788400000;
        let mut codex = in_session("c", now - 60, "gpt-5-codex", 0.0);
        codex.unpriced = true;
        let priced = in_session("c", now - 30, "sonnet", 1.0);
        let events = vec![codex, priced];

        let data = data_json(&events, &[], now, Range::All);
        let models = data["groups"]["model"].as_array().unwrap().clone();
        let counts: Vec<u64> = models
            .iter()
            .map(|r| r["unpriced"].as_u64().unwrap())
            .collect();
        assert_eq!(counts.iter().sum::<u64>(), 1, "{models:?}");

        let refs: Vec<&UsageEvent> = events.iter().collect();
        assert_eq!(Spend::of(&refs).unpriced, 1);
        let live = live_json(&events, &events, &events, now);
        assert_eq!(live["active"][0]["unpriced"], 1);
        let flags: Vec<u64> = live["feed"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["unpriced"].as_u64().unwrap())
            .collect();
        assert_eq!(flags.iter().sum::<u64>(), 1);
    }

    #[test]
    fn totals_report_the_estimated_cost_and_the_learned_models() {
        let mut usage = vec![
            event(1788252682, "sonnet", 1.0),
            event(1788252683, "opus", 0.5),
        ];
        usage[0].cost_source = crate::CostSource::Telemetry;
        usage[1].cost_source = crate::CostSource::Learned;

        let data = data_json(&usage, &[], 1788400000, Range::All);

        assert!((data["totals"]["estimated_usd"].as_f64().unwrap() - 0.5).abs() < 1e-9);
        assert_eq!(data["totals"]["learned_models"][0], "opus");
    }

    #[test]
    fn totals_report_unpriced_messages_and_their_models() {
        let mut usage = vec![event(1788252682, "sonnet", 0.25)];
        usage[0].unpriced = true;

        let data = data_json(&usage, &[], 1788400000, Range::All);

        assert_eq!(data["totals"]["unpriced_messages"], 1);
        assert_eq!(data["totals"]["unpriced_models"][0], "sonnet");
    }

    #[test]
    fn totals_and_groups_match_hand_computed_sums() {
        let usage = vec![
            event(1788252682, "sonnet", 0.25),
            event(1788307201, "opus", 0.5),
        ];

        let data = data_json(&usage, &[], 1788400000, Range::All);

        assert_eq!(data["generated_at"], 1788400000);
        assert_eq!(data["time_zone"]["mode"], "utc");
        assert_eq!(data["time_zone"]["offset_seconds"], 0);
        assert_eq!(data["time_zone"]["label"], "UTC");
        assert_eq!(data["totals"]["messages"], 2);
        assert_eq!(data["totals"]["output_tokens"], 40);
        assert!((data["totals"]["cost_usd"].as_f64().unwrap() - 0.75).abs() < 1e-9);
        assert_eq!(data["groups"]["day"].as_array().unwrap().len(), 2);
        assert_eq!(data["groups"]["model"][0]["key"], "opus");
    }

    #[test]
    fn empty_input_is_valid_json_with_empty_groups() {
        let data = data_json(&[], &[], 0, Range::All);

        assert_eq!(data["totals"]["messages"], 0);
        assert!(data["groups"]["day"].as_array().unwrap().is_empty());
        assert!(data["skills"].as_array().unwrap().is_empty());
        assert!(data["range"]["start"].is_null());
    }

    const SEP_3_NOON: i64 = 1788436800;

    fn span(range: Range, now: i64) -> (String, String) {
        let (first, past) = range.days(now).expect("a bounded range");
        (date_key(first), date_key(past - 1))
    }

    #[test]
    fn parses_every_range_and_rejects_anything_else() {
        assert_eq!(Range::parse(None), Some(Range::All));
        assert_eq!(Range::parse(Some("all")), Some(Range::All));
        assert_eq!(Range::parse(Some("30d")), Some(Range::LastDays(30)));
        assert_eq!(Range::parse(Some("60d")), Some(Range::LastDays(60)));
        assert_eq!(Range::parse(Some("90d")), Some(Range::LastDays(90)));
        assert_eq!(Range::parse(Some("month")), Some(Range::CurrentMonth));
        assert_eq!(Range::parse(Some("7d")), None);
        assert_eq!(Range::parse(Some("")), None);
    }

    #[test]
    fn last_n_days_include_today_and_months_run_first_to_last_day() {
        assert_eq!(
            span(Range::LastDays(30), SEP_3_NOON),
            ("2026-08-05".into(), "2026-09-03".into())
        );
        assert_eq!(
            span(Range::LastDays(90), SEP_3_NOON),
            ("2026-06-06".into(), "2026-09-03".into())
        );
        assert_eq!(
            span(Range::CurrentMonth, SEP_3_NOON),
            ("2026-09-01".into(), "2026-09-30".into())
        );
        assert_eq!(
            span(Range::CurrentMonth, 1834185600),
            ("2028-02-01".into(), "2028-02-29".into())
        );
        assert_eq!(
            span(Range::CurrentMonth, 1798761599),
            ("2026-12-01".into(), "2026-12-31".into())
        );
        assert_eq!(Range::All.days(SEP_3_NOON), None);
    }

    #[test]
    fn a_range_filters_usage_and_tools_at_the_utc_day_edge() {
        let usage = vec![
            event(1785887999, "sonnet", 1.0),
            event(1785888000, "sonnet", 0.25),
            event(1788252682, "opus", 0.5),
        ];
        let tool = |ts: i64| ToolInvocationEvent {
            event_id: ts.to_string(),
            timestamp: ts,
            session_id: "s".into(),
            account: "a".into(),
            kind: ToolKind::Skill,
            name: "plan".into(),
        };
        let tools = vec![tool(1785887999), tool(1788252682)];

        let data = data_json(&usage, &tools, SEP_3_NOON, Range::LastDays(30));

        assert_eq!(data["totals"]["messages"], 2);
        assert!((data["totals"]["cost_usd"].as_f64().unwrap() - 0.75).abs() < 1e-9);
        assert_eq!(data["skills"][0]["count"], 1);
        assert_eq!(data["range"]["key"], "30d");
        assert_eq!(data["range"]["start"], "2026-08-05");
        assert_eq!(data["range"]["end"], "2026-09-03");
    }

    #[test]
    fn ranges_hold_at_midnight_across_the_year_and_in_a_short_february() {
        const JAN_1_2027: i64 = 1798761600;
        const FEB_15_2027: i64 = 1802649600;

        assert_eq!(
            span(Range::CurrentMonth, JAN_1_2027),
            ("2027-01-01".into(), "2027-01-31".into())
        );
        assert_eq!(
            span(Range::LastDays(30), JAN_1_2027),
            ("2026-12-03".into(), "2027-01-01".into())
        );
        assert_eq!(
            span(Range::LastDays(30), JAN_1_2027 - 1),
            ("2026-12-02".into(), "2026-12-31".into())
        );
        assert_eq!(
            span(Range::CurrentMonth, FEB_15_2027),
            ("2027-02-01".into(), "2027-02-28".into())
        );

        let usage = vec![
            event(JAN_1_2027 - 1, "opus", 1.0),
            event(JAN_1_2027, "opus", 2.0),
        ];
        let data = data_json(&usage, &[], JAN_1_2027 - 1, Range::LastDays(30));
        assert_eq!(
            data["totals"]["messages"], 1,
            "tomorrow is outside the window"
        );
        let month = data_json(&[], &[], JAN_1_2027, Range::CurrentMonth);
        assert_eq!(month["range"]["start"], "2027-01-01");
        assert_eq!(month["range"]["end"], "2027-01-31");
    }

    #[test]
    fn all_time_reports_the_first_and_last_day_with_data() {
        let usage = vec![
            event(1788252682, "opus", 0.5),
            event(1785888000, "opus", 0.5),
        ];

        let data = data_json(&usage, &[], SEP_3_NOON, Range::All);

        assert_eq!(data["range"]["key"], "all");
        assert_eq!(data["range"]["start"], "2026-08-05");
        assert_eq!(data["range"]["end"], "2026-09-01");
    }

    fn in_session(id: &str, ts: i64, model: &str, cost: f64) -> UsageEvent {
        UsageEvent {
            session_id: id.into(),
            agent: "codex".into(),
            ..event(ts, model, cost)
        }
    }

    #[test]
    fn the_by_agent_group_splits_claude_code_from_codex() {
        let mut usage = vec![event(1788252682, "sonnet", 0.25)];
        usage[0].agent = "claude-code".into();
        usage.push(in_session("c", 1788252700, "gpt", 0.0));

        let data = data_json(&usage, &[], 1788400000, Range::All);

        let keys: Vec<&str> = data["groups"]["agent"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["key"].as_str().unwrap())
            .collect();
        assert_eq!(keys, ["claude-code", "codex"]);
    }

    #[test]
    fn the_live_window_starts_at_midnight_or_an_hour_ago_whichever_is_earlier() {
        let midnight = 1788393600;
        assert_eq!(midnight % 86_400, 0);
        assert_eq!(live_window_start(midnight + 1800), midnight + 1800 - 3540);
        assert_eq!(live_window_start(midnight + 5 * 3600), midnight);
    }

    #[test]
    fn live_sums_the_last_hour_and_today_and_lists_only_recent_sessions() {
        let now = 1788393600 + 5 * 3600;
        let window = vec![
            in_session("early", now - 4 * 3600, "m", 4.0),
            in_session("hour", now - 3000, "m", 2.0),
            in_session("busy", now - 600, "m", 1.0),
            in_session("busy", now - 30, "opus", 0.5),
        ];
        let sessions = vec![
            in_session("busy", now - 7200, "m", 8.0),
            window[2].clone(),
            window[3].clone(),
        ];
        let latest: Vec<UsageEvent> = window.iter().rev().cloned().collect();

        let live = live_json(&window, &sessions, &latest, now);

        assert_eq!(live["hour"]["messages"], 3);
        assert!((live["hour"]["cost_usd"].as_f64().unwrap() - 3.5).abs() < 1e-9);
        assert_eq!(live["today"]["messages"], 4);
        assert!((live["today"]["cost_usd"].as_f64().unwrap() - 7.5).abs() < 1e-9);
        let active = live["active"].as_array().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0]["id"], "busy");
        assert_eq!(active[0]["model"], "opus");
        assert_eq!(active[0]["last"], now - 30);
        assert_eq!(active[0]["messages"], 3);
        assert!((active[0]["cost_usd"].as_f64().unwrap() - 9.5).abs() < 1e-9);
        assert_eq!(live["feed"][0]["time"], now - 30);
    }

    #[test]
    fn a_session_exactly_fifteen_minutes_quiet_is_no_longer_active() {
        let now = 1788400000;
        let window = vec![
            in_session("edge", now - ACTIVE_SECONDS, "m", 1.0),
            in_session("in", now - ACTIVE_SECONDS + 1, "m", 1.0),
        ];

        assert_eq!(active_session_ids(&window, now), ["in"]);
    }

    #[test]
    fn the_feed_is_capped_and_the_burn_buckets_by_minute() {
        let now = 1788400000;
        let latest: Vec<UsageEvent> = (0..30)
            .map(|i| in_session("f", now - i, "m", 0.0))
            .collect();
        let hot = vec![
            in_session("a", now - 10, "m", 1.0),
            in_session("a", now - 20, "m", 1.0),
        ];

        let live = live_json(&hot, &[], &latest, now);

        assert_eq!(live["feed"].as_array().unwrap().len(), FEED_LIMIT);
    }

    #[test]
    fn the_hour_crossing_midnight_is_not_the_same_as_today() {
        let midnight = 1788393600;
        let now = midnight + 1800;
        let window = vec![
            in_session("a", midnight - 600, "m", 3.0),
            in_session("b", midnight + 600, "m", 1.0),
        ];

        let live = live_json(&window, &[], &[], now);

        assert_eq!(live["hour"]["messages"], 2);
        assert!((live["hour"]["cost_usd"].as_f64().unwrap() - 4.0).abs() < 1e-9);
        assert_eq!(live["today"]["messages"], 1);
        assert!((live["today"]["cost_usd"].as_f64().unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn the_comparison_json_names_both_spans_the_change_and_the_models() {
        let now = 1_788_436_800; // Thursday 2026-09-03 12:00 UTC
        let events = [
            event(now - 60, "opus", 3.0),
            event(now - 86_400 - 60, "opus", 1.0),
            event(now - 86_400 - 30, "sonnet", 1.0),
        ];

        let v = comparison_json(&events, now, Period::Today);

        assert_eq!(v["period"], "today");
        assert_eq!(v["span_seconds"], 12 * 3600 + 1);
        assert_eq!(v["current"]["start_date"], "2026-09-03");
        assert_eq!(v["previous"]["start_date"], "2026-09-02");
        assert_eq!(v["current"]["messages"], 1);
        assert_eq!(v["previous"]["messages"], 2);
        assert_eq!(v["change"]["cost_usd"]["amount"], 1.0);
        assert_eq!(v["change"]["cost_usd"]["percent"], 50.0);
        let models = v["models"].as_array().unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[1]["key"], "sonnet");
        assert_eq!(models[1]["current"]["messages"], 0);
        // A model with no spend before has no percent, not a division by zero.
        let v = comparison_json(&events[..1], now, Period::Today);
        assert!(v["change"]["cost_usd"]["percent"].is_null());
    }
}
