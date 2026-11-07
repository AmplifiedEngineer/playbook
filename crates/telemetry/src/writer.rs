// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The one thread that writes telemetry rows.
//!
//! It owns the SQLite connection, so connection threads never contend for the
//! write lock. Jobs arrive over a bounded channel; the loop takes what is
//! queued, up to a batch limit, and commits it in one transaction. Each job
//! carries a reply channel, so the connection thread answers 200 or 503 after
//! the commit, as the single threaded receiver did. The thread also prunes old
//! rows on a timer.

use super::otlp::Parsed;
use super::telemetry_db::{
    insert_error, insert_metric, insert_request, insert_tool, prune, Retention, TelemetryMetric,
};
use playbook_store::Connection;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Jobs that may wait for the writer; a full channel answers 503 at once.
pub const CHANNEL_BOUND: usize = 256;
/// Most jobs committed in one transaction.
const MAX_BATCH: usize = 64;
/// Longest a drain may take before the batch is committed.
const MAX_BATCH_WAIT: Duration = Duration::from_millis(50);
/// How often a long running writer prunes (the collector also prunes at start).
const PRUNE_EVERY: Duration = Duration::from_secs(6 * 3600);

/// The rows of one request, already parsed.
pub enum Kind {
    Metrics(Vec<TelemetryMetric>),
    Logs(Parsed),
    /// Makes the loop panic, to test the stop path.
    #[cfg(test)]
    Panic,
}

/// One request's rows and where to say whether they were stored.
pub struct Job {
    pub kind: Kind,
    pub reply: SyncSender<bool>,
}

/// Starts the writer. It sets `stop` if its loop panics, so the process ends,
/// frees its lock and the next starter replaces a collector that would answer
/// the probe and store nothing.
pub fn spawn(
    conn: Connection,
    retention: Option<Retention>,
    now: fn() -> i64,
    stop: Arc<AtomicBool>,
) -> (SyncSender<Job>, JoinHandle<()>) {
    let (tx, rx) = std::sync::mpsc::sync_channel(CHANNEL_BOUND);
    let commits = Arc::new(AtomicUsize::new(0));
    (
        tx,
        start(rx, conn, retention, now, stop, commits, PRUNE_EVERY),
    )
}

/// `spawn` with the channel, the commit counter and the prune interval given,
/// so a test can queue jobs before the loop starts and count commits.
fn start(
    rx: Receiver<Job>,
    conn: Connection,
    retention: Option<Retention>,
    now: fn() -> i64,
    stop: Arc<AtomicBool>,
    commits: Arc<AtomicUsize>,
    prune_every: Duration,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let run = AssertUnwindSafe(|| run(&rx, &conn, retention, now, &commits, prune_every));
        if catch_unwind(run).is_err() {
            stop.store(true, Ordering::SeqCst);
        }
    })
}

