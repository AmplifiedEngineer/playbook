// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Chooses, per session, whether telemetry or transcripts is the source.
//!
//! Transcripts stay the authority. A session that has telemetry rows is
//! compared with its transcript totals:
//!
//! - `Complete`: tokens within 1 percent and cost within 0.5 percent. The
//!   session's source becomes telemetry, so its per agent split (which only
//!   telemetry has) can be shown.
//! - `Partial`: telemetry has fewer tokens than the transcript (the receiver
//!   started late or missed a batch). The source stays transcripts.
//! - `Disagree`: telemetry has more tokens, or the cost differs. The source
//!   stays transcripts and a note says by how much.
//! - No telemetry rows: nothing is recorded and the session reads from
//!   transcripts, so history from before telemetry needs no backfill.
//!
//! Never double counted: one source per session, and the totals in `--json`,
//! `--summary` and the terminal view always sum the transcript rows, priced by
//! playbook, so their meaning and UTC bucketing do not change. A `Complete`
//! session has verified totals, which makes its telemetry rows a valid
//! per agent split of the same total. Telemetry rows are deduplicated by
//! (session id, request id) when stored.
//!
//! The comparison is per session and needs no id join: the store keeps the
//! transcript's message id, which telemetry does not carry.

use super::db::load_usage_events_of_sessions;
use super::telemetry_db::{set_session_source, Source};
use playbook_store::{params, Connection};

/// Tokens may differ by this share of the transcript total.
pub const TOKEN_TOLERANCE: f64 = 0.01;
/// Cost may differ by this share of the transcript cost.
pub const COST_TOLERANCE: f64 = 0.005;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Complete,
    Partial,
    Disagree,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Complete => "complete",
            State::Partial => "partial",
            State::Disagree => "disagree",
        }
    }
}

/// One side's session totals.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Totals {
    pub tokens: u64,
    pub cost: f64,
}

/// The verdict for one session and the note that explains a mismatch.
pub fn compare(telemetry: Totals, transcript: Totals) -> (State, String) {
    let base = transcript.tokens.max(1) as f64;
    let token_gap = (telemetry.tokens as f64 - transcript.tokens as f64) / base;
    if token_gap < -TOKEN_TOLERANCE {
        let note = format!(
            "telemetry has {} of {} tokens, using transcripts",
            telemetry.tokens, transcript.tokens
        );
        return (State::Partial, note);
    }
    if token_gap > TOKEN_TOLERANCE {
        let note = format!(
            "telemetry has {} tokens and transcripts {}, using transcripts",
            telemetry.tokens, transcript.tokens
        );
        return (State::Disagree, note);
    }
    let cost_gap = (telemetry.cost - transcript.cost).abs() / transcript.cost.max(1e-9);
    if (telemetry.cost - transcript.cost).abs() > 1e-9 && cost_gap > COST_TOLERANCE {
        let note = format!(
            "telemetry cost {:.4} differs from transcript cost {:.4}, using transcripts",
            telemetry.cost, transcript.cost
        );
        return (State::Disagree, note);
    }
    (State::Complete, String::new())
}

/// A session's result.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub session_id: String,
    pub state: State,
    pub source: Source,
    pub note: String,
}

fn telemetry_totals(conn: &Connection) -> Result<Vec<(String, Totals)>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT session_id,
                SUM(input_tokens + output_tokens + cache_creation_tokens + cache_read_tokens),
                SUM(cost_usd)
             FROM telemetry_requests GROUP BY session_id ORDER BY session_id",
        )
        .map_err(|e| format!("failed to prepare telemetry totals: {e}"))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Totals {
                    tokens: r.get::<_, i64>(1)?.max(0) as u64,
                    cost: r.get::<_, f64>(2)?,
                },
            ))
        })
        .map_err(|e| format!("failed to read telemetry totals: {e}"))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| format!("failed to read a telemetry total: {e}"))
}

fn transcript_totals(conn: &Connection, session: &str) -> Result<Totals, String> {
    let events = load_usage_events_of_sessions(conn, &[session.to_string()])?;
    Ok(Totals {
        tokens: events
            .iter()
            .map(|e| {
                e.input_tokens + e.output_tokens + e.cache_creation_tokens + e.cache_read_tokens
            })
            .sum(),
        cost: events.iter().map(|e| e.cost_usd).sum(),
    })
}

