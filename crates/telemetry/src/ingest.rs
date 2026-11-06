// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Incremental ingest: read events newer than the source's watermark, tag the
//! account, write them, and advance the watermark, all in one transaction. A
//! failure anywhere leaves both the rows and the watermark untouched.

use super::claude_code::ClaudeCodeSource;
use super::db;
use super::{Events, UsageSource};
use rusqlite::{Connection, Transaction, TransactionBehavior};
use std::path::PathBuf;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct IngestStats {
    pub usage_inserted: usize,
    pub usage_updated: usize,
    pub tools_inserted: usize,
    pub duplicates_skipped: usize,
}

/// The watermark trails the clock by this much, so a message stamped when its
/// generation began but written later is still seen by the next scan. Dedup
/// absorbs the overlap.
const WATERMARK_SAFETY_SECS: i64 = 900;

/// Dedup is by `event_id`, so re-reading events after a crash or through the
/// safety overlap never double-counts. A repeat usage event only raises its
/// token counts, never lowers them.
pub fn ingest(
    source: &dyn UsageSource,
    account: &str,
    conn: &Connection,
) -> Result<IngestStats, String> {
    ingest_at(source, account, conn, crate::util::time::now_secs())
}

fn ingest_at(
    source: &dyn UsageSource,
    account: &str,
    conn: &Connection,
    now: i64,
) -> Result<IngestStats, String> {
    let name = source.name();
    let watermark = db::get_watermark(conn, name)?;
    let events = source.events_since(watermark)?;

    // IMMEDIATE takes the write lock up front, so a second ingest waits out
    // `busy_timeout` instead of failing with "database is locked" when its
    // stale read snapshot cannot upgrade to a write. It also makes the
    // event_id pre-filter below race-free.
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| format!("failed to begin ingest transaction: {e}"))?;
    let mut stats = IngestStats::default();
    let newest = write_events(&tx, events, account, name, watermark, &mut stats)?;
    backfill_tools(&tx)?;

    // raise_watermark never moves backwards, so a quiet scan keeps the old value.
    db::raise_watermark(&tx, name, newest.min(now - WATERMARK_SAFETY_SECS))?;
    tx.commit()
        .map_err(|e| format!("failed to commit ingest transaction: {e}"))?;
    Ok(stats)
}

/// Writes `events` into `tx` and returns the newest timestamp seen, at least
/// `newest`. Dedup by id and the raise-only token rule are in the db calls.
fn write_events(
    tx: &Transaction,
    events: Events,
    account: &str,
    name: &str,
    mut newest: i64,
    stats: &mut IngestStats,
) -> Result<i64, String> {
    for event in events.usage {
        newest = newest.max(event.timestamp);
        let existed = db::has_usage_event(tx, &event.event_id)?;
        let mut event = event;
        event.account = account.to_string();
        if event.agent.is_empty() {
            event.agent = name.to_string();
        }
        let written = db::upsert_usage_event(tx, &event)?;
        match (existed, written) {
            (false, _) => stats.usage_inserted += 1,
            (true, 0) => stats.duplicates_skipped += 1,
            (true, _) => stats.usage_updated += 1,
        }
    }
    for event in events.tools {
        newest = newest.max(event.timestamp);
        if db::has_tool_event(tx, &event.event_id)? {
            stats.duplicates_skipped += 1;
            continue;
        }
        let mut event = event;
        event.account = account.to_string();
        db::insert_tool_event(tx, &event)?;
        stats.tools_inserted += 1;
    }
    Ok(newest)
}

/// The most time one ingest spends copying tool calls into the telemetry table.
const TOOL_BACKFILL_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

/// Copies the tool calls the transcripts show into `telemetry_tools` as
/// `transcript` rows, inside the ingest transaction. Live rows are never
/// touched, only the retention window is filled, and a long history takes a
/// few runs because each one stops at its time budget.
fn backfill_tools(tx: &Transaction) -> Result<(), String> {
    crate::telemetry_db::backfill_transcript_tools(
        tx,
        crate::util::time::now_secs(),
        crate::collector::configured_retention(),
        TOOL_BACKFILL_BUDGET,
    )
    .map(|_| ())
}

/// Set once a first-run backfill has read every transcript.
const BACKFILL_DONE: &str = "backfill-complete:claude-code";

