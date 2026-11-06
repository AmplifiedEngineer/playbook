// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! End to end check of the dual source rule behind `playbook usage`: telemetry
//! is the richer source when it is complete, transcripts are the fallback, and
//! nothing is counted twice. The fixture store is a scratch `$HOME` with three
//! transcript sessions and hand written telemetry rows. The real `~/.claude`
//! is never read.

use playbook_store::Connection;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "playbook-dual-source-{}",
            playbook::testing::run_id()
        ));
        let project = dir.join(".claude/projects/proj-one");
        fs::create_dir_all(&project).unwrap();
        let line = |session: &str, id: &str, ts: &str, input: u64, output: u64| {
            format!(
                "{{\"type\":\"assistant\",\"uuid\":\"{id}\",\"sessionId\":\"{session}\",\"timestamp\":\"{ts}\",\"cwd\":\"/w/proj-one\",\"gitBranch\":\"main\",\"message\":{{\"id\":\"msg_{id}\",\"model\":\"claude-sonnet-5\",\"content\":[],\"usage\":{{\"input_tokens\":{input},\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":{output}}}}}}}\n"
            )
        };
        let sessions = [
            (
                "sess-a",
                line("sess-a", "a1", "2026-09-01T08:00:00.000Z", 1000, 500)
                    + &line("sess-a", "a2", "2026-09-01T08:05:00.000Z", 2000, 1000),
            ),
            (
                "sess-b",
                line("sess-b", "b1", "2026-09-01T09:00:00.000Z", 3000, 700)
                    + &line("sess-b", "b2", "2026-09-01T09:05:00.000Z", 4000, 800),
            ),
            (
                "sess-c",
                line("sess-c", "c1", "2026-09-01T10:00:00.000Z", 500, 100),
            ),
        ];
        for (name, text) in sessions {
            fs::write(project.join(format!("{name}.jsonl")), text).unwrap();
        }
        fs::write(dir.join(".claude.json"), "{}").unwrap();
        Home(dir.canonicalize().unwrap())
    }

    fn totals(&self) -> Value {
        let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(["usage", "--json", "--range", "all"])
            .env("HOME", &self.0)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        v["totals"].clone()
    }

    fn db(&self) -> Connection {
        Connection::open(self.0.join(".config/playbook/data/usage.db")).unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(Path::new(&self.0));
    }
}

/// A telemetry row that matches the transcript message of the same tokens.
fn add_request(conn: &Connection, session: &str, id: &str, tokens: (u64, u64), cost: f64) {
    conn.execute(
        "INSERT OR IGNORE INTO telemetry_requests
         (session_id, request_id, timestamp, model, effort, agent_name, skill_name,
          input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, cost_usd, duration_ms)
         VALUES (?1, ?2, 1788000000, 'claude-sonnet-5', '', 'reviewer', '', ?3, ?4, 0, 0, ?5, 10)",
        playbook_store::params![session, id, tokens.0 as i64, tokens.1 as i64, cost],
    )
    .unwrap();
}

fn transcript_cost(conn: &Connection, session: &str, id: &str) -> f64 {
    conn.query_row(
        "SELECT cost_usd FROM usage_events WHERE session_id = ?1 AND event_id LIKE ?2",
        playbook_store::params![session, format!("%{id}%")],
        |r| r.get(0),
    )
    .unwrap()
}

fn state(conn: &Connection, session: &str) -> Option<String> {
    conn.query_row(
        "SELECT state FROM session_reconcile WHERE session_id = ?1",
        [session],
        |r| r.get(0),
    )
    .ok()
}

fn source(conn: &Connection, session: &str) -> Option<String> {
    conn.query_row(
        "SELECT source FROM session_sources WHERE session_id = ?1",
        [session],
        |r| r.get(0),
    )
    .ok()
}

#[test]
fn telemetry_never_changes_a_total_and_each_session_reads_from_one_source() {
    let home = Home::new();

    // Transcripts only: the fallback. This also creates the store.
    let before = home.totals();
    assert_eq!(before["messages"], 5, "{before}");

    let conn = home.db();
    let cost = |s: &str, id: &str| transcript_cost(&conn, s, id);
    // Session A: telemetry matches both messages (complete).
    add_request(&conn, "sess-a", "req_a1", (1000, 500), cost("sess-a", "a1"));
    add_request(
        &conn,
        "sess-a",
        "req_a2",
        (2000, 1000),
        cost("sess-a", "a2"),
    );
    // Session B: the receiver missed the second message (partial).
    add_request(&conn, "sess-b", "req_b1", (3000, 700), cost("sess-b", "b1"));
    // Session D: telemetry with no transcript at all. It must add nothing.
    add_request(&conn, "sess-d", "req_d1", (9000, 9000), 99.0);
    // A redelivered batch is stored once.
    add_request(&conn, "sess-a", "req_a1", (1000, 500), cost("sess-a", "a1"));
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM telemetry_requests", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 4);

    let after = home.totals();
    assert_eq!(after, before, "telemetry rows must not move any total");

    let conn = home.db();
    assert_eq!(state(&conn, "sess-a").as_deref(), Some("complete"));
    assert_eq!(source(&conn, "sess-a").as_deref(), Some("telemetry"));
    assert_eq!(state(&conn, "sess-b").as_deref(), Some("partial"));
    assert_eq!(source(&conn, "sess-b").as_deref(), Some("transcript"));
    assert_eq!(state(&conn, "sess-c"), None, "no telemetry, no verdict");
    assert_eq!(source(&conn, "sess-c"), None, "reads from transcripts");
    assert_eq!(state(&conn, "sess-d"), None);
    assert_eq!(source(&conn, "sess-d").as_deref(), Some("transcript"));

    // Telemetry turned off or gone: the totals are the same numbers.
    conn.execute_batch(
        "DROP TABLE telemetry_requests; DROP TABLE session_sources; DROP TABLE session_reconcile;",
    )
    .unwrap();
    drop(conn);
    assert_eq!(home.totals(), before, "the transcript fallback is exact");
}
