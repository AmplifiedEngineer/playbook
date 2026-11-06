// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Reads an OTLP/HTTP JSON logs body into telemetry rows.
//!
//! Three events are used. `api_request` carries one API request with its own
//! request id, `tool_result` one tool call with its own tool use id, and
//! `api_error` one failed attempt, keyed by session and event sequence. Each
//! key makes a redelivered batch a no-op, so nothing counts twice. Metrics are
//! ignored on purpose (they have no key).
//!
//! Privacy: this parser is the allowlist. Only the keys in `ALLOWED_KEYS` are
//! ever read, so prompts, tool content, paths, emails, user ids and resource
//! attributes are dropped here and never reach storage. String values that
//! are kept (model, effort, agent name, ids) are checked for a safe charset
//! and length, and a value that fails the check is dropped.

use super::telemetry_db::{TelemetryError, TelemetryMetric, TelemetryRequest, TelemetryTool};
use serde_json::Value;

/// The attribute keys this parser reads. Everything else is ignored.
pub const ALLOWED_KEYS: &[&str] = &[
    "event.name",
    "session.id",
    "request_id",
    "model",
    "effort",
    "agent.name",
    "skill.name",
    "tool_name",
    "tool_use_id",
    "success",
    "duration_ms",
    "status_code",
    "attempt",
    "event.sequence",
    "input_tokens",
    "output_tokens",
    "cache_read_tokens",
    "cache_creation_tokens",
    "cost_usd",
];

/// The metrics read, with the short name stored. The cost and token metrics
/// are left out, the requests already carry them with a key.
const METRICS: &[(&str, &str)] = &[
    ("active_time.total", "active_time"),
    ("lines_of_code.count", "lines_of_code"),
    ("commit.count", "commits"),
    ("pull_request.count", "pull_requests"),
];

/// The attribute keys read on a metric point.
const METRIC_KEYS: &[&str] = &["session.id", "type"];

/// The most log records read from one body; the rest are dropped.
pub const MAX_RECORDS: usize = 2000;

const MAX_NAME_LEN: usize = 64;
const MAX_ID_LEN: usize = 128;
const EVENT_PREFIX: &str = "claude_code.";

#[derive(Default)]
struct Fields {
    event: Option<String>,
    session: Option<String>,
    request: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    agent: Option<String>,
    skill: Option<String>,
    tool: Option<String>,
    tool_use: Option<String>,
    success: Option<bool>,
    duration_ms: Option<u64>,
    status: Option<u64>,
    attempt: Option<u64>,
    sequence: Option<u64>,
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cache_creation: Option<u64>,
    cost: Option<f64>,
}

/// A name or id value: ASCII letters, digits and `. _ - : /` only, within `max`.
pub(crate) fn safe_text(value: &str, max: usize) -> Option<String> {
    let ok = !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-:/".contains(&b));
    ok.then(|| value.to_string())
}

fn any_value(v: &Value) -> Option<&Value> {
    let map = v.as_object()?;
    ["stringValue", "intValue", "doubleValue", "boolValue"]
        .iter()
        .find_map(|k| map.get(*k))
}

fn as_u64(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64().or_else(|| {
            n.as_f64()
                .filter(|f| *f >= 0.0 && f.fract() == 0.0)
                .map(|f| f as u64)
        }),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn as_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn as_f64(v: &Value) -> Option<f64> {
    let f = match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }?;
    (f.is_finite() && f >= 0.0).then_some(f)
}

