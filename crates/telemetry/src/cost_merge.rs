// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Joins telemetry cost onto transcript events, so every view sums the same
//! per request figures.
//!
//! A transcript message and a telemetry request are one API call, and the
//! transcript line carries the `requestId` that the telemetry row calls
//! `request_id`. The join is by (session id, request id), so it is exact.
//!
//! - A request telemetry has a cost for takes that cost. Claude Code computed
//!   it, so it beats the price table estimate.
//! - A request only telemetry saw is added as an event of its own.
//! - A request telemetry missed keeps the estimate from the price table. When
//!   its model has no table price but telemetry priced other requests of that
//!   model, the estimate uses the per token rate learned from them. A model
//!   with neither stays unpriced.
//!
//! Requests are added only once the stored transcript rows have been given
//! their request ids (see `backfill`). A row the backfill could not fill (its
//! transcript is gone, say) stays without one, so a telemetry request with no
//! exact match is also compared with the rows of its session that have none:
//! the same model and token counts mean it is that row, and it is not added.
//! When more than one such row fits, the request is still not added, since
//! counting twice is the worse mistake.

use super::db::get_watermark;
use super::{CostSource, UsageEvent};
use playbook_store::{params, Connection};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Set by the backfill that filled `request_id` on rows stored before it.
pub const REQUEST_ID_BACKFILL_MARKER: &str = "backfill:request-id-v1";

/// A rate is learned from at least this many priced telemetry requests.
const LEARN_MIN_REQUESTS: i64 = 3;
/// How a model's token kinds weigh against one input token when a rate is
/// learned: output costs 5 times, a cache write 1.25 times and a cache read
/// a tenth, the ratios of the published Claude and GPT prices.
const OUTPUT_WEIGHT: f64 = 5.0;
const CACHE_WRITE_WEIGHT: f64 = 1.25;
const CACHE_READ_WEIGHT: f64 = 0.1;

const TELEMETRY_AGENT: &str = "claude-code";

/// Which events the merge was asked about.
#[derive(Debug, Clone, Copy)]
pub enum Extent<'a> {
    /// Every event.
    All,
    /// Events from this time on.
    Since(i64),
    /// Every event of these sessions.
    Sessions(&'a [String]),
    /// A handful of events picked by time. Costs are replaced but no request
    /// is added, since the rest of the window is not loaded.
    Picked,
}

fn weighted(e: &UsageEvent) -> f64 {
    e.input_tokens as f64
        + e.output_tokens as f64 * OUTPUT_WEIGHT
        + e.cache_creation_tokens as f64 * CACHE_WRITE_WEIGHT
        + e.cache_read_tokens as f64 * CACHE_READ_WEIGHT
}

/// Replaces estimated costs with telemetry costs, adds requests only telemetry
/// saw and learns rates for models the price table lacks. The result is sorted
/// oldest first, like the loaders' own order, except for `Picked`.
pub fn merge(
    conn: &Connection,
    mut events: Vec<UsageEvent>,
    extent: Extent<'_>,
) -> Result<Vec<UsageEvent>, String> {
    if !telemetry_exists(conn) {
        return Ok(events);
    }
    apply_telemetry_costs(conn, &mut events)?;
    apply_learned_rates(conn, &mut events)?;
    let from = match extent {
        Extent::Picked => return Ok(events),
        Extent::All => i64::MIN,
        Extent::Since(t) => t,
        Extent::Sessions(_) => events.iter().map(|e| e.timestamp).min().unwrap_or(i64::MAX),
    };
    // An unreadable marker counts as not done: nothing is added.
    if get_watermark(conn, REQUEST_ID_BACKFILL_MARKER).unwrap_or(0) > 0 {
        let wanted = match extent {
            Extent::Sessions(ids) => Some(ids),
            _ => None,
        };
        let (added, orphan_costs) = telemetry_only(conn, from, wanted, &events)?;
        for event in &mut events {
            if let Some(cost) = orphan_costs.get(&event.event_id) {
                event.cost_usd = *cost;
                event.unpriced = false;
                event.cost_source = CostSource::Telemetry;
            }
        }
        events.extend(added);
        events.sort_by(|a, b| (a.timestamp, &a.event_id).cmp(&(b.timestamp, &b.event_id)));
    }
    Ok(events)
}

fn telemetry_exists(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM telemetry_requests)",
        [],
        |r| r.get(0),
    )
    .unwrap_or(false)
}

