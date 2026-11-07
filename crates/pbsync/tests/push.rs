// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

#[path = "support/local.rs"]
mod local;
#[path = "support/sqlite_fake.rs"]
mod sqlite_fake;

use std::cell::{Cell, RefCell};
use std::net::TcpListener;

use local::{ACCOUNT, BRANCH, MACHINE, REPO, SESSION};
use pbsync::push::{
    backoff_seconds, push, PushError, PushSettings, SYNC_BATCH_ROWS, SYNC_RESEND_WINDOW_SECONDS,
};
use pbsync::remote::ensure_schema;
use pbsync::tables::usage_events;
use pbsync::token::resolve_token;
use pbsync::transport::{
    HttpTransport, Statement, StatementResult, Transport, TransportError, Value,
};
use playbook_store::Connection;
use sqlite_fake::SqliteFake;

const NOW: i64 = 1_800_000_000;
const OLD: i64 = NOW - 10 * SYNC_RESEND_WINDOW_SECONDS;
const SALT: &str = "test-salt";
const SECRET_TOKEN: &str = "tok-very-secret-123";

/// Records every statement and can fail the Nth batch (a pipeline that starts with BEGIN).
struct Probe<'a> {
    inner: &'a SqliteFake,
    seen: RefCell<Vec<Statement>>,
    fail_batch: Option<usize>,
    batches: Cell<usize>,
}

impl<'a> Probe<'a> {
    fn new(inner: &'a SqliteFake) -> Self {
        Self {
            inner,
            seen: RefCell::new(Vec::new()),
            fail_batch: None,
            batches: Cell::new(0),
        }
    }

    fn failing_on(mut self, batch: usize) -> Self {
        self.fail_batch = Some(batch);
        self
    }

    fn everything_sent(&self) -> String {
        format!("{:?}", self.seen.borrow()).to_lowercase()
    }

    fn row_writes(&self) -> usize {
        let marker = "INSERT INTO SHARED_USAGE_EVENTS";
        let writes = |s: &&Statement| s.sql.trim_start().to_uppercase().starts_with(marker);
        self.seen.borrow().iter().filter(writes).count()
    }

    fn data_writes(&self) -> usize {
        let is_write = |s: &Statement| {
            let sql = s.sql.trim_start().to_uppercase();
            !(sql.starts_with("SELECT") || sql.starts_with("CREATE"))
        };
        self.seen.borrow().iter().filter(|s| is_write(s)).count()
    }
}

impl Transport for Probe<'_> {
    fn execute(&self, statements: &[Statement]) -> Result<Vec<StatementResult>, TransportError> {
        self.seen.borrow_mut().extend(statements.iter().cloned());
        if statements.first().is_some_and(|s| s.sql == "BEGIN") {
            self.batches.set(self.batches.get() + 1);
            if Some(self.batches.get()) == self.fail_batch {
                return Err(TransportError::Statement {
                    index: 1,
                    message: "injected failure".to_string(),
                });
            }
        }
        self.inner.execute(statements)
    }
}

fn settings(accounts: &[&str]) -> PushSettings {
    PushSettings {
        machine: MACHINE.to_string(),
        allowed_accounts: accounts.iter().map(|a| (*a).to_string()).collect(),
        salt: SALT.to_string(),
    }
}

fn try_run(local: &Connection, remote: &dyn Transport, now: i64) -> Result<usize, PushError> {
    let stats = push(
        local,
        remote,
        &usage_events(),
        &settings(&[ACCOUNT]),
        &|| now,
    )?;
    Ok(stats.rows_sent)
}

/// Like `try_run`, with an error that can be compared.
fn run(local: &Connection, remote: &dyn Transport, now: i64) -> Result<usize, String> {
    try_run(local, remote, now).map_err(|e| e.to_string())
}

fn remote_count(fake: &SqliteFake) -> i64 {
    match fake.query("SELECT COUNT(*) FROM shared_usage_events")[0][0] {
        Value::Integer(n) => n,
        ref other => panic!("{other:?}"),
    }
}