fn read_attributes(attrs: &Value) -> Fields {
    let mut f = Fields::default();
    let Some(list) = attrs.as_array() else {
        return f;
    };
    for attr in list {
        let Some(key) = attr.get("key").and_then(Value::as_str) else {
            continue;
        };
        if !ALLOWED_KEYS.contains(&key) {
            continue;
        }
        let Some(value) = attr.get("value").and_then(any_value) else {
            continue;
        };
        let text = value.as_str();
        match key {
            "event.name" => f.event = text.map(str::to_string),
            "session.id" => f.session = text.and_then(|t| safe_text(t, MAX_ID_LEN)),
            "request_id" => f.request = text.and_then(|t| safe_text(t, MAX_ID_LEN)),
            "model" => f.model = text.and_then(|t| safe_text(t, MAX_NAME_LEN)),
            "effort" => f.effort = text.and_then(|t| safe_text(t, MAX_NAME_LEN)),
            "agent.name" => f.agent = text.and_then(|t| safe_text(t, MAX_NAME_LEN)),
            "skill.name" => f.skill = text.and_then(|t| safe_text(t, MAX_NAME_LEN)),
            "tool_name" => f.tool = text.and_then(|t| safe_text(t, MAX_NAME_LEN)),
            "tool_use_id" => f.tool_use = text.and_then(|t| safe_text(t, MAX_ID_LEN)),
            "success" => f.success = as_bool(value),
            "duration_ms" => f.duration_ms = as_u64(value),
            "status_code" => f.status = as_u64(value),
            "attempt" => f.attempt = as_u64(value),
            "event.sequence" => f.sequence = as_u64(value),
            "input_tokens" => f.input = as_u64(value),
            "output_tokens" => f.output = as_u64(value),
            "cache_read_tokens" => f.cache_read = as_u64(value),
            "cache_creation_tokens" => f.cache_creation = as_u64(value),
            "cost_usd" => f.cost = as_f64(value),
            _ => {}
        }
    }
    f
}

fn nanos_to_secs(record: &Value) -> Option<i64> {
    let raw = record.get("timeUnixNano")?;
    let nanos = as_u64(raw)?;
    i64::try_from(nanos / 1_000_000_000).ok().filter(|s| *s > 0)
}

fn event_name(record: &Value, f: &Fields) -> Option<String> {
    let name = f.event.clone().or_else(|| {
        record
            .get("body")
            .and_then(any_value)
            .and_then(Value::as_str)
            .map(str::to_string)
    })?;
    Some(name.strip_prefix(EVENT_PREFIX).unwrap_or(&name).to_string())
}

/// The rows of one logs body.
#[derive(Debug, Default, PartialEq)]
pub struct Parsed {
    pub requests: Vec<TelemetryRequest>,
    pub tools: Vec<TelemetryTool>,
    pub errors: Vec<TelemetryError>,
}

/// Parses an OTLP JSON logs body. Records that are not a complete
/// `api_request`, `tool_result` or `api_error` (no session id, no key, or
/// another event) are skipped. `now` stamps a record that has no usable time.
pub fn parse_events(body: &[u8], now: i64) -> Result<Parsed, String> {
    let doc: Value = serde_json::from_slice(body).map_err(|e| format!("invalid JSON: {e}"))?;
    let resources = doc
        .get("resourceLogs")
        .and_then(Value::as_array)
        .ok_or("missing resourceLogs")?;
    let mut out = Parsed::default();
    let mut seen = 0usize;
    for scopes in resources
        .iter()
        .filter_map(|r| r.get("scopeLogs")?.as_array())
    {
        for records in scopes
            .iter()
            .filter_map(|s| s.get("logRecords")?.as_array())
        {
            for record in records {
                seen += 1;
                if seen > MAX_RECORDS {
                    return Ok(out);
                }
                let f = read_attributes(record.get("attributes").unwrap_or(&Value::Null));
                let timestamp = nanos_to_secs(record).unwrap_or(now);
                match event_name(record, &f).as_deref() {
                    Some("api_request") => out.requests.extend(request(f, timestamp)),
                    Some("tool_result") => out.tools.extend(tool(f, timestamp)),
                    Some("api_error") => out.errors.extend(error(f, timestamp)),
                    _ => {}
                }
            }
        }
    }
    Ok(out)
}