/// Transcripts read per batch, at most, and the bytes that end a batch early.
const BATCH_FILES: usize = 250;
const BATCH_BYTES: u64 = 64 * 1024 * 1024;

/// The first read of a large history, in small committed batches, newest
/// transcript first, so the store fills while the caller watches. Each batch
/// commits its rows and the files it finished together, so an interrupted run
/// resumes where it stopped. The source watermark moves only after the last
/// batch: until then an unread older file could hold events older than it.
/// A store that already has a watermark, or the done marker, takes the plain
/// incremental `ingest`. `on_batch(done, total)` runs after each commit, with
/// file counts only. It returns false to stop: the batches so far stay
/// committed and the watermark does not move, so the next run resumes.
pub fn ingest_claude(
    source: &ClaudeCodeSource,
    account: &str,
    conn: &Connection,
    on_batch: &mut dyn FnMut(usize, usize) -> bool,
) -> Result<IngestStats, String> {
    let name = source.name();
    if db::get_watermark(conn, name)? > 0 || db::get_watermark(conn, BACKFILL_DONE)? > 0 {
        return ingest(source, account, conn);
    }
    let started = crate::util::time::now_secs();
    let mut files: Vec<(PathBuf, i64, u64)> = source
        .files()
        .into_iter()
        .map(|f| {
            let meta = std::fs::metadata(&f).ok();
            let mtime = meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64);
            (f, mtime, meta.map_or(1, |m| m.len().max(1)))
        })
        .collect();
    files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let total = files.len();
    let finished = db::backfilled_files(conn)?;
    files.retain(|(f, mtime, _)| finished.get(&f.to_string_lossy().to_string()) != Some(mtime));
    let mut done = total - files.len();
    if !on_batch(done, total) {
        return Ok(IngestStats::default());
    }
    let mut stats = IngestStats::default();
    let mut rest = files.as_slice();
    while !rest.is_empty() {
        let mut take = 0;
        let mut bytes = 0;
        while take < rest.len() && take < BATCH_FILES && (take == 0 || bytes < BATCH_BYTES) {
            bytes += rest[take].2;
            take += 1;
        }
        let (batch, later) = rest.split_at(take);
        rest = later;
        let mut paths: Vec<PathBuf> = batch.iter().map(|b| b.0.clone()).collect();
        paths.sort();
        let events = source.events_from_files(&paths, 0, super::claude_code::worker_count());
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .map_err(|e| format!("failed to begin ingest transaction: {e}"))?;
        write_events(&tx, events, account, name, 0, &mut stats)?;
        for (f, mtime, _) in batch {
            db::record_backfilled_file(&tx, &f.to_string_lossy(), *mtime)?;
        }
        tx.commit()
            .map_err(|e| format!("failed to commit ingest transaction: {e}"))?;
        done += batch.len();
        if !on_batch(done, total) {
            return Ok(stats);
        }
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| format!("failed to begin ingest transaction: {e}"))?;
    backfill_tools(&tx)?;
    let newest = db::newest_usage_of(&tx, name)?;
    db::raise_watermark(&tx, name, newest.min(started - WATERMARK_SAFETY_SECS))?;
    db::raise_watermark(&tx, BACKFILL_DONE, 1)?;
    db::clear_backfilled_files(&tx)?;
    tx.commit()
        .map_err(|e| format!("failed to commit ingest transaction: {e}"))?;
    Ok(stats)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_code::ClaudeCodeSource;
    use crate::util::scratch_dir;
    use crate::{Events, ToolKind, UsageEvent};
    use std::fs;
    use std::path::PathBuf;

    fn fixture_root(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/usage")
            .join(name)
    }

    fn count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    struct Fake(Events);
    impl UsageSource for Fake {
        fn name(&self) -> &str {
            "fake"
        }
        fn events_since(&self, _watermark: i64) -> Result<Events, String> {
            Ok(self.0.clone())
        }
    }

    fn usage(id: &str, ts: i64, cost: f64) -> UsageEvent {
        UsageEvent {
            event_id: id.into(),
            timestamp: ts,
            session_id: "s".into(),
            account: String::new(),
            model: "claude-sonnet-5".into(),
            effort: String::new(),
            repo: "r".into(),
            branch: "b".into(),
            input_tokens: 1,
            output_tokens: 1,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            cost_usd: cost,
            ..UsageEvent::default()
        }
    }

    #[test]
    fn ingest_writes_events_tags_the_account_and_advances_the_watermark() {
        let dir = scratch_dir("usage-ingest");
        let conn = db::open_db(&dir.join("usage.db")).unwrap();
        let source = ClaudeCodeSource::new(fixture_root("usage"));

        let stats = ingest(&source, "dev@example.com", &conn).unwrap();

        assert_eq!(stats.usage_inserted, 3);
        let rows = db::load_usage_events(&conn).unwrap();
        assert!(rows.iter().all(|r| r.account == "dev@example.com"));
        assert!(rows.iter().all(|r| r.agent == "claude-code"));
        assert_eq!(db::get_watermark(&conn, "claude-code").unwrap(), 1788307201);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_lost_watermark_reingests_the_same_events_without_double_counting() {
        let dir = scratch_dir("usage-dedup");
        let conn = db::open_db(&dir.join("usage.db")).unwrap();
        let source = ClaudeCodeSource::new(fixture_root("tools"));
        ingest(&source, "a", &conn).unwrap();

        // Simulate a crash that lost the watermark: the source re-returns
        // every event, so only the event_id pre-filter prevents doubles.
        conn.execute(
            "UPDATE usage_watermarks SET watermark = 0 WHERE source = 'claude-code'",
            [],
        )
        .unwrap();
        let again = ingest(&source, "a", &conn).unwrap();

        assert_eq!(count(&conn, "tool_invocation_events"), 4);
        assert_eq!(count(&conn, "usage_events"), 4);
        assert_eq!(again.tools_inserted + again.usage_inserted, 0);
        assert_eq!(again.duplicates_skipped, 8);
        let tools = db::load_tool_events(&conn).unwrap();
        assert_eq!(tools[0].kind, ToolKind::Skill);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_slower_ingest_cannot_move_the_watermark_backwards() {
        let dir = scratch_dir("usage-raise");
        let conn = db::open_db(&dir.join("usage.db")).unwrap();
        db::raise_watermark(&conn, "fake", 900).unwrap();

        db::raise_watermark(&conn, "fake", 100).unwrap();
        assert_eq!(db::get_watermark(&conn, "fake").unwrap(), 900);
        db::raise_watermark(&conn, "fake", 1200).unwrap();
        assert_eq!(db::get_watermark(&conn, "fake").unwrap(), 1200);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failure_mid_batch_rolls_back_rows_and_leaves_the_watermark() {
        let dir = scratch_dir("usage-rollback");
        let conn = db::open_db(&dir.join("usage.db")).unwrap();
        // The middle event violates the cost_usd CHECK, an unrelated schema
        // constraint, so valid rows on both sides must not survive.
        let source = Fake(Events {
            usage: vec![
                usage("ok-1", 100, 0.5),
                usage("bad", 200, -1.0),
                usage("ok-2", 300, 0.5),
            ],
            tools: Vec::new(),
        });

        let result = ingest(&source, "a", &conn);

        assert!(result.is_err());
        assert_eq!(count(&conn, "usage_events"), 0);
        assert_eq!(db::get_watermark(&conn, "fake").unwrap(), 0);
        let _ = fs::remove_dir_all(dir);
    }

    fn with_output(id: &str, output: u64) -> UsageEvent {
        let mut event = usage(id, 100, 0.5);
        event.output_tokens = output;
        event
    }

    fn fake_with(event: UsageEvent) -> Fake {
        Fake(Events {
            usage: vec![event],
            tools: Vec::new(),
        })
    }

    fn stored_output(conn: &Connection, id: &str) -> i64 {
        conn.query_row(
            "SELECT output_tokens FROM usage_events WHERE event_id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn a_later_larger_count_for_a_stored_message_raises_it() {
        let dir = scratch_dir("usage-upsert-up");
        let conn = db::open_db(&dir.join("usage.db")).unwrap();

        let first = ingest(&fake_with(with_output("m", 1)), "a", &conn).unwrap();
        let second = ingest(&fake_with(with_output("m", 770)), "a", &conn).unwrap();

        assert_eq!((first.usage_inserted, first.usage_updated), (1, 0));
        assert_eq!((second.usage_inserted, second.usage_updated), (0, 1));
        assert_eq!(stored_output(&conn, "m"), 770);
        assert_eq!(count(&conn, "usage_events"), 1);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_later_smaller_count_never_lowers_a_stored_message() {
        let dir = scratch_dir("usage-upsert-down");
        let conn = db::open_db(&dir.join("usage.db")).unwrap();

        ingest(&fake_with(with_output("m", 770)), "a", &conn).unwrap();
        let again = ingest(&fake_with(with_output("m", 1)), "a", &conn).unwrap();

        assert_eq!((again.usage_updated, again.duplicates_skipped), (0, 1));
        assert_eq!(stored_output(&conn, "m"), 770);
        let _ = fs::remove_dir_all(dir);
    }

    fn transcript_line(id: &str, timestamp: &str) -> String {
        format!(
            "{{\"type\":\"assistant\",\"sessionId\":\"s\",\"timestamp\":\"{timestamp}\",\"cwd\":\"/x/r\",\"gitBranch\":\"b\",\"message\":{{\"id\":\"{id}\",\"model\":\"claude-sonnet-5\",\"content\":[],\"usage\":{{\"input_tokens\":1,\"output_tokens\":1,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0}}}}}}\n"
        )
    }

    #[test]
    fn a_message_stamped_before_the_newest_seen_event_is_still_ingested_later() {
        let dir = scratch_dir("usage-overlap");
        let conn = db::open_db(&dir.join("usage.db")).unwrap();
        let projects = dir.join("projects");
        fs::create_dir_all(&projects).unwrap();
        let source = ClaudeCodeSource::new(projects.clone());
        let newest = 1788252682; // 2026-09-01T08:51:22Z
        let now = newest + 60;
        fs::write(
            projects.join("a.jsonl"),
            transcript_line("msg_late", "2026-09-01T08:51:22.982Z"),
        )
        .unwrap();
        ingest_at(&source, "a", &conn, now).unwrap();

        // A second session flushes a message stamped earlier than the newest
        // event the first ingest saw (2026-09-01T08:40:00Z).
        fs::write(
            projects.join("b.jsonl"),
            transcript_line("msg_early", "2026-09-01T08:40:00.000Z"),
        )
        .unwrap();
        ingest_at(&source, "a", &conn, now).unwrap();

        assert_eq!(count(&conn, "usage_events"), 2);
        assert_eq!(
            db::get_watermark(&conn, "claude-code").unwrap(),
            now - WATERMARK_SAFETY_SECS
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// `n` transcripts of one message each, file `i` last modified `i` seconds
    /// after the epoch plus a base, so a higher index is newer.
    fn transcripts(name: &str, n: usize) -> (PathBuf, ClaudeCodeSource) {
        let root = scratch_dir(name);
        let now = crate::util::time::now_secs();
        for i in 0..n {
            let dir = root.join(format!("proj{}", i % 3));
            fs::create_dir_all(&dir).unwrap();
            let ts = playbook_common::time::iso_utc(now - 3_600 * 24 + i as i64);
            let line = format!(
                r#"{{"type":"assistant","timestamp":"{ts}","sessionId":"s{i}","cwd":"/fake/p","message":{{"id":"msg_{i}","model":"claude-sonnet-5","usage":{{"input_tokens":10,"output_tokens":5}}}}}}"#
            );
            let path = dir.join(format!("f{i}.jsonl"));
            fs::write(&path, line + "\n").unwrap();
            let when =
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000 + i as u64);
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(when)
                .unwrap();
        }
        (root.clone(), ClaudeCodeSource::new(root))
    }

    const TOTAL: usize = BATCH_FILES * 2 + 20;

    #[test]
    fn a_first_read_goes_in_batches_newest_files_first_and_resumes_after_an_interrupt() {
        let (_root, source) = transcripts("usage-batched", TOTAL);
        let dir = scratch_dir("usage-batched-db");
        let conn = db::open_db(&dir.join("usage.db")).unwrap();

        // Stop after the first batch commits.
        let mut seen = Vec::new();
        ingest_claude(&source, "a", &conn, &mut |done, total| {
            seen.push((done, total));
            done == 0
        })
        .unwrap();
        assert_eq!(seen, vec![(0, TOTAL), (BATCH_FILES, TOTAL)]);
        let stored: Vec<String> = db::load_usage_events(&conn)
            .unwrap()
            .into_iter()
            .map(|e| e.event_id)
            .collect();
        assert_eq!(stored.len(), BATCH_FILES);
        // The newest files come first.
        assert!(stored.contains(&format!("msg_{}", TOTAL - 1)));
        assert!(!stored.contains(&"msg_0".to_string()));
        // An interrupted run never moves the watermark past unread files.
        assert_eq!(db::get_watermark(&conn, "claude-code").unwrap(), 0);
        assert_eq!(db::backfilled_files(&conn).unwrap().len(), BATCH_FILES);

        // The next run starts from the files left and finishes.
        let mut seen = Vec::new();
        ingest_claude(&source, "a", &conn, &mut |done, total| {
            seen.push((done, total));
            true
        })
        .unwrap();
        assert_eq!(seen.first(), Some(&(BATCH_FILES, TOTAL)));
        assert_eq!(seen.last(), Some(&(TOTAL, TOTAL)));
        assert_eq!(count(&conn, "usage_events"), TOTAL as i64);
        assert!(db::get_watermark(&conn, "claude-code").unwrap() > 0);
        assert_eq!(count(&conn, "usage_backfill_files"), 0);

        // A finished store takes the incremental path and reports no batches.
        let mut calls = 0;
        ingest_claude(&source, "a", &conn, &mut |_, _| {
            calls += 1;
            true
        })
        .unwrap();
        assert_eq!(calls, 0);
        assert_eq!(count(&conn, "usage_events"), TOTAL as i64);
    }

    #[test]
    fn a_batched_first_read_stores_what_the_plain_ingest_stores() {
        let (_root, source) = transcripts("usage-batched-same", 55);
        let a = db::open_db(&scratch_dir("usage-same-a").join("usage.db")).unwrap();
        let b = db::open_db(&scratch_dir("usage-same-b").join("usage.db")).unwrap();
        ingest(&source, "x", &a).unwrap();
        ingest_claude(&source, "x", &b, &mut |_, _| true).unwrap();
        let ids = |c: &Connection| {
            let mut v: Vec<(String, i64, u64)> = db::load_usage_events(c)
                .unwrap()
                .into_iter()
                .map(|e| (e.event_id, e.timestamp, e.output_tokens))
                .collect();
            v.sort();
            v
        };
        assert_eq!(ids(&a), ids(&b));
        assert_eq!(
            db::get_watermark(&a, "claude-code").unwrap(),
            db::get_watermark(&b, "claude-code").unwrap()
        );
    }

    #[test]
    fn an_existing_watermark_skips_the_backfill() {
        let (_root, source) = transcripts("usage-batched-old", 5);
        let conn = db::open_db(&scratch_dir("usage-old-db").join("usage.db")).unwrap();
        db::raise_watermark(&conn, "claude-code", 5).unwrap();
        let mut calls = 0;
        ingest_claude(&source, "a", &conn, &mut |_, _| {
            calls += 1;
            true
        })
        .unwrap();
        assert_eq!(calls, 0);
        assert_eq!(count(&conn, "usage_events"), 5);
    }

    #[test]
    fn ingest_copies_tool_calls_into_the_telemetry_table_once_and_keeps_live_rows() {
        let dir = scratch_dir("usage-tool-backfill");
        let conn = db::open_db(&dir.join("usage.db")).unwrap();
        let now = crate::util::time::now_secs();
        let call = |id: &str, kind: ToolKind| crate::ToolInvocationEvent {
            event_id: id.into(),
            timestamp: now - 60,
            session_id: "s".into(),
            account: String::new(),
            kind,
            name: "playbook:reviewer".into(),
        };
        conn.execute(
            "INSERT INTO telemetry_tools VALUES ('s', 'tu_live', ?1, 'Agent', 0, 250, 'otlp')",
            [now - 60],
        )
        .unwrap();
        let source = Fake(Events {
            tools: vec![
                call("tu_live", ToolKind::Agent),
                call("tu_a", ToolKind::Agent),
                call("tu_s", ToolKind::Skill),
            ],
            ..Events::default()
        });
        ingest(&source, "a", &conn).unwrap();
        ingest(&source, "a", &conn).unwrap();

        assert_eq!(count(&conn, "telemetry_tools"), 3);
        let live: (String, i64, i64) = conn
            .query_row(
                "SELECT source, success, duration_ms FROM telemetry_tools WHERE tool_use_id = 'tu_live'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(live, ("otlp".to_string(), 0, 250));
        let _ = fs::remove_dir_all(dir);
    }
}