fn remote_value(fake: &SqliteFake, column: &str, event_id: &str) -> Value {
    let sql = format!("SELECT {column} FROM shared_usage_events WHERE event_id = '{event_id}'");
    fake.query(&sql)[0][0].clone()
}

fn insert_many(local: &Connection, count: usize) {
    for n in 0..count {
        local::insert(local, &format!("e{n:05}"), OLD, 10);
    }
}

#[test]
fn an_account_off_the_allowlist_and_an_empty_allowlist_send_nothing() {
    for allowed in [&["other@example.com"][..], &[][..]] {
        // Arrange
        let (local, fake) = (local::open(), SqliteFake::new());
        local::insert(&local, "e1", OLD, 10);
        let probe = Probe::new(&fake);
        // Act
        let stats = push(&local, &probe, &usage_events(), &settings(allowed), &|| NOW).unwrap();
        // Assert
        assert_eq!(stats.rows_sent, 0);
        assert_eq!(probe.row_writes(), 0, "{allowed:?}");
    }
}

#[test]
fn a_second_push_writes_no_rows() {
    // Arrange
    let (local, fake) = (local::open(), SqliteFake::new());
    insert_many(&local, 3);
    assert_eq!(run(&local, &fake, NOW), Ok(3));
    // Act
    let second = run(&local, &fake, NOW);
    // Assert
    assert_eq!(second, Ok(0));
    assert_eq!(remote_count(&fake), 3);
    assert_eq!(local::watermark(&local), 3);
}

#[test]
fn a_path_repo_leaves_only_its_hash_remotely() {
    // Arrange
    let (local, fake) = (local::open(), SqliteFake::new());
    local::insert(&local, "e1", OLD, 10);
    let probe = Probe::new(&fake);
    // Act
    run(&local, &probe, NOW).unwrap();
    // Assert
    let sent = probe.everything_sent();
    assert!(!sent.contains("secret-project") && !sent.contains("/users/dev"));
    let Value::Text(repo) = remote_value(&fake, "repo", "e1") else {
        panic!("repo is not text")
    };
    assert_eq!(repo.len(), 32);
    assert!(repo.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_ne!(repo, REPO);
}

#[test]
fn no_statement_carries_the_session_agent_branch_or_raw_email() {
    // Arrange
    let (local, fake) = (local::open(), SqliteFake::new());
    local::insert(&local, "e1", OLD, 10);
    let probe = Probe::new(&fake);
    // Act
    run(&local, &probe, NOW).unwrap();
    // Assert
    let sent = probe.everything_sent();
    for banned in [
        SESSION,
        BRANCH,
        ACCOUNT,
        "session_id",
        "agent",
        "branch",
        "claude-code",
    ] {
        assert!(!sent.contains(&banned.to_lowercase()), "{banned}");
    }
}

#[test]
fn every_batch_size_pushes_complete() {
    for count in [0, 1, SYNC_BATCH_ROWS, SYNC_BATCH_ROWS + 1] {
        // Arrange
        let (local, fake) = (local::open(), SqliteFake::new());
        insert_many(&local, count);
        // Act
        let sent = run(&local, &fake, NOW);
        // Assert
        assert_eq!(sent, Ok(count));
        assert_eq!(remote_count(&fake), i64::try_from(count).unwrap());
        assert_eq!(local::watermark(&local), i64::try_from(count).unwrap());
    }
}

#[test]
fn a_larger_remote_duplicate_is_kept_and_a_smaller_one_is_replaced_only_by_growth() {
    for (remote_output, expected_output, expected_input) in [(50, 50, 7), (5, 10, 1), (10, 10, 7)] {
        // Arrange
        let (local, fake) = (local::open(), SqliteFake::new());
        local::insert(&local, "e1", OLD, 10);
        ensure_schema(&fake, &[usage_events()], NOW).unwrap();
        fake.seed(&format!(
            "INSERT INTO shared_usage_events VALUES ('{MACHINE}', 'e1', {OLD}, 'a', 'r', 'opus',
             'high', 7, {remote_output}, 7, 7, 7, 9.0, 1, 1)"
        ));
        // Act
        run(&local, &fake, NOW).unwrap();
        // Assert
        assert_eq!(
            remote_value(&fake, "output_tokens", "e1"),
            Value::Integer(expected_output)
        );
        assert_eq!(
            remote_value(&fake, "input_tokens", "e1"),
            Value::Integer(expected_input)
        );
        assert_eq!(remote_count(&fake), 1);
    }
}

