// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Integration-level coverage for `playbook::gate::db`, exercised through
//! the real `open_db` the way a future CLI caller would: reopen the same
//! path and confirm no error and no duplicate rows.

use playbook::gate::db::{open_db, query_phase, record_round, upsert_phase};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch_db_path(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir()
        .join(format!(
            "playbook-gate-db-{tag}-{}-{n}",
            playbook::testing::run_id()
        ))
        .join("state.db")
}

#[test]
fn reopen_after_upserts_does_not_duplicate_rows() {
    // Arrange
    let path = scratch_db_path("reopen");
    let conn = open_db(&path).expect("first open");
    upsert_phase(
        &conn,
        "plan-a",
        "spec",
        "WARN",
        "first evidence",
        "cmd-1",
        "2026-01-01T00:00:00Z",
        "test-hash",
    )
    .expect("first upsert");
    upsert_phase(
        &conn,
        "plan-a",
        "spec",
        "PASS",
        "second evidence",
        "cmd-2",
        "2026-01-02T00:00:00Z",
        "test-hash",
    )
    .expect("second upsert");
    drop(conn);

    // Act
    let reopened = open_db(&path);

    // Assert
    assert!(
        reopened.is_ok(),
        "reopening the same path should not error: {:?}",
        reopened.err()
    );
    let row = query_phase(&reopened.unwrap(), "plan-a", "spec")
        .expect("query after reopen")
        .expect("row should survive reopen");
    assert_eq!(row.verdict, "PASS");
    assert_eq!(row.evidence, "second evidence");
}

#[test]
fn cross_plan_isolation_holds_through_real_open_db() {
    // Arrange
    let path = scratch_db_path("cross-plan");
    let conn = open_db(&path).expect("open");
    upsert_phase(
        &conn,
        "plan-a",
        "spec",
        "PASS",
        "a-evidence",
        "a-cmd",
        "2026-01-01T00:00:00Z",
        "test-hash",
    )
    .expect("upsert plan-a");
    upsert_phase(
        &conn,
        "plan-b",
        "spec",
        "FAIL",
        "b-evidence",
        "b-cmd",
        "2026-01-02T00:00:00Z",
        "test-hash",
    )
    .expect("upsert plan-b");

    // Act
    let row_a = query_phase(&conn, "plan-a", "spec")
        .expect("query plan-a")
        .expect("row a");
    let row_b = query_phase(&conn, "plan-b", "spec")
        .expect("query plan-b")
        .expect("row b");

    // Assert
    assert_eq!(row_a.verdict, "PASS");
    assert_eq!(row_b.verdict, "FAIL");
    assert_ne!(row_a.evidence, row_b.evidence);
}

#[test]
fn open_db_against_legacy_six_column_schema_adds_source_hash_column() {
    // Arrange: a database file shaped exactly like the pre-migration
    // schema, written with a raw connection so open_db never touches it first.
    let path = scratch_db_path("legacy-schema");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let legacy_conn = playbook_store::Connection::open(&path).expect("open legacy conn");
    legacy_conn
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS gate_phases (
    plan_slug TEXT NOT NULL,
    phase TEXT NOT NULL,
    verdict TEXT NOT NULL CHECK(verdict IN ('PASS','FAIL','WARN','INCONCLUSIVE')),
    evidence TEXT NOT NULL,
    command TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (plan_slug, phase)
) WITHOUT ROWID",
        )
        .expect("create legacy schema");
    drop(legacy_conn);

    // Act
    let result = open_db(&path);

    // Assert
    assert!(result.is_ok(), "expected Ok, got {:?}", result.err());
    let conn = result.unwrap();
    let mut stmt = conn.prepare("PRAGMA table_info(gate_phases)").unwrap();
    let names: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert!(
        names.contains(&"source_hash".to_string()),
        "expected source_hash column after open_db, got columns {names:?}"
    );
}

/// Reads `src/gate/db.rs` from disk, greps for the deleted gitignore shape-check by name, proving it is gone, not just unreferenced.
#[test]
fn gitignore_shape_check_function_and_call_site_fully_removed() {
    // Arrange
    let path = PathBuf::from(env!("PLAYBOOK_REPO_ROOT")).join("crates/cli/src/gate/db.rs");

    // Act
    let source = fs::read_to_string(&path).expect("src/gate/db.rs should read");

    // Assert
    assert!(
        !source.contains("ensure_state_db_gitignored"),
        "ensure_state_db_gitignored must be fully removed from {path:?}, not just unreferenced"
    );
    assert!(
        !source.contains("append_gitignore_entry"),
        "its gitignore-append helper must be fully removed from {path:?} too"
    );
}

#[test]
fn open_db_rebuilds_an_old_check_constraint_so_stuck_can_be_stored() {
    // Arrange: a database with the four-verdict CHECK and a row to keep.
    let path = scratch_db_path("stuck-migration");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let old = playbook_store::Connection::open(&path).expect("open old conn");
    old.execute_batch(
        "CREATE TABLE gate_phases (
    plan_slug TEXT NOT NULL,
    phase TEXT NOT NULL,
    verdict TEXT NOT NULL CHECK(verdict IN ('PASS','FAIL','WARN','INCONCLUSIVE')),
    evidence TEXT NOT NULL,
    command TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    source_hash TEXT,
    PRIMARY KEY (plan_slug, phase)
) WITHOUT ROWID;
INSERT INTO gate_phases VALUES ('plan-a', 'adversarial', 'FAIL', 'e', 'plan', 't0', 'h0');",
    )
    .expect("seed old schema");
    drop(old);

    // Act
    let conn = open_db(&path).expect("open_db migrates");
    upsert_phase(
        &conn,
        "plan-a",
        "test-review",
        "STUCK",
        "e",
        "plan",
        "t1",
        "h1",
    )
    .expect("STUCK is accepted after the rebuild");

    // Assert
    let kept = query_phase(&conn, "plan-a", "adversarial")
        .unwrap()
        .unwrap();
    assert_eq!(
        (kept.verdict.as_str(), kept.source_hash.as_deref()),
        ("FAIL", Some("h0"))
    );
    assert!(upsert_phase(&conn, "plan-a", "x", "BOGUS", "e", "plan", "t", "h").is_err());
    let again = open_db(&path).expect("a second open is a no-op");
    assert_eq!(
        query_phase(&again, "plan-a", "test-review")
            .unwrap()
            .unwrap()
            .verdict,
        "STUCK"
    );
}

#[test]
fn rounds_count_up_through_fail_and_restart_after_pass_or_stuck() {
    // Arrange
    let path = scratch_db_path("rounds");
    let conn = open_db(&path).expect("open db");
    let round =
        |phase: &str, verdict: &str| record_round(&conn, "plan-a", phase, verdict, "t").unwrap();

    // Act, Assert: FAIL, FAIL, INCONCLUSIVE, PASS is one series of four rounds.
    assert_eq!(round("adversarial", "FAIL"), 1);
    assert_eq!(round("adversarial", "FAIL"), 2);
    assert_eq!(round("adversarial", "INCONCLUSIVE"), 3);
    assert_eq!(round("adversarial", "PASS"), 4);
    // Phases count separately, and a PASS ends the series.
    assert_eq!(round("test-review", "FAIL"), 1);
    assert_eq!(round("adversarial", "FAIL"), 1);
    // STUCK ends a series too.
    assert_eq!(round("adversarial", "STUCK"), 2);
    assert_eq!(round("adversarial", "FAIL"), 1);
}