/// Parses an OTLP JSON metrics body into counter points. Only delta sums of
/// the metrics in `METRICS` with a session id are kept; a cumulative sum is
/// skipped because adding it up would count it again and again.
pub fn parse_metrics(body: &[u8], now: i64) -> Result<Vec<TelemetryMetric>, String> {
    let doc: Value = serde_json::from_slice(body).map_err(|e| format!("invalid JSON: {e}"))?;
    let empty = Vec::new();
    let resources = doc
        .get("resourceMetrics")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let mut out = Vec::new();
    let mut seen = 0usize;
    for metrics in resources
        .iter()
        .filter_map(|r| r.get("scopeMetrics")?.as_array())
        .flatten()
        .filter_map(|s| s.get("metrics")?.as_array())
    {
        for metric in metrics {
            let name = metric.get("name").and_then(Value::as_str).unwrap_or("");
            let name = name.strip_prefix(EVENT_PREFIX).unwrap_or(name);
            let Some((_, short)) = METRICS.iter().find(|(n, _)| *n == name) else {
                continue;
            };
            let Some(sum) = metric.get("sum") else {
                continue;
            };
            // 1 is delta in the OTLP enum, 2 is cumulative.
            let delta = match sum.get("aggregationTemporality") {
                Some(Value::String(s)) => s == "AGGREGATION_TEMPORALITY_DELTA",
                Some(other) => as_u64(other) == Some(1),
                None => false,
            };
            if !delta {
                continue;
            }
            for point in sum
                .get("dataPoints")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                seen += 1;
                if seen > MAX_RECORDS {
                    return Ok(out);
                }
                out.extend(metric_point(point, short, now));
            }
        }
    }
    Ok(out)
}

fn metric_point(point: &Value, metric: &str, now: i64) -> Option<TelemetryMetric> {
    let (mut session, mut kind) = (None, String::new());
    for attr in point.get("attributes")?.as_array()? {
        let key = attr.get("key").and_then(Value::as_str).unwrap_or("");
        if !METRIC_KEYS.contains(&key) {
            continue;
        }
        let text = attr
            .get("value")
            .and_then(any_value)
            .and_then(Value::as_str);
        match key {
            "session.id" => session = text.and_then(|t| safe_text(t, MAX_ID_LEN)),
            _ => {
                kind = text
                    .and_then(|t| safe_text(t, MAX_NAME_LEN))
                    .unwrap_or_default()
            }
        }
    }
    let value = point
        .get("asDouble")
        .or_else(|| point.get("asInt"))
        .and_then(as_f64)?;
    let end_ns = point.get("timeUnixNano").and_then(as_u64)?;
    let start_ns = point
        .get("startTimeUnixNano")
        .and_then(as_u64)
        .unwrap_or(end_ns);
    Some(TelemetryMetric {
        session_id: session?,
        metric: metric.to_string(),
        kind,
        start_ns: i64::try_from(start_ns).ok()?,
        end_ns: i64::try_from(end_ns).ok()?,
        timestamp: nanos_to_secs(point).unwrap_or(now),
        value,
    })
}

fn request(f: Fields, timestamp: i64) -> Option<TelemetryRequest> {
    Some(TelemetryRequest {
        session_id: f.session?,
        request_id: f.request?,
        timestamp,
        model: f.model.unwrap_or_default(),
        effort: f.effort.unwrap_or_default(),
        agent_name: f.agent.unwrap_or_default(),
        skill_name: f.skill.unwrap_or_default(),
        input_tokens: f.input.unwrap_or(0),
        output_tokens: f.output.unwrap_or(0),
        cache_creation_tokens: f.cache_creation.unwrap_or(0),
        cache_read_tokens: f.cache_read.unwrap_or(0),
        cost_usd: f.cost.unwrap_or(0.0),
        duration_ms: f.duration_ms.unwrap_or(0),
    })
}

fn tool(f: Fields, timestamp: i64) -> Option<TelemetryTool> {
    Some(TelemetryTool {
        session_id: f.session?,
        tool_use_id: f.tool_use?,
        timestamp,
        tool_name: f.tool.unwrap_or_default(),
        success: f.success.unwrap_or(true),
        duration_ms: f.duration_ms.unwrap_or(0),
    })
}