#[test]
fn a_remote_failing_on_batch_two_of_three_stops_the_watermark_and_a_rerun_converges() {
    // Arrange
    let (local, fake) = (local::open(), SqliteFake::new());
    insert_many(&local, 3 * SYNC_BATCH_ROWS - 100);
    let failing = Probe::new(&fake).failing_on(2);
    // Act
    let first = run(&local, &failing, NOW);
    // Assert
    assert!(first.is_err());
    assert_eq!(local::watermark(&local), 500);
    assert_eq!(remote_count(&fake), 500);
    assert!(fake.log().iter().any(|sql| sql == "ROLLBACK"));
    // Act again
    let second = run(&local, &fake, NOW);
    // Assert
    assert_eq!(second, Ok(900));
    assert_eq!(remote_count(&fake), 1400);
    assert_eq!(local::watermark(&local), 1400);
}

#[test]
fn an_unreachable_remote_keeps_the_watermark_and_records_an_error_without_the_token() {
    // Arrange
    let (local, fake) = (local::open(), SqliteFake::new());
    local::insert(&local, "e1", OLD, 10);
    run(&local, &fake, NOW).unwrap();
    local::insert(&local, "e2", OLD, 10);
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let token = resolve_token(Some(SECRET_TOKEN), "UNUSED", &|_| None).unwrap();
    let dead = HttpTransport::new(&format!("http://127.0.0.1:{port}"), token)
        .unwrap()
        .with_sleeper(|_| {});
    // Act
    let result = run(&local, &dead, NOW);
    // Assert
    assert!(result.is_err());
    assert_eq!(local::watermark(&local), 1);
    let recorded = local::state(&local, "last_error").unwrap();
    assert!(!recorded.is_empty() && !recorded.contains(SECRET_TOKEN));
    assert_eq!(local::state(&local, "failures").as_deref(), Some("1"));
}

#[test]
fn consecutive_failures_back_off_30_60_120_240_480_900_900_and_success_resets() {
    // Arrange
    let (local, fake) = (local::open(), SqliteFake::new());
    local::insert(&local, "e1", OLD, 10);
    let failing = Probe::new(&fake).failing_on(1);
    let mut waits = Vec::new();
    // Act
    for _ in 0..7 {
        let _ = run(&local, &failing, NOW);
        let failures = local::state(&local, "failures").unwrap().parse().unwrap();
        waits.push(backoff_seconds(failures));
        failing.batches.set(0);
    }
    // Assert
    assert_eq!(waits, [30, 60, 120, 240, 480, 900, 900]);
    assert_eq!(backoff_seconds(0), 0);
    run(&local, &fake, NOW).unwrap();
    assert_eq!(local::state(&local, "failures").as_deref(), Some("0"));
    assert_eq!(
        local::state(&local, "last_success").as_deref(),
        Some(NOW.to_string().as_str())
    );
}

#[test]
fn a_row_with_an_empty_machine_is_not_sent() {
    // Arrange
    let (local, fake) = (local::open(), SqliteFake::new());
    local::insert_for(&local, "e1", OLD, 10, ACCOUNT, "");
    // Act
    let sent = run(&local, &fake, NOW);
    // Assert
    assert_eq!(sent, Ok(0));
    assert_eq!(remote_count(&fake), 0);
}