/// Telemetry is the small side, so it drives the join and each row looks its
/// transcript row up by (session, request). Left to itself SQLite walks every
/// stored message instead, which took 20 seconds on 270 thousand rows.
const COST_JOIN_SQL: &str = "SELECT u.event_id, t.cost_usd
     FROM telemetry_requests t
     CROSS JOIN usage_events u INDEXED BY usage_events_by_request
       ON u.session_id = t.session_id AND u.request_id = t.request_id
     WHERE u.timestamp BETWEEN ?1 AND ?2 AND u.request_id != '' AND t.cost_usd > 0";

fn apply_telemetry_costs(conn: &Connection, events: &mut [UsageEvent]) -> Result<(), String> {
    let (Some(first), Some(last)) = (
        events.iter().map(|e| e.timestamp).min(),
        events.iter().map(|e| e.timestamp).max(),
    ) else {
        return Ok(());
    };
    let mut stmt = conn
        .prepare(COST_JOIN_SQL)
        .map_err(|e| format!("failed to prepare the telemetry cost join: {e}"))?;
    let costs: HashMap<String, f64> = stmt
        .query_map(params![first, last], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| format!("failed to read telemetry costs: {e}"))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("failed to read a telemetry cost: {e}"))?;
    for event in events {
        if let Some(cost) = costs.get(&event.event_id) {
            event.cost_usd = *cost;
            event.unpriced = false;
            event.cost_source = CostSource::Telemetry;
        }
    }
    Ok(())
}

struct Learned {
    model: String,
    cost: f64,
    weighted_tokens: f64,
    requests: i64,
}

fn learned_rates(conn: &Connection) -> Result<Vec<Learned>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT model, COUNT(*), SUM(cost_usd), SUM(input_tokens), SUM(output_tokens),
                    SUM(cache_creation_tokens), SUM(cache_read_tokens)
             FROM telemetry_requests WHERE cost_usd > 0 GROUP BY model",
        )
        .map_err(|e| format!("failed to prepare the learned rates: {e}"))?;
    let rows = stmt
        .query_map([], |r| {
            let tokens = |i| r.get::<_, i64>(i).map(|n| n.max(0) as f64);
            Ok(Learned {
                model: r.get(0)?,
                requests: r.get(1)?,
                cost: r.get(2)?,
                weighted_tokens: tokens(3)?
                    + tokens(4)? * OUTPUT_WEIGHT
                    + tokens(5)? * CACHE_WRITE_WEIGHT
                    + tokens(6)? * CACHE_READ_WEIGHT,
            })
        })
        .map_err(|e| format!("failed to read the learned rates: {e}"))?;
    let mut merged: Vec<Learned> = Vec::new();
    for row in rows {
        let row = row.map_err(|e| format!("failed to read a learned rate: {e}"))?;
        match merged
            .iter_mut()
            .find(|m| crate::pricing::same_model(&m.model, &row.model))
        {
            Some(m) => {
                m.cost += row.cost;
                m.weighted_tokens += row.weighted_tokens;
                m.requests += row.requests;
            }
            None => merged.push(row),
        }
    }
    Ok(merged)
}

fn apply_learned_rates(conn: &Connection, events: &mut [UsageEvent]) -> Result<(), String> {
    let unpriced: BTreeSet<&str> = events
        .iter()
        .filter(|e| e.unpriced)
        .map(|e| e.model.as_str())
        .collect();
    if unpriced.is_empty() {
        return Ok(());
    }
    let learned = learned_rates(conn)?;
    for event in events.iter_mut().filter(|e| e.unpriced) {
        let rate = learned
            .iter()
            .find(|l| crate::pricing::same_model(&l.model, &event.model))
            .filter(|l| l.requests >= LEARN_MIN_REQUESTS && l.weighted_tokens > 0.0)
            .map(|l| l.cost / l.weighted_tokens);
        if let Some(rate) = rate {
            event.cost_usd = rate * weighted(event);
            event.unpriced = false;
            event.cost_source = CostSource::Learned;
        }
    }
    Ok(())
}

/// A stored transcript row that has no request id.
struct Orphan {
    event_id: String,
    session_id: String,
    model: String,
    tokens: [u64; 4],
    taken: bool,
}