fn error(f: Fields, timestamp: i64) -> Option<TelemetryError> {
    Some(TelemetryError {
        session_id: f.session?,
        sequence: f.sequence?,
        timestamp,
        model: f.model.unwrap_or_default(),
        status_code: f.status.unwrap_or(0),
        attempt: f.attempt.unwrap_or(1),
        duration_ms: f.duration_ms.unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse_logs(body: &[u8], now: i64) -> Result<Vec<TelemetryRequest>, String> {
        parse_events(body, now).map(|p| p.requests)
    }

    fn attr(k: &str, v: Value) -> Value {
        json!({"key": k, "value": v})
    }

    fn s(k: &str, v: &str) -> Value {
        attr(k, json!({"stringValue": v}))
    }

    fn n(k: &str, v: i64) -> Value {
        attr(k, json!({"intValue": v.to_string()}))
    }

    fn body(records: Vec<Value>) -> Vec<u8> {
        json!({"resourceLogs": [{
            "resource": {"attributes": [s("user.email", "a@b.c")]},
            "scopeLogs": [{"logRecords": records}]
        }]})
        .to_string()
        .into_bytes()
    }

    fn api_request(extra: Vec<Value>) -> Value {
        let mut attrs = vec![
            s("event.name", "api_request"),
            s("session.id", "sess-1"),
            s("request_id", "req_1"),
            s("model", "claude-opus-5"),
            s("effort", "high"),
            s("agent.name", "reviewer"),
            n("input_tokens", 10),
            n("output_tokens", 20),
            n("cache_read_tokens", 30),
            n("cache_creation_tokens", 40),
            attr("cost_usd", json!({"doubleValue": 0.5})),
        ];
        attrs.extend(extra);
        json!({"timeUnixNano": "1760000000000000000", "attributes": attrs})
    }

    #[test]
    fn an_api_request_becomes_one_row() {
        let rows = parse_logs(&body(vec![api_request(vec![])]), 5).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.session_id, "sess-1");
        assert_eq!(r.request_id, "req_1");
        assert_eq!(r.timestamp, 1_760_000_000);
        assert_eq!(r.agent_name, "reviewer");
        assert_eq!(
            (
                r.input_tokens,
                r.output_tokens,
                r.cache_read_tokens,
                r.cache_creation_tokens
            ),
            (10, 20, 30, 40)
        );
        assert_eq!(r.cost_usd, 0.5);
    }

    #[test]
    fn the_skill_name_is_kept_and_an_unsafe_one_is_dropped() {
        let rows = parse_logs(
            &body(vec![api_request(vec![s("skill.name", "tinyskill")])]),
            5,
        )
        .unwrap();
        assert_eq!(rows[0].skill_name, "tinyskill");
        let rows = parse_logs(
            &body(vec![api_request(vec![s("skill.name", "a b/../c d")])]),
            5,
        )
        .unwrap();
        assert_eq!(rows[0].skill_name, "");
        let rows = parse_logs(&body(vec![api_request(vec![])]), 5).unwrap();
        assert_eq!(rows[0].skill_name, "");
    }

    fn tool_result(extra: Vec<Value>) -> Value {
        let mut attrs = vec![
            s("event.name", "tool_result"),
            s("session.id", "sess-1"),
            s("tool_name", "Bash"),
            s("tool_use_id", "toolu_1"),
            s("success", "false"),
            s("duration_ms", "507"),
            s("tool_input_size_bytes", "99"),
        ];
        attrs.extend(extra);
        json!({"timeUnixNano": "1760000000000000000", "attributes": attrs})
    }

    fn api_error(extra: Vec<Value>) -> Value {
        let mut attrs = vec![
            s("event.name", "api_error"),
            s("session.id", "sess-1"),
            s("model", "claude-opus-5"),
            n("status_code", 529),
            n("attempt", 2),
            n("duration_ms", 865),
            n("event.sequence", 39),
            s("error", "overloaded for user someone@example.com"),
        ];
        attrs.extend(extra);
        json!({"timeUnixNano": "1760000000000000000", "attributes": attrs})
    }

    #[test]
    fn a_tool_result_and_an_api_error_become_rows() {
        let p = parse_events(&body(vec![tool_result(vec![]), api_error(vec![])]), 5).unwrap();
        assert_eq!(p.tools.len(), 1);
        let t = &p.tools[0];
        assert_eq!(
            (
                t.tool_name.as_str(),
                t.tool_use_id.as_str(),
                t.success,
                t.duration_ms
            ),
            ("Bash", "toolu_1", false, 507)
        );
        assert_eq!(p.errors.len(), 1);
        let e = &p.errors[0];
        assert_eq!(
            (e.sequence, e.status_code, e.attempt, e.duration_ms),
            (39, 529, 2, 865)
        );
        assert!(p.requests.is_empty());
        assert!(
            !format!("{p:?}").contains("someone"),
            "error text must not be read"
        );
    }

    #[test]
    fn a_tool_result_without_an_id_or_an_error_without_a_sequence_is_skipped() {
        let mut t = tool_result(vec![]);
        t["attributes"]
            .as_array_mut()
            .unwrap()
            .retain(|a| a["key"] != "tool_use_id");
        let mut e = api_error(vec![]);
        e["attributes"]
            .as_array_mut()
            .unwrap()
            .retain(|a| a["key"] != "event.sequence");
        let p = parse_events(&body(vec![t, e]), 5).unwrap();
        assert!(p.tools.is_empty() && p.errors.is_empty());
    }

    #[test]
    fn a_real_boolean_success_is_read_and_an_api_request_keeps_its_duration() {
        let t = tool_result(vec![attr("success", json!({"boolValue": true}))]);
        // The later duplicate key wins, like a map built from the list.
        let p = parse_events(&body(vec![t, api_request(vec![n("duration_ms", 1200)])]), 5).unwrap();
        assert!(p.tools[0].success);
        assert_eq!(p.requests[0].duration_ms, 1200);
    }

    #[test]
    fn the_event_name_may_come_from_the_body_with_the_prefix() {
        let mut rec = api_request(vec![]);
        let attrs = rec["attributes"].as_array_mut().unwrap();
        attrs.retain(|a| a["key"] != "event.name");
        rec["body"] = json!({"stringValue": "claude_code.api_request"});
        assert_eq!(parse_logs(&body(vec![rec]), 5).unwrap().len(), 1);
    }

    #[test]
    fn a_prompt_bearing_attribute_is_dropped_and_never_read() {
        let rec = api_request(vec![
            s("prompt", "write me a secret"),
            s("prompt_text", "hunter2"),
            s("file_path", "/Users/someone/secret.txt"),
            s("user.email", "someone@example.com"),
            s("tool_input", "rm -rf"),
        ]);
        let rows = parse_logs(&body(vec![rec]), 5).unwrap();
        let dump = format!("{rows:?}");
        for leaked in ["secret", "hunter2", "someone", "rm -rf"] {
            assert!(!dump.contains(leaked), "{leaked} leaked into {dump}");
        }
        assert!(!ALLOWED_KEYS
            .iter()
            .any(|k| k.contains("prompt") || k.contains("path")));
    }

    #[test]
    fn an_unsafe_name_is_dropped_not_stored() {
        let mut rec = api_request(vec![]);
        rec["attributes"]
            .as_array_mut()
            .unwrap()
            .retain(|a| a["key"] != "agent.name");
        rec["attributes"]
            .as_array_mut()
            .unwrap()
            .push(s("agent.name", "/Users/me/my project"));
        assert_eq!(parse_logs(&body(vec![rec]), 5).unwrap()[0].agent_name, "");
    }

    #[test]
    fn other_events_and_incomplete_records_are_skipped() {
        let tool = json!({"attributes": [s("event.name", "tool_result"), s("session.id", "s")]});
        let mut no_req = api_request(vec![]);
        no_req["attributes"]
            .as_array_mut()
            .unwrap()
            .retain(|a| a["key"] != "request_id");
        let rows = parse_logs(&body(vec![tool, no_req, api_request(vec![])]), 5).unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn a_missing_time_uses_now_and_bad_numbers_are_zero() {
        let mut rec = api_request(vec![]);
        rec.as_object_mut().unwrap().remove("timeUnixNano");
        let attrs = rec["attributes"].as_array_mut().unwrap();
        attrs.retain(|a| a["key"] != "cost_usd" && a["key"] != "input_tokens");
        attrs.push(attr("cost_usd", json!({"doubleValue": -3.0})));
        attrs.push(s("input_tokens", "lots"));
        let r = &parse_logs(&body(vec![rec]), 77).unwrap()[0];
        assert_eq!((r.timestamp, r.cost_usd, r.input_tokens), (77, 0.0, 0));
    }

    #[test]
    fn more_than_the_record_cap_is_truncated() {
        let records = (0..MAX_RECORDS + 5)
            .map(|i| {
                let mut r = api_request(vec![]);
                r["attributes"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|a| a["key"] != "request_id");
                r["attributes"]
                    .as_array_mut()
                    .unwrap()
                    .push(s("request_id", &format!("r{i}")));
                r
            })
            .collect();
        assert_eq!(parse_logs(&body(records), 5).unwrap().len(), MAX_RECORDS);
    }

    #[test]
    fn invalid_bodies_are_errors() {
        assert!(parse_logs(b"not json", 1).is_err());
        assert!(parse_logs(b"{}", 1).is_err());
        assert!(parse_logs(b"[]", 1).is_err());
    }

    #[test]
    fn a_deeply_nested_body_is_rejected_without_a_crash() {
        let deep = format!("{}{}", "[".repeat(5000), "]".repeat(5000));
        assert!(parse_logs(deep.as_bytes(), 1).is_err());
    }

    fn metric_body(name: &str, temporality: Value, points: Vec<Value>) -> Vec<u8> {
        json!({"resourceMetrics": [{
            "resource": {"attributes": [s("user.email", "a@b.c")]},
            "scopeMetrics": [{"metrics": [{
                "name": name,
                "sum": {"aggregationTemporality": temporality, "dataPoints": points}
            }]}]
        }]})
        .to_string()
        .into_bytes()
    }

    fn point(session: &str, kind: &str, value: f64) -> Value {
        json!({
            "startTimeUnixNano": "1760000000000000000",
            "timeUnixNano": "1760000060000000000",
            "asDouble": value,
            "attributes": [s("session.id", session), s("type", kind), s("user.id", "u-1")]
        })
    }

    #[test]
    fn an_active_time_delta_point_becomes_a_row_without_other_attributes() {
        let body = metric_body(
            "claude_code.active_time.total",
            json!(1),
            vec![point("s1", "cli", 12.5)],
        );
        let rows = parse_metrics(&body, 5).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(
            (r.metric.as_str(), r.kind.as_str(), r.session_id.as_str()),
            ("active_time", "cli", "s1")
        );
        assert_eq!((r.value, r.timestamp), (12.5, 1_760_000_060));
    }

    #[test]
    fn cumulative_unknown_and_sessionless_metrics_are_skipped() {
        let cumulative = metric_body(
            "claude_code.active_time.total",
            json!(2),
            vec![point("s1", "cli", 1.0)],
        );
        assert!(parse_metrics(&cumulative, 5).unwrap().is_empty());
        let cost = metric_body(
            "claude_code.cost.usage",
            json!(1),
            vec![point("s1", "", 1.0)],
        );
        assert!(parse_metrics(&cost, 5).unwrap().is_empty());
        let none =
            json!({"startTimeUnixNano": "1", "timeUnixNano": "2", "asInt": "3", "attributes": []});
        let body = metric_body("claude_code.commit.count", json!(1), vec![none]);
        assert!(parse_metrics(&body, 5).unwrap().is_empty());
        assert!(parse_metrics(b"{}", 5).unwrap().is_empty());
        assert!(parse_metrics(b"nope", 5).is_err());
    }

    #[test]
    fn the_four_counters_are_read_with_integer_values() {
        for (name, short) in METRICS {
            let p = json!({"startTimeUnixNano": "1760000000000000000", "timeUnixNano": "1760000060000000000", "asInt": "7",
                "attributes": [s("session.id", "s1")]});
            let rows = parse_metrics(
                &metric_body(
                    &format!("claude_code.{name}"),
                    json!("AGGREGATION_TEMPORALITY_DELTA"),
                    vec![p],
                ),
                5,
            )
            .unwrap();
            assert_eq!((rows[0].metric.as_str(), rows[0].value), (*short, 7.0));
        }
    }
}