#[test]
fn a_row_that_grows_inside_the_resend_window_is_resent_and_outside_it_is_not() {
    for (age, resent) in [
        (SYNC_RESEND_WINDOW_SECONDS, true),
        (SYNC_RESEND_WINDOW_SECONDS + 1, false),
    ] {
        // Arrange
        let (local, fake) = (local::open(), SqliteFake::new());
        local::insert(&local, "e1", NOW - age, 10);
        run(&local, &fake, NOW).unwrap();
        local
            .execute("UPDATE usage_events SET output_tokens = 20", [])
            .unwrap();
        // Act
        let sent = run(&local, &fake, NOW);
        // Assert
        let expected = if resent { 20 } else { 10 };
        assert_eq!(sent, Ok(usize::from(resent)), "age {age}");
        assert_eq!(
            remote_value(&fake, "output_tokens", "e1"),
            Value::Integer(expected)
        );
        assert_eq!(local::watermark(&local), 1, "the window never lowers it");
    }
}

#[test]
fn pushed_at_comes_from_the_clock_and_a_grown_row_gets_a_new_one() {
    // Arrange
    let (local, fake) = (local::open(), SqliteFake::new());
    local::insert(&local, "e1", NOW, 10);
    run(&local, &fake, NOW).unwrap();
    let first = remote_value(&fake, "pushed_at", "e1");
    local
        .execute("UPDATE usage_events SET output_tokens = 20", [])
        .unwrap();
    // Act
    run(&local, &fake, NOW + 60).unwrap();
    // Assert
    assert_eq!(first, Value::Integer(NOW));
    assert_eq!(
        remote_value(&fake, "pushed_at", "e1"),
        Value::Integer(NOW + 60)
    );
}

#[test]
fn a_local_column_missing_from_the_allowlist_stays_local() {
    // Arrange
    let (local, fake) = (local::open(), SqliteFake::new());
    local
        .execute("ALTER TABLE usage_events ADD COLUMN private_note TEXT", [])
        .unwrap();
    local::insert(&local, "e1", OLD, 10);
    local
        .execute("UPDATE usage_events SET private_note = 'hush-hush'", [])
        .unwrap();
    let probe = Probe::new(&fake);
    // Act
    run(&local, &probe, NOW).unwrap();
    // Assert
    let columns = fake.query("SELECT name FROM pragma_table_info('shared_usage_events')");
    assert!(!columns.contains(&vec![Value::Text("private_note".to_string())]));
    assert!(!probe.everything_sent().contains("hush-hush"));
}

#[test]
fn a_usage_db_newer_or_older_than_sync_understands_stops_with_no_remote_requests() {
    for (version, expected) in [(6, "update pbsync"), (4, "run any playbook command")] {
        // Arrange
        let (local, fake) = (local::open(), SqliteFake::new());
        local::insert(&local, "e1", OLD, 10);
        local::stamp(&local, version);
        let probe = Probe::new(&fake);
        // Act
        let result = push(
            &local,
            &probe,
            &usage_events(),
            &settings(&[ACCOUNT]),
            &|| NOW,
        );
        // Assert
        let error = result.unwrap_err();
        assert!(matches!(error, PushError::LocalSchema(_)));
        assert!(error.to_string().contains(expected), "{error}");
        assert!(probe.seen.borrow().is_empty());
    }
}

#[test]
fn a_newer_remote_schema_stops_with_no_data_writes() {
    // Arrange
    let (local, fake) = (local::open(), SqliteFake::new());
    local::insert(&local, "e1", OLD, 10);
    fake.seed(
        "CREATE TABLE shared_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO shared_meta VALUES ('schema_version', '99');",
    );
    let probe = Probe::new(&fake);
    // Act
    let result = try_run(&local, &probe, NOW);
    // Assert
    assert!(matches!(result, Err(PushError::Remote(_))));
    assert_eq!(probe.data_writes(), 0);
    assert_eq!(local::watermark(&local), 0);
}

#[test]
fn a_database_without_the_sync_tables_is_refused_by_name() {
    // Arrange
    let local = Connection::open_in_memory().unwrap();
    let fake = SqliteFake::new();
    // Act
    let result = try_run(&local, &fake, NOW);
    // Assert
    let error = result.unwrap_err();
    assert!(matches!(error, PushError::LocalSchema(_)));
    assert!(error.to_string().contains("any playbook command"));
}