fn run(
    rx: &Receiver<Job>,
    conn: &Connection,
    retention: Option<Retention>,
    now: fn() -> i64,
    commits: &AtomicUsize,
    prune_every: Duration,
) {
    let mut next_prune = Instant::now() + prune_every;
    loop {
        // Wake for the prune even when no request arrives.
        let wait = next_prune.saturating_duration_since(Instant::now());
        let first = match rx.recv_timeout(wait) {
            Ok(job) => job,
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {
                next_prune = Instant::now() + prune_every;
                if let Some(r) = retention {
                    let _ = prune(conn, now(), r, false);
                }
                continue;
            }
        };
        let mut batch = vec![first];
        let drain_until = Instant::now() + MAX_BATCH_WAIT;
        while batch.len() < MAX_BATCH && Instant::now() < drain_until {
            match rx.try_recv() {
                Ok(job) => batch.push(job),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        let stored = commit(conn, &batch);
        if stored {
            commits.fetch_add(1, Ordering::SeqCst);
        }
        for job in batch {
            let _ = job.reply.send(stored);
        }
    }
}

/// Stores every job in one transaction. Any failure rolls the batch back.
fn commit(conn: &Connection, batch: &[Job]) -> bool {
    let Ok(tx) = conn.unchecked_transaction() else {
        return false;
    };
    for job in batch {
        if insert(&tx, &job.kind).is_err() {
            return false;
        }
    }
    tx.commit().is_ok()
}

fn insert(conn: &Connection, kind: &Kind) -> Result<(), String> {
    match kind {
        Kind::Metrics(points) => points
            .iter()
            .try_for_each(|p| insert_metric(conn, p).map(drop)),
        Kind::Logs(rows) => {
            rows.requests
                .iter()
                .try_for_each(|r| insert_request(conn, r).map(drop))?;
            rows.tools
                .iter()
                .try_for_each(|r| insert_tool(conn, r).map(drop))?;
            rows.errors
                .iter()
                .try_for_each(|r| insert_error(conn, r).map(drop))
        }
        #[cfg(test)]
        Kind::Panic => panic!("test panic in the writer loop"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry_db::{count_requests, migrate_up, Retention};
    use crate::util::scratch_dir;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc::sync_channel;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const FAR: Duration = Duration::from_secs(3600);

    fn db(name: &str) -> (playbook_store::Connection, playbook_store::Connection) {
        let path = scratch_dir(name).join("usage.db");
        let writer = crate::db::open_db(&path).unwrap();
        migrate_up(&writer).unwrap();
        (writer, crate::db::open_db(&path).unwrap())
    }

    fn logs(request_id: &str) -> Kind {
        let body = serde_json::json!({"resourceLogs":[{"scopeLogs":[{"logRecords":[{
        "attributes":[
            {"key":"event.name","value":{"stringValue":"api_request"}},
            {"key":"session.id","value":{"stringValue":"s1"}},
            {"key":"request_id","value":{"stringValue":request_id}}
        ]}]}]}]})
        .to_string();
        Kind::Logs(crate::otlp::parse_events(body.as_bytes(), 1).unwrap())
    }

    fn metric() -> Kind {
        let body = br#"{"resourceMetrics":[{"scopeMetrics":[{"metrics":[{"name":"claude_code.active_time.total","sum":{"aggregationTemporality":1,"dataPoints":[{"startTimeUnixNano":"1","timeUnixNano":"2000000000","asDouble":9,"attributes":[{"key":"session.id","value":{"stringValue":"s1"}},{"key":"type","value":{"stringValue":"user"}}]}]}}]}]}]}"#;
        Kind::Metrics(crate::otlp::parse_metrics(body, 1).unwrap())
    }

    fn job(kind: Kind) -> (Job, std::sync::mpsc::Receiver<bool>) {
        let (reply, answer) = sync_channel(1);
        (Job { kind, reply }, answer)
    }

    struct Parts {
        stop: Arc<AtomicBool>,
        commits: Arc<AtomicUsize>,
    }

    fn parts() -> Parts {
        Parts {
            stop: Arc::new(AtomicBool::new(false)),
            commits: Arc::new(AtomicUsize::new(0)),
        }
    }

    #[test]
    fn live_rows_of_every_telemetry_table_carry_the_local_machine_id() {
        // Arrange
        let (conn, reader) = db("writer-machine");
        let id: String = reader
            .query_row("SELECT value FROM meta WHERE key = 'machine_id'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let rows = crate::otlp::Parsed {
            requests: vec![Default::default()],
            tools: vec![Default::default()],
            errors: vec![Default::default()],
        };

        // Act
        assert!(commit(&conn, &[job(Kind::Logs(rows)).0, job(metric()).0]));

        // Assert
        for table in [
            "telemetry_requests",
            "telemetry_tools",
            "telemetry_errors",
            "telemetry_metrics",
        ] {
            let found: i64 = reader
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE machine = ?1"),
                    [&id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(found, 1, "{table}");
        }
    }

    #[test]
    fn a_hundred_queued_jobs_land_in_at_most_two_commits() {
        let (conn, reader) = db("writer-batch");
        let (tx, rx) = sync_channel(CHANNEL_BOUND);
        let mut answers = Vec::new();
        for i in 0..100 {
            let (j, a) = job(logs(&format!("r{i}")));
            tx.send(j).unwrap();
            answers.push(a);
        }
        let p = parts();
        let handle = start(
            rx,
            conn,
            None,
            || 1,
            Arc::clone(&p.stop),
            Arc::clone(&p.commits),
            FAR,
        );

        assert!(answers.iter().all(|a| a.recv().unwrap()));
        drop(tx);
        handle.join().unwrap();

        assert_eq!(count_requests(&reader).unwrap(), 100);
        assert!(p.commits.load(Ordering::SeqCst) <= 2);
    }

    #[test]
    fn a_failing_batch_replies_false_and_the_next_good_batch_commits() {
        let (conn, reader) = db("writer-fail");
        conn.execute("DROP TABLE telemetry_requests", []).unwrap();
        let p = parts();
        let (tx, handle) = spawn_with(conn, &p);

        let (bad, bad_answer) = job(logs("r1"));
        tx.send(bad).unwrap();
        assert!(!bad_answer.recv().unwrap());

        let (good, good_answer) = job(metric());
        tx.send(good).unwrap();
        assert!(good_answer.recv().unwrap());
        drop(tx);
        handle.join().unwrap();

        let stored: i64 = reader
            .query_row("SELECT COUNT(*) FROM telemetry_metrics", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, 1);
    }

    #[test]
    fn one_bad_job_rolls_back_the_whole_batch() {
        let (conn, reader) = db("writer-rollback");
        conn.execute("DROP TABLE telemetry_requests", []).unwrap();
        // Queued before the loop starts, so both land in one batch.
        let (tx, rx) = sync_channel(CHANNEL_BOUND);
        let (good, good_answer) = job(metric());
        let (bad, bad_answer) = job(logs("r1"));
        tx.send(good).unwrap();
        tx.send(bad).unwrap();
        let p = parts();
        let handle = start(
            rx,
            conn,
            None,
            || 1,
            Arc::clone(&p.stop),
            Arc::clone(&p.commits),
            FAR,
        );

        assert!(!good_answer.recv().unwrap());
        assert!(!bad_answer.recv().unwrap());
        drop(tx);
        handle.join().unwrap();

        let stored: i64 = reader
            .query_row("SELECT COUNT(*) FROM telemetry_metrics", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, 0);
        assert_eq!(p.commits.load(Ordering::SeqCst), 0);
    }

    fn spawn_with(
        conn: playbook_store::Connection,
        p: &Parts,
    ) -> (
        std::sync::mpsc::SyncSender<Job>,
        std::thread::JoinHandle<()>,
    ) {
        let (tx, rx) = sync_channel(CHANNEL_BOUND);
        let handle = start(
            rx,
            conn,
            None,
            || 1,
            Arc::clone(&p.stop),
            Arc::clone(&p.commits),
            FAR,
        );
        (tx, handle)
    }

    #[test]
    fn a_dropped_sender_still_commits_what_is_queued() {
        let (conn, reader) = db("writer-drop");
        let (tx, rx) = sync_channel(CHANNEL_BOUND);
        let (j, _answer) = job(logs("r1"));
        tx.send(j).unwrap();
        drop(tx);
        let p = parts();

        start(
            rx,
            conn,
            None,
            || 1,
            Arc::clone(&p.stop),
            Arc::clone(&p.commits),
            FAR,
        )
        .join()
        .unwrap();

        assert_eq!(count_requests(&reader).unwrap(), 1);
        assert!(!p.stop.load(Ordering::SeqCst), "a clean exit is not a stop");
    }

    #[test]
    fn a_panic_in_the_loop_sets_the_stop_flag_and_ends_the_thread() {
        let (conn, _reader) = db("writer-panic");
        let p = parts();
        let (tx, handle) = spawn_with(conn, &p);

        let (j, answer) = job(Kind::Panic);
        tx.send(j).unwrap();
        handle.join().unwrap();

        assert!(p.stop.load(Ordering::SeqCst));
        assert!(answer.recv().is_err(), "the reply is dropped, not answered");
    }

    #[test]
    fn the_writer_prunes_on_its_own_schedule() {
        let (conn, reader) = db("writer-prune");
        let p = parts();
        let keep_one = Some(Retention {
            days: 0,
            max_rows: 1,
        });
        let (tx, rx) = sync_channel(CHANNEL_BOUND);
        let handle = start(
            rx,
            conn,
            keep_one,
            || 1,
            Arc::clone(&p.stop),
            Arc::clone(&p.commits),
            Duration::from_millis(100),
        );
        for i in 0..3 {
            let (j, answer) = job(logs(&format!("r{i}")));
            tx.send(j).unwrap();
            assert!(answer.recv().unwrap());
        }

        let deadline = Instant::now() + Duration::from_secs(10);
        while count_requests(&reader).unwrap() > 1 {
            assert!(Instant::now() < deadline, "prune never ran");
            std::thread::sleep(Duration::from_millis(50));
        }
        drop(tx);
        handle.join().unwrap();
    }
}