/// Compares every session that has telemetry rows and records the verdicts.
/// Safe to run any number of times; it never touches the transcript tables.
pub fn reconcile(conn: &Connection, now: i64) -> Result<Vec<Verdict>, String> {
    let mut out = Vec::new();
    for (session_id, telemetry) in telemetry_totals(conn)? {
        let transcript = transcript_totals(conn, &session_id)?;
        if transcript.tokens == 0 {
            // Nothing to verify against yet; transcripts stay the source.
            set_session_source(conn, &session_id, Source::Transcript, now)?;
            conn.execute(
                "DELETE FROM session_reconcile WHERE session_id = ?1",
                [&session_id],
            )
            .map_err(|e| format!("failed to clear a verdict: {e}"))?;
            continue;
        }
        let (state, note) = compare(telemetry, transcript);
        let source = if state == State::Complete {
            Source::Telemetry
        } else {
            Source::Transcript
        };
        set_session_source(conn, &session_id, source, now)?;
        conn.execute(
            "INSERT INTO session_reconcile (session_id, state, telemetry_tokens,
                transcript_tokens, telemetry_cost, transcript_cost, note, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(session_id) DO UPDATE SET state = excluded.state,
                telemetry_tokens = excluded.telemetry_tokens,
                transcript_tokens = excluded.transcript_tokens,
                telemetry_cost = excluded.telemetry_cost,
                transcript_cost = excluded.transcript_cost,
                note = excluded.note, updated_at = excluded.updated_at",
            params![
                session_id,
                state.as_str(),
                telemetry.tokens as i64,
                transcript.tokens as i64,
                telemetry.cost,
                transcript.cost,
                note,
                now
            ],
        )
        .map_err(|e| format!("failed to record a verdict: {e}"))?;
        out.push(Verdict {
            session_id,
            state,
            source,
            note,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{count_usage_events, upsert_usage_event};
    use crate::telemetry_db::{insert_request, migrate_up, session_source, TelemetryRequest};
    use crate::UsageEvent;

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE usage_events (
                event_id TEXT PRIMARY KEY, timestamp INTEGER NOT NULL, session_id TEXT NOT NULL,
                account TEXT NOT NULL, model TEXT NOT NULL, effort TEXT NOT NULL,
                repo TEXT NOT NULL, branch TEXT NOT NULL, input_tokens INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL, cache_creation_tokens INTEGER NOT NULL,
                cache_read_tokens INTEGER NOT NULL, cost_usd REAL NOT NULL,
                cache_creation_1h_tokens INTEGER NOT NULL DEFAULT 0,
                agent TEXT NOT NULL DEFAULT 'claude-code');",
        )
        .unwrap();
        migrate_up(&conn).unwrap();
        conn
    }

    fn transcript(conn: &Connection, id: &str, session: &str, ts: i64, input: u64, output: u64) {
        let event = UsageEvent {
            event_id: id.into(),
            session_id: session.into(),
            timestamp: ts,
            model: "claude-opus-4-1".into(),
            input_tokens: input,
            output_tokens: output,
            ..UsageEvent::default()
        };
        upsert_usage_event(conn, &event).unwrap();
    }

    fn telemetry(
        conn: &Connection,
        id: &str,
        session: &str,
        ts: i64,
        tokens: (u64, u64),
        cost: f64,
    ) {
        insert_request(
            conn,
            &TelemetryRequest {
                session_id: session.into(),
                request_id: id.into(),
                timestamp: ts,
                model: "claude-opus-4-1".into(),
                input_tokens: tokens.0,
                output_tokens: tokens.1,
                cost_usd: cost,
                ..TelemetryRequest::default()
            },
        )
        .unwrap();
    }

    fn transcript_cost(conn: &Connection, session: &str) -> f64 {
        transcript_totals(conn, session).unwrap().cost
    }

    #[test]
    fn matching_totals_choose_telemetry() {
        let conn = conn();
        transcript(&conn, "m1", "s", 10, 1000, 500);
        let cost = transcript_cost(&conn, "s");
        telemetry(&conn, "r1", "s", 10, (1000, 500), cost);
        let verdicts = reconcile(&conn, 99).unwrap();
        assert_eq!(verdicts[0].state, State::Complete);
        assert_eq!(session_source(&conn, "s").unwrap(), Source::Telemetry);
    }

    #[test]
    fn fewer_telemetry_tokens_is_partial_and_keeps_transcripts() {
        let conn = conn();
        transcript(&conn, "m1", "s", 10, 1000, 500);
        telemetry(&conn, "r1", "s", 10, (400, 100), 0.0);
        let v = &reconcile(&conn, 1).unwrap()[0];
        assert_eq!((v.state, v.source), (State::Partial, Source::Transcript));
        assert!(v.note.contains("500 of 1500"), "{}", v.note);
        assert_eq!(session_source(&conn, "s").unwrap(), Source::Transcript);
    }

    #[test]
    fn more_telemetry_tokens_or_a_cost_gap_is_a_disagreement_with_a_note() {
        let conn = conn();
        transcript(&conn, "m1", "a", 10, 1000, 500);
        telemetry(&conn, "r1", "a", 10, (2000, 500), 0.0);
        transcript(&conn, "m2", "b", 10, 1000, 500);
        telemetry(
            &conn,
            "r2",
            "b",
            10,
            (1000, 500),
            transcript_cost(&conn, "b") * 1.1,
        );
        let verdicts = reconcile(&conn, 1).unwrap();
        assert!(verdicts.iter().all(|v| v.state == State::Disagree));
        assert!(verdicts.iter().all(|v| !v.note.is_empty()));
        assert!(verdicts.iter().all(|v| v.source == Source::Transcript));
    }

    #[test]
    fn the_tolerances_are_inclusive_at_the_edge() {
        let t = |tokens, cost| Totals { tokens, cost };
        assert_eq!(compare(t(990, 1.0), t(1000, 1.0)).0, State::Complete);
        assert_eq!(compare(t(1010, 1.0), t(1000, 1.0)).0, State::Complete);
        assert_eq!(compare(t(989, 1.0), t(1000, 1.0)).0, State::Partial);
        assert_eq!(compare(t(1011, 1.0), t(1000, 1.0)).0, State::Disagree);
        assert_eq!(compare(t(1000, 1.005), t(1000, 1.0)).0, State::Complete);
        assert_eq!(compare(t(1000, 1.006), t(1000, 1.0)).0, State::Disagree);
    }

    #[test]
    fn a_session_without_telemetry_is_untouched_and_reads_transcripts() {
        let conn = conn();
        transcript(&conn, "m1", "old", 10, 1000, 500);
        assert!(reconcile(&conn, 1).unwrap().is_empty());
        assert_eq!(session_source(&conn, "old").unwrap(), Source::Transcript);
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM session_reconcile", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn telemetry_without_a_transcript_waits_on_transcripts() {
        let conn = conn();
        telemetry(&conn, "r1", "s", 10, (10, 10), 0.1);
        assert!(reconcile(&conn, 1).unwrap().is_empty());
        assert_eq!(session_source(&conn, "s").unwrap(), Source::Transcript);
    }

    #[test]
    fn reconcile_is_idempotent_and_never_changes_the_transcript_rows() {
        let conn = conn();
        transcript(&conn, "m1", "s", 10, 1000, 500);
        let cost = transcript_cost(&conn, "s");
        telemetry(&conn, "r1", "s", 10, (1000, 500), cost);
        let a = reconcile(&conn, 1).unwrap();
        let b = reconcile(&conn, 2).unwrap();
        assert_eq!(a, b);
        assert_eq!(count_usage_events(&conn).unwrap(), 1);
        assert_eq!(transcript_cost(&conn, "s"), cost);
    }

    #[test]
    fn a_redelivered_request_is_not_counted_twice() {
        let conn = conn();
        transcript(&conn, "m1", "s", 10, 1000, 500);
        let cost = transcript_cost(&conn, "s");
        telemetry(&conn, "r1", "s", 10, (1000, 500), cost);
        telemetry(&conn, "r1", "s", 10, (1000, 500), cost);
        assert_eq!(reconcile(&conn, 1).unwrap()[0].state, State::Complete);
    }

    #[test]
    fn a_late_transcript_turns_a_partial_session_complete_on_the_next_run() {
        let conn = conn();
        transcript(&conn, "m1", "s", 10, 1000, 0);
        telemetry(&conn, "r1", "s", 10, (1000, 0), transcript_cost(&conn, "s"));
        telemetry(&conn, "r2", "s", 20, (0, 500), 0.0);
        assert_eq!(reconcile(&conn, 1).unwrap()[0].state, State::Disagree);
        transcript(&conn, "m2", "s", 20, 0, 500);
        let cost = transcript_cost(&conn, "s");
        conn.execute(
            "UPDATE telemetry_requests SET cost_usd = ?1 WHERE request_id = 'r2'",
            [cost - transcript_cost_of(&conn, "m1")],
        )
        .unwrap();
        assert_eq!(reconcile(&conn, 2).unwrap()[0].state, State::Complete);
    }

    fn transcript_cost_of(conn: &Connection, event: &str) -> f64 {
        let events = crate::db::load_usage_events(conn).unwrap();
        events
            .iter()
            .find(|e| e.event_id == event)
            .unwrap()
            .cost_usd
    }

    #[test]
    fn a_session_across_utc_midnight_is_one_session() {
        let conn = conn();
        let before = 1_760_054_399; // 2025-10-09 23:59:59 UTC
        let after = before + 2; // 2025-10-10 00:00:01 UTC
        transcript(&conn, "m1", "s", before, 1000, 0);
        transcript(&conn, "m2", "s", after, 0, 500);
        let cost = transcript_cost(&conn, "s");
        telemetry(
            &conn,
            "r1",
            "s",
            before,
            (1000, 0),
            transcript_cost_of(&conn, "m1"),
        );
        telemetry(
            &conn,
            "r2",
            "s",
            after,
            (0, 500),
            cost - transcript_cost_of(&conn, "m1"),
        );
        let verdicts = reconcile(&conn, 1).unwrap();
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[0].state, State::Complete);
    }
}
