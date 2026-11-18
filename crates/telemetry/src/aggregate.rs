// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Pure grouping over normalized events. The ASCII summary and the JSON
//! both call this, so there is exactly one aggregation implementation.

use super::{ToolInvocationEvent, ToolKind, UsageEvent};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dimension {
    Day,
    Week,
    Model,
    Repo,
    Branch,
    Effort,
    Account,
    Agent,
}

impl Dimension {
    pub fn label(self) -> &'static str {
        match self {
            Dimension::Day => "day",
            Dimension::Week => "week (starting Monday)",
            Dimension::Model => "model",
            Dimension::Repo => "repo",
            Dimension::Branch => "branch",
            Dimension::Effort => "effort",
            Dimension::Account => "account",
            Dimension::Agent => "agent",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Group {
    pub key: String,
    pub messages: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub cost_usd: f64,
    /// Messages whose model has no known price (counted as $0).
    pub unpriced: u64,
}

pub const SECONDS_PER_DAY: i64 = 86_400;
const UNSET: &str = "(none)";

/// Days since 1970-01-01 to (year, month, day), the inverse of the
/// days-from-civil conversion used when parsing transcript timestamps.
pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

pub fn date_key(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

fn day_key(timestamp: i64) -> String {
    date_key(crate::tz::day_of(timestamp))
}

/// The Monday starting the week containing `timestamp`. 1970-01-01 was a
/// Thursday, so day 0 is weekday index 3 with Monday as 0.
fn week_key(timestamp: i64) -> String {
    let days = crate::tz::day_of(timestamp);
    let weekday = (days + 3).rem_euclid(7);
    date_key(days - weekday)
}

fn key_for(event: &UsageEvent, dim: Dimension) -> String {
    let raw = match dim {
        Dimension::Day => return day_key(event.timestamp),
        Dimension::Week => return week_key(event.timestamp),
        Dimension::Model => &event.model,
        Dimension::Repo => &event.repo,
        Dimension::Branch => &event.branch,
        Dimension::Effort => &event.effort,
        Dimension::Account => &event.account,
        Dimension::Agent => &event.agent,
    };
    if raw.is_empty() {
        UNSET.to_string()
    } else {
        raw.clone()
    }
}

/// Groups sorted by key.
pub fn group_usage(events: &[UsageEvent], dim: Dimension) -> Vec<Group> {
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for event in events {
        let key = key_for(event, dim);
        let group = groups.entry(key.clone()).or_insert_with(|| Group {
            key,
            ..Group::default()
        });
        group.messages += 1;
        group.input_tokens += event.input_tokens;
        group.output_tokens += event.output_tokens;
        group.cache_creation_tokens += event.cache_creation_tokens;
        group.cache_read_tokens += event.cache_read_tokens;
        group.cost_usd += event.cost_usd;
        group.unpriced += u64::from(event.unpriced);
    }
    groups.into_values().collect()
}

/// How many messages used a model with no known price (counted as $0), and
/// which models, sorted.
pub fn unpriced_models(events: &[UsageEvent]) -> (usize, Vec<String>) {
    let mut models: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut count = 0;
    for event in events.iter().filter(|e| e.unpriced) {
        count += 1;
        models.insert(event.model.clone());
    }
    (count, models.into_iter().collect())
}

/// The models whose cost is a rate learned from telemetry, sorted.
pub fn learned_models(events: &[UsageEvent]) -> Vec<String> {
    let models: std::collections::BTreeSet<&str> = events
        .iter()
        .filter(|e| e.cost_source == crate::CostSource::Learned)
        .map(|e| e.model.as_str())
        .collect();
    models.into_iter().map(str::to_string).collect()
}

/// Invocation counts per name for one tool kind, most used first, then by name.
pub fn count_tools(tools: &[ToolInvocationEvent], kind: ToolKind) -> Vec<(String, u64)> {
    let mut counts: BTreeMap<&str, u64> = BTreeMap::new();
    for tool in tools.iter().filter(|t| t.kind == kind) {
        *counts.entry(tool.name.as_str()).or_insert(0) += 1;
    }
    let mut out: Vec<(String, u64)> = counts
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(ts: i64, model: &str, effort: &str, out: u64, cost: f64) -> UsageEvent {
        UsageEvent {
            event_id: format!("{ts}-{model}"),
            timestamp: ts,
            session_id: "s".into(),
            account: "a".into(),
            model: model.into(),
            effort: effort.into(),
            repo: "r".into(),
            branch: "b".into(),
            input_tokens: 1,
            output_tokens: out,
            cost_usd: cost,
            ..UsageEvent::default()
        }
    }

    #[test]
    fn unpriced_models_counts_flagged_messages_and_lists_models_once() {
        let mut events = sample();
        events[0].unpriced = true;
        events[0].model = "mystery".into();
        events[2].unpriced = true;
        events[2].model = "mystery".into();

        assert_eq!(unpriced_models(&events), (2, vec!["mystery".to_string()]));
        assert_eq!(unpriced_models(&sample()), (0, vec![]));
    }

    // 2026-09-01 (Tue) 08:51, 2026-09-01 09:00, 2026-09-02 (Wed) 00:00.
    fn sample() -> Vec<UsageEvent> {
        vec![
            event(1788252682, "sonnet", "high", 770, 0.125),
            event(1788253200, "opus", "xhigh", 200, 0.25),
            event(1788307201, "sonnet", "", 500, 0.5),
        ]
    }

    #[test]
    fn day_and_week_keys_match_independently_computed_dates() {
        assert_eq!(day_key(1788252682), "2026-09-01");
        assert_eq!(day_key(1788307201), "2026-09-02");
        assert_eq!(week_key(1788252682), "2026-08-31");
        assert_eq!(week_key(1788307201), "2026-08-31");
        assert_eq!(week_key(0), "1969-12-29");
    }

    #[test]
    fn groups_by_day_with_hand_computed_totals() {
        let groups = group_usage(&sample(), Dimension::Day);

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].key, "2026-09-01");
        assert_eq!((groups[0].messages, groups[0].output_tokens), (2, 970));
        assert!((groups[0].cost_usd - 0.375).abs() < 1e-9);
        assert_eq!(groups[1].key, "2026-09-02");
        assert!((groups[1].cost_usd - 0.5).abs() < 1e-9);
    }

    #[test]
    fn groups_by_model_and_effort_and_labels_a_missing_effort() {
        let by_model = group_usage(&sample(), Dimension::Model);
        let by_effort = group_usage(&sample(), Dimension::Effort);

        let sonnet = by_model.iter().find(|g| g.key == "sonnet").unwrap();
        assert_eq!((sonnet.messages, sonnet.output_tokens), (2, 1270));
        assert!((sonnet.cost_usd - 0.625).abs() < 1e-9);
        let keys: Vec<&str> = by_effort.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(keys, vec!["(none)", "high", "xhigh"]);
    }

    #[test]
    fn counts_tools_by_kind_most_used_first() {
        let tool = |id: &str, kind, name: &str| ToolInvocationEvent {
            event_id: id.into(),
            timestamp: 0,
            session_id: "s".into(),
            account: "a".into(),
            kind,
            name: name.into(),
        };
        let tools = vec![
            tool("1", ToolKind::Skill, "plan"),
            tool("2", ToolKind::Skill, "implement"),
            tool("3", ToolKind::Skill, "implement"),
            tool("4", ToolKind::Agent, "Explore"),
        ];

        assert_eq!(
            count_tools(&tools, ToolKind::Skill),
            vec![("implement".to_string(), 2), ("plan".to_string(), 1)]
        );
        assert_eq!(
            count_tools(&tools, ToolKind::Agent),
            vec![("Explore".to_string(), 1)]
        );
    }
}