fn orphans(conn: &Connection, from: i64) -> Result<Vec<Orphan>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT event_id, session_id, model, input_tokens, output_tokens,
                    cache_creation_tokens, cache_read_tokens
             FROM usage_events
             WHERE request_id = '' AND agent = ?2
               AND session_id IN (SELECT t.session_id FROM telemetry_requests t
                                  WHERE t.timestamp >= ?1)",
        )
        .map_err(|e| format!("failed to prepare the rows without a request id: {e}"))?;
    let rows = stmt
        .query_map(params![from, TELEMETRY_AGENT], |r| {
            let tokens = |i| r.get::<_, i64>(i).map(|n| n.max(0) as u64);
            Ok(Orphan {
                event_id: r.get(0)?,
                session_id: r.get(1)?,
                model: r.get(2)?,
                tokens: [tokens(3)?, tokens(4)?, tokens(5)?, tokens(6)?],
                taken: false,
            })
        })
        .map_err(|e| format!("failed to read the rows without a request id: {e}"))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| format!("failed to read a row without a request id: {e}"))
}

/// The telemetry requests no transcript row answers, and the telemetry cost of
/// each row without a request id that exactly one request matched.
fn telemetry_only(
    conn: &Connection,
    from: i64,
    sessions: Option<&[String]>,
    loaded: &[UsageEvent],
) -> Result<(Vec<UsageEvent>, BTreeMap<String, f64>), String> {
    let mut by_session: HashMap<String, Vec<Orphan>> = HashMap::new();
    for orphan in orphans(conn, from)? {
        by_session
            .entry(orphan.session_id.clone())
            .or_default()
            .push(orphan);
    }
    let mut homes: HashMap<&str, &UsageEvent> = HashMap::new();
    for event in loaded {
        homes.entry(event.session_id.as_str()).or_insert(event);
    }
    let mut orphan_costs = BTreeMap::new();
    let mut stmt = conn
        .prepare(
            "SELECT t.session_id, t.request_id, t.timestamp, t.model, t.effort,
                    t.input_tokens, t.output_tokens, t.cache_creation_tokens,
                    t.cache_read_tokens, t.cost_usd, t.machine
             FROM telemetry_requests t
             WHERE t.timestamp >= ?1 AND t.cost_usd > 0
               AND NOT EXISTS (SELECT 1 FROM usage_events u
                               WHERE u.session_id = t.session_id AND u.request_id = t.request_id)",
        )
        .map_err(|e| format!("failed to prepare the telemetry only requests: {e}"))?;
    let rows = stmt
        .query_map(params![from], |r| {
            let tokens = |i| r.get::<_, i64>(i).map(|n| n.max(0) as u64);
            let session_id: String = r.get(0)?;
            let request_id: String = r.get(1)?;
            Ok(UsageEvent {
                event_id: format!("telemetry:{session_id}:{request_id}"),
                session_id,
                request_id,
                timestamp: r.get(2)?,
                model: r.get(3)?,
                effort: r.get(4)?,
                input_tokens: tokens(5)?,
                output_tokens: tokens(6)?,
                cache_creation_tokens: tokens(7)?,
                cache_read_tokens: tokens(8)?,
                cost_usd: r.get(9)?,
                machine: r.get(10)?,
                agent: TELEMETRY_AGENT.to_string(),
                cost_source: CostSource::Telemetry,
                ..UsageEvent::default()
            })
        })
        .map_err(|e| format!("failed to read the telemetry only requests: {e}"))?;
    let mut out = Vec::new();
    for row in rows {
        let mut event = row.map_err(|e| format!("failed to read a telemetry request: {e}"))?;
        if sessions.is_some_and(|ids| !ids.contains(&event.session_id)) {
            continue;
        }
        let tokens = [
            event.input_tokens,
            event.output_tokens,
            event.cache_creation_tokens,
            event.cache_read_tokens,
        ];
        let fits = |o: &Orphan| {
            !o.taken && o.tokens == tokens && crate::pricing::same_model(&o.model, &event.model)
        };
        let same_session = by_session.get_mut(&event.session_id);
        let matching = same_session
            .as_ref()
            .map_or(0, |rows| rows.iter().filter(|o| fits(o)).count());
        if matching > 0 {
            let first = same_session.and_then(|rows| rows.iter_mut().find(|o| fits(o)));
            if let Some(orphan) = first {
                orphan.taken = true;
                if matching == 1 {
                    orphan_costs.insert(orphan.event_id.clone(), event.cost_usd);
                }
            }
            continue;
        }
        if let Some(known) = homes.get(event.session_id.as_str()) {
            event.account = known.account.clone();
            event.repo = known.repo.clone();
            event.branch = known.branch.clone();
        } else {
            event.account = crate::account::UNKNOWN_ACCOUNT.to_string();
        }
        out.push(event);
    }
    Ok((out, orphan_costs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{load_usage_events, load_usage_events_since, open_db, upsert_usage_event};
    use crate::telemetry_db::{insert_request, TelemetryRequest};

    fn store(tag: &str) -> Connection {
        let dir = crate::util::scratch_dir(tag);
        open_db(&dir.join("usage.db")).unwrap()
    }

    fn transcript(id: &str, request: &str, ts: i64, model: &str) -> UsageEvent {
        UsageEvent {
            event_id: id.into(),
            request_id: request.into(),
            session_id: "s".into(),
            timestamp: ts,
            account: "a@b.c".into(),
            repo: "repo".into(),
            model: model.into(),
            agent: "claude-code".into(),
            input_tokens: 1_000_000,
            ..UsageEvent::default()
        }
    }

    fn request(id: &str, ts: i64, model: &str, cost: f64) -> TelemetryRequest {
        TelemetryRequest {
            session_id: "s".into(),
            request_id: id.into(),
            timestamp: ts,
            model: model.into(),
            input_tokens: 1_000_000,
            cost_usd: cost,
            ..TelemetryRequest::default()
        }
    }

    fn mark_backfilled(conn: &Connection) {
        crate::db::raise_watermark(conn, REQUEST_ID_BACKFILL_MARKER, 1).unwrap();
    }

    #[test]
    fn a_request_with_telemetry_cost_takes_it_and_others_keep_the_estimate() {
        let conn = store("merge-override");
        upsert_usage_event(&conn, &transcript("m1", "r1", 100, "claude-opus-5-5")).unwrap();
        upsert_usage_event(&conn, &transcript("m2", "r2", 200, "claude-opus-5-5")).unwrap();
        insert_request(&conn, &request("r1", 100, "claude-opus-5-5", 9.5)).unwrap();

        let events = load_usage_events(&conn).unwrap();

        assert_eq!(events[0].cost_usd, 9.5);
        assert_eq!(events[0].cost_source, CostSource::Telemetry);
        assert_eq!(events[1].cost_usd, 4.0);
        assert_eq!(events[1].cost_source, CostSource::Table);
    }

    #[test]
    fn the_cost_join_walks_telemetry_and_looks_rows_up_by_request() {
        let conn = store("merge-plan");
        let plan: Vec<String> = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {COST_JOIN_SQL}"))
            .unwrap()
            .query_map(params![0, 1], |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();

        assert!(plan[0].starts_with("SCAN t"), "{plan:?}");
        assert!(plan[1].contains("usage_events_by_request"), "{plan:?}");
    }

    #[test]
    fn a_zero_telemetry_cost_does_not_beat_the_estimate() {
        let conn = store("merge-zero");
        upsert_usage_event(&conn, &transcript("m1", "r1", 100, "claude-opus-5-5")).unwrap();
        insert_request(&conn, &request("r1", 100, "claude-opus-5-5", 0.0)).unwrap();

        let events = load_usage_events(&conn).unwrap();

        assert_eq!(
            (events[0].cost_usd, events[0].cost_source),
            (4.0, CostSource::Table)
        );
    }

    #[test]
    fn a_request_only_telemetry_saw_is_added_once_the_backfill_is_done() {
        let conn = store("merge-add");
        upsert_usage_event(&conn, &transcript("m1", "r1", 100, "claude-opus-5-5")).unwrap();
        insert_request(&conn, &request("r1", 100, "claude-opus-5-5", 4.0)).unwrap();
        insert_request(&conn, &request("r9", 150, "claude-opus-5-5", 2.0)).unwrap();

        let before = load_usage_events(&conn).unwrap();
        mark_backfilled(&conn);
        let after = load_usage_events(&conn).unwrap();
        let since = load_usage_events_since(&conn, 120).unwrap();

        assert_eq!(
            before.len(),
            1,
            "an old row without its id could double count"
        );
        assert_eq!(after.len(), 2);
        assert_eq!(after[1].event_id, "telemetry:s:r9");
        assert_eq!(after[1].cost_usd, 2.0);
        assert_eq!(after[1].account, "a@b.c", "the session's account is reused");
        assert_eq!(after[1].repo, "repo");
        assert_eq!(since.len(), 1);
    }

    #[test]
    fn a_row_the_backfill_could_not_fill_and_its_telemetry_request_count_once() {
        let conn = store("merge-orphan");
        let mut orphan = transcript("m1", "", 100, "claude-opus-5-5");
        orphan.output_tokens = 7;
        upsert_usage_event(&conn, &orphan).unwrap();
        let mut seen = request("r1", 100, "claude-opus-5-5", 9.0);
        seen.output_tokens = 7;
        insert_request(&conn, &seen).unwrap();
        mark_backfilled(&conn);

        let events = load_usage_events(&conn).unwrap();

        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].event_id, "m1");
        assert_eq!(events[0].cost_usd, 9.0, "telemetry still wins");
        assert_eq!(events[0].cost_source, CostSource::Telemetry);
    }

    #[test]
    fn an_orphan_with_other_tokens_or_model_does_not_hide_a_request() {
        let conn = store("merge-orphan-other");
        upsert_usage_event(&conn, &transcript("m1", "", 100, "claude-opus-5-5")).unwrap();
        insert_request(&conn, &request("r1", 100, "claude-sonnet-5-5", 1.0)).unwrap();
        let mut more = request("r2", 110, "claude-opus-5-5", 2.0);
        more.input_tokens = 5;
        insert_request(&conn, &more).unwrap();
        mark_backfilled(&conn);

        let events = load_usage_events(&conn).unwrap();

        assert_eq!(events.len(), 3, "{events:?}");
    }

    #[test]
    fn an_ambiguous_orphan_match_adds_nothing_and_keeps_the_estimate() {
        let conn = store("merge-orphan-ambiguous");
        upsert_usage_event(&conn, &transcript("m1", "", 100, "claude-opus-5-5")).unwrap();
        upsert_usage_event(&conn, &transcript("m2", "", 101, "claude-opus-5-5")).unwrap();
        insert_request(&conn, &request("r1", 100, "claude-opus-5-5", 9.0)).unwrap();
        mark_backfilled(&conn);

        let events = load_usage_events(&conn).unwrap();

        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|e| e.cost_usd == 4.0), "{events:?}");
    }

    #[test]
    fn telemetry_for_a_session_with_no_transcript_rows_still_counts() {
        let conn = store("merge-no-transcript");
        upsert_usage_event(&conn, &transcript("m1", "r1", 100, "claude-opus-5-5")).unwrap();
        let mut gone = request("r9", 150, "claude-opus-5-5", 2.0);
        gone.session_id = "deleted-session".into();
        insert_request(&conn, &gone).unwrap();
        mark_backfilled(&conn);

        let events = load_usage_events(&conn).unwrap();

        assert_eq!(events.len(), 2);
        assert_eq!(events[1].session_id, "deleted-session");
        assert_eq!(events[1].cost_usd, 2.0);
    }

    #[test]
    fn a_model_without_a_table_price_learns_its_rate_from_telemetry() {
        let conn = store("merge-learn");
        let model = "claude-newmodel-9";
        for i in 0..LEARN_MIN_REQUESTS {
            insert_request(&conn, &request(&format!("t{i}"), 10 + i, model, 3.0)).unwrap();
        }
        upsert_usage_event(&conn, &transcript("m1", "x1", 500, model)).unwrap();
        upsert_usage_event(&conn, &transcript("m2", "x2", 600, "claude-other-1")).unwrap();

        let events = load_usage_events(&conn).unwrap();

        // Each telemetry request had 1M input tokens for $3, so $3 per M.
        assert!((events[0].cost_usd - 3.0).abs() < 1e-9);
        assert_eq!(events[0].cost_source, CostSource::Learned);
        assert!(!events[0].unpriced);
        assert!(
            events[1].unpriced,
            "no telemetry for this model: still unpriced"
        );
        assert_eq!(events[1].cost_usd, 0.0);
    }

    #[test]
    fn too_few_telemetry_requests_teach_no_rate() {
        let conn = store("merge-learn-few");
        let model = "claude-newmodel-9";
        insert_request(&conn, &request("t0", 10, model, 3.0)).unwrap();
        upsert_usage_event(&conn, &transcript("m1", "x1", 500, model)).unwrap();

        let events = load_usage_events(&conn).unwrap();

        assert!(events[0].unpriced);
    }

    #[test]
    fn without_telemetry_rows_events_are_untouched() {
        let conn = store("merge-none");
        upsert_usage_event(&conn, &transcript("m1", "r1", 100, "claude-opus-5-5")).unwrap();

        let events = load_usage_events(&conn).unwrap();

        assert_eq!(events[0].cost_source, CostSource::Table);
        assert_eq!(events[0].cost_usd, 4.0);
    }
}
