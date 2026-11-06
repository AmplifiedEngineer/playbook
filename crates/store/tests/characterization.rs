// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Behavior of the engine that callers rely on, pinned on real database files.

use std::cell::Cell;
use std::path::PathBuf;
use std::time::Duration;

use playbook_store::{
    params, retry_busy_only, retry_busy_or_locked, retry_with_pause, Connection, Error, ErrorCode,
    OpenFlags, OptionalExtension,
};

/// A fresh database path in its own folder, named by test and process id.
fn db_path(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("playbook-store-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp folder");
    dir.join("test.db")
}

fn code_of<T>(result: &Result<T, Error>) -> Option<ErrorCode> {
    match result {
        Err(Error::SqliteFailure(err, _)) => Some(err.code),
        _ => None,
    }
}

/// Opens `path` with no busy wait, so a held lock fails the statement at once.
fn open_no_wait(path: &PathBuf) -> Connection {
    let conn = Connection::open(path).expect("open");
    conn.busy_timeout(Duration::ZERO).expect("busy_timeout");
    conn
}

fn seed_table(path: &PathBuf) {
    let conn = Connection::open(path).expect("open");
    conn.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT);")
        .expect("create");
}

#[test]
fn a_busy_write_succeeds_when_the_lock_clears_inside_the_budget() {
    // Arrange
    let path = db_path("busy_clears");
    seed_table(&path);
    let holder = Connection::open(&path).expect("open holder");
    holder.execute_batch("BEGIN IMMEDIATE").expect("hold lock");
    let writer = open_no_wait(&path);
    let pauses = Cell::new(0);

    // Act
    let got = retry_with_pause(
        || writer.execute("INSERT INTO t (v) VALUES ('a')", []),
        Duration::from_secs(30),
        &[ErrorCode::DatabaseBusy],
        || {
            pauses.set(pauses.get() + 1);
            if pauses.get() == 3 {
                holder.execute_batch("COMMIT").expect("release lock");
            }
        },
    );

    // Assert
    assert_eq!(got.ok(), Some(1));
    assert_eq!(pauses.get(), 3);
}

#[test]
fn a_write_that_stays_busy_returns_the_busy_code_after_the_budget() {
    // Arrange
    let path = db_path("busy_stays");
    seed_table(&path);
    let holder = Connection::open(&path).expect("open holder");
    holder.execute_batch("BEGIN IMMEDIATE").expect("hold lock");
    let writer = open_no_wait(&path);

    // Act
    let got = retry_with_pause(
        || writer.execute("INSERT INTO t (v) VALUES ('a')", []),
        Duration::from_millis(30),
        &[ErrorCode::DatabaseBusy],
        || std::thread::sleep(Duration::from_millis(5)),
    );

    // Assert
    assert_eq!(code_of(&got), Some(ErrorCode::DatabaseBusy));
}

#[test]
fn the_busy_only_policy_does_not_retry_a_locked_error() {
    // Arrange
    let calls = Cell::new(0);
    let locked = || {
        calls.set(calls.get() + 1);
        Err::<(), _>(Error::SqliteFailure(
            playbook_store::ffi::Error::new(playbook_store::ffi::SQLITE_LOCKED),
            None,
        ))
    };

    // Act
    let only = retry_busy_only(locked, Duration::from_millis(50));
    let after_only = calls.get();
    calls.set(0);
    let both = retry_busy_or_locked(locked, Duration::from_millis(50));

    // Assert
    assert_eq!(code_of(&only), Some(ErrorCode::DatabaseLocked));
    assert_eq!(after_only, 1);
    assert_eq!(code_of(&both), Some(ErrorCode::DatabaseLocked));
    assert!(calls.get() > 1);
}

#[test]
fn a_write_on_a_read_only_open_returns_the_read_only_code() {
    // Arrange
    let path = db_path("read_only");
    seed_table(&path);
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open");

    // Act
    let got = conn.execute("INSERT INTO t (v) VALUES ('a')", []);

    // Assert
    assert_eq!(code_of(&got), Some(ErrorCode::ReadOnly));
}

#[test]
fn a_full_text_match_returns_the_exact_ids_in_order() {
    // Arrange
    let path = db_path("fts5");
    let conn = Connection::open(&path).expect("open");
    conn.execute_batch(
        "CREATE VIRTUAL TABLE docs USING fts5(body);
         INSERT INTO docs (rowid, body) VALUES
           (1, 'alpha beta'), (2, 'gamma'), (3, 'beta beta'), (4, 'delta beta');",
    )
    .expect("seed");

    // Act
    let ids: Vec<i64> = conn
        .prepare("SELECT rowid FROM docs WHERE docs MATCH ?1 ORDER BY rowid")
        .expect("prepare")
        .query_map(params!["beta"], |r| r.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");

    // Assert
    assert_eq!(ids, vec![1, 3, 4]);
}

#[test]
fn a_dropped_transaction_rolls_back() {
    // Arrange
    let path = db_path("rollback");
    seed_table(&path);
    let mut conn = Connection::open(&path).expect("open");

    // Act
    {
        let tx = conn.transaction().expect("begin");
        tx.execute("INSERT INTO t (v) VALUES ('a')", [])
            .expect("insert");
    }
    let found: Option<i64> = conn
        .query_row("SELECT id FROM t", [], |r| r.get(0))
        .optional()
        .expect("select");

    // Assert
    assert_eq!(found, None);
}
