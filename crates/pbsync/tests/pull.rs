// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

#[path = "support/local.rs"]
mod local;
#[path = "support/sqlite_fake.rs"]
mod sqlite_fake;

use std::cell::Cell;

use pbsync::privacy::{salted_hash, Domain};
use pbsync::pull::{pull, pull_with, PullError, PullSettings, PullStats};
use pbsync::push::{push, PushSettings, SYNC_BATCH_ROWS, SYNC_RESEND_WINDOW_SECONDS};
use pbsync::remote::RemoteError;
use pbsync::tables::usage_events;
use pbsync::transport::{Statement, StatementResult, Transport, TransportError};
use playbook_store::Connection;
use sqlite_fake::SqliteFake;

const NOW: i64 = 1_800_000_000;
const SALT: &str = "test-salt";
const MACHINE_A: &str = "machine-a";
const MACHINE_B: &str = "machine-b";
const OTHER_ACCOUNT: &str = "other@example.com";
const PAGE_MARKER: &str = "ORDER BY pushed_at";

/// Fails the Nth page query, passing everything else to the fake.
struct FailingPage<'a> {
    inner: &'a SqliteFake,
    fail_page: usize,
    pages: Cell<usize>,
}

impl Transport for FailingPage<'_> {
    fn execute(&self, statements: &[Statement]) -> Result<Vec<StatementResult>, TransportError> {
        if statements.iter().any(|s| s.sql.contains(PAGE_MARKER)) {
            self.pages.set(self.pages.get() + 1);
            if self.pages.get() == self.fail_page {
                return Err(TransportError::Network {
                    reason: "injected".to_string(),
                });
            }
        }
        self.inner.execute(statements)
    }
}

fn push_as(conn: &Connection, remote: &SqliteFake, machine: &str, accounts: &[&str]) {
    let settings = PushSettings {
        machine: machine.to_string(),
        allowed_accounts: accounts.iter().map(|a| a.to_string()).collect(),
        salt: SALT.to_string(),
    };
    push(conn, remote, &usage_events(), &settings, &|| NOW).unwrap();
}

fn pull_as(
    conn: &Connection,
    remote: &dyn Transport,
    machine: &str,
) -> Result<PullStats, PullError> {
    let settings = PullSettings {
        machine: machine.to_string(),
        salt: SALT.to_string(),
    };
    pull(conn, remote, &usage_events(), &settings, &|| NOW)
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn remote_rows(conn: &Connection) -> i64 {
    count(conn, "SELECT COUNT(*) FROM usage_events_remote")
}

/// A machine with `rows` events pushed to the shared remote.
fn pushed(rows: usize, machine: &str, remote: &SqliteFake) -> Connection {
    let conn = local::open();
    for n in 0..rows {
        local::insert_for(
            &conn,
            &format!("{machine}-{n:04}"),
            NOW,
            10,
            local::ACCOUNT,
            machine,
        );
    }
    push_as(&conn, remote, machine, &[local::ACCOUNT]);
    conn
}

#[test]
fn two_machines_converge_on_the_same_totals_and_a_second_round_writes_nothing() {
    // Arrange
    let remote = SqliteFake::new();
    let a = pushed(3, MACHINE_A, &remote);
    let b = pushed(2, MACHINE_B, &remote);
    // Act
    let first_a = pull_as(&a, &remote, MACHINE_A).unwrap();
    let first_b = pull_as(&b, &remote, MACHINE_B).unwrap();
    let second_a = pull_as(&a, &remote, MACHINE_A).unwrap();
    let second_b = pull_as(&b, &remote, MACHINE_B).unwrap();
    // Assert
    let total = |c: &Connection| {
        count(c, "SELECT SUM(output_tokens) FROM usage_events")
            + count(
                c,
                "SELECT SUM(r.output_tokens) FROM usage_events_remote r
                 JOIN account_aliases a ON a.hash = r.account WHERE a.key = 'dev@example.com'",
            )
    };
    assert_eq!(total(&a), 50);
    assert_eq!(total(&b), 50);
    assert_eq!((first_a.rows_written, first_b.rows_written), (2, 3));
    assert_eq!((second_a.rows_written, second_b.rows_written), (0, 0));
}

#[test]
fn a_clash_on_the_same_event_id_is_counted_without_duplicates() {
    // Arrange
    let remote = SqliteFake::new();
    let _a = pushed(1, MACHINE_A, &remote);
    let b = local::open();
    local::insert_for(&b, "machine-a-0000", NOW, 10, local::ACCOUNT, MACHINE_B);
    // Act
    let stats = pull_as(&b, &remote, MACHINE_B).unwrap();
    // Assert
    assert_eq!(stats.overlaps, 1);
    assert_eq!(remote_rows(&b), 1);
    assert_eq!(count(&b, "SELECT COUNT(*) FROM usage_events"), 1);
}

#[test]
fn self_echo_is_skipped_and_the_same_event_id_from_two_machines_keeps_two_rows() {
    // Arrange
    let remote = SqliteFake::new();
    let a = pushed(1, MACHINE_A, &remote);
    let b = local::open();
    local::insert_for(&b, "machine-a-0000", NOW, 10, local::ACCOUNT, MACHINE_B);
    push_as(&b, &remote, MACHINE_B, &[local::ACCOUNT]);
    let c = local::open();
    // Act
    pull_as(&a, &remote, MACHINE_A).unwrap();
    pull_as(&c, &remote, "machine-c").unwrap();
    // Assert
    assert_eq!(
        count(
            &a,
            "SELECT COUNT(*) FROM usage_events_remote WHERE machine = 'machine-a'"
        ),
        0
    );
    assert_eq!(remote_rows(&c), 2);
}

#[test]
fn a_pull_leaves_the_local_usage_events_untouched() {
    // Arrange
    let remote = SqliteFake::new();
    let _a = pushed(4, MACHINE_A, &remote);
    let b = pushed(2, MACHINE_B, &remote);
    // Act
    pull_as(&b, &remote, MACHINE_B).unwrap();
    // Assert
    assert_eq!(count(&b, "SELECT COUNT(*) FROM usage_events"), 2);
}

#[test]
fn a_known_account_resolves_and_an_unknown_hash_stays_unresolved() {
    // Arrange
    let remote = SqliteFake::new();
    let a = local::open();
    local::insert_for(&a, "x-1", NOW, 5, OTHER_ACCOUNT, MACHINE_A);
    local::insert_for(&a, "x-2", NOW, 5, local::ACCOUNT, MACHINE_A);
    push_as(&a, &remote, MACHINE_A, &[OTHER_ACCOUNT, local::ACCOUNT]);
    let b = local::open();
    local::insert_for(&b, "y-1", NOW, 5, local::ACCOUNT, MACHINE_B);
    // Act
    pull_as(&b, &remote, MACHINE_B).unwrap();
    // Assert
    let resolved = |hash: String| -> Option<String> {
        b.query_row(
            "SELECT key FROM account_aliases WHERE hash = ?1",
            [hash],
            |r| r.get(0),
        )
        .ok()
    };
    assert_eq!(
        resolved(salted_hash(SALT, Domain::Account, local::ACCOUNT)).as_deref(),
        Some(local::ACCOUNT)
    );
    assert_eq!(
        resolved(salted_hash(SALT, Domain::Account, OTHER_ACCOUNT)),
        None
    );
}

#[test]
fn an_account_only_in_account_names_gets_an_alias() {
    // Arrange
    let remote = SqliteFake::new();
    let b = local::open();
    local::add_account_names(&b, OTHER_ACCOUNT, "Other");
    // Act
    pull_as(&b, &remote, MACHINE_B).unwrap();
    // Assert
    let key: String = b
        .query_row(
            "SELECT key FROM account_aliases WHERE hash = ?1",
            [salted_hash(SALT, Domain::Account, OTHER_ACCOUNT)],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(key, OTHER_ACCOUNT);
}

#[test]
fn a_remote_failure_on_page_two_keeps_the_page_one_cursor_and_a_rerun_converges() {
    // Arrange
    let remote = SqliteFake::new();
    let _a = pushed(SYNC_BATCH_ROWS + 100, MACHINE_A, &remote);
    let b = local::open();
    let failing = FailingPage {
        inner: &remote,
        fail_page: 2,
        pages: Cell::new(0),
    };
    // Act
    let failed = pull_as(&b, &failing, MACHINE_B);
    let after_failure = (
        remote_rows(&b),
        b.query_row("SELECT key FROM sync_cursors", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
    );
    pull_as(&b, &remote, MACHINE_B).unwrap();
    // Assert
    assert!(matches!(failed, Err(PullError::Transport(_))));
    assert_eq!(after_failure.0, 500);
    assert_eq!(after_failure.1, "machine-a-0499");
    assert_eq!(remote_rows(&b), 600);
    assert_eq!(
        count(
            &b,
            "SELECT COUNT(DISTINCT event_id) FROM usage_events_remote"
        ),
        600
    );
    assert_eq!(local::state(&b, "failures").as_deref(), Some("0"));
}

#[test]
fn a_failure_is_recorded_without_any_token() {
    // Arrange
    let remote = SqliteFake::new();
    let _a = pushed(1, MACHINE_A, &remote);
    let b = local::open();
    let failing = FailingPage {
        inner: &remote,
        fail_page: 1,
        pages: Cell::new(0),
    };
    // Act
    let _ = pull_as(&b, &failing, MACHINE_B);
    // Assert
    assert_eq!(local::state(&b, "failures").as_deref(), Some("1"));
    assert!(local::state(&b, "last_error").unwrap().contains("injected"));
    assert_eq!(local::state(&b, "last_success"), None);
}

#[test]
fn page_sizes_around_the_batch_limit_all_arrive() {
    for rows in [0, 1, SYNC_BATCH_ROWS, SYNC_BATCH_ROWS + 1] {
        // Arrange
        let remote = SqliteFake::new();
        let _a = pushed(rows, MACHINE_A, &remote);
        let b = local::open();
        // Act
        let stats = pull_as(&b, &remote, MACHINE_B).unwrap();
        // Assert
        assert_eq!(
            remote_rows(&b),
            rows as i64,
            "{rows} rows, all one pushed_at"
        );
        assert_eq!(stats.rows_written, rows);
    }
}

#[test]
fn a_smaller_duplicate_changes_nothing_and_a_larger_one_updates_tokens_and_cost() {
    // Arrange
    let remote = SqliteFake::new();
    let _a = pushed(1, MACHINE_A, &remote);
    let b = local::open();
    pull_as(&b, &remote, MACHINE_B).unwrap();
    let read = || -> (i64, f64) {
        b.query_row(
            "SELECT output_tokens, cost_usd FROM usage_events_remote",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    // Act
    remote.seed("UPDATE shared_usage_events SET output_tokens = 1, cost_usd = 0.01");
    pull_as(&b, &remote, MACHINE_B).unwrap();
    let after_smaller = read();
    remote.seed("UPDATE shared_usage_events SET output_tokens = 99, cost_usd = 9.5");
    pull_as(&b, &remote, MACHINE_B).unwrap();
    // Assert
    assert_eq!(after_smaller, (10, 0.5));
    assert_eq!(read(), (99, 9.5));
}

#[test]
fn a_row_that_grows_below_the_cursor_is_found_through_the_resend_window() {
    // Arrange
    let remote = SqliteFake::new();
    let _a = pushed(1, MACHINE_A, &remote);
    let b = local::open();
    pull_as(&b, &remote, MACHINE_B).unwrap();
    let older = NOW - SYNC_RESEND_WINDOW_SECONDS / 2;
    remote.seed(&format!(
        "UPDATE shared_usage_events SET output_tokens = 77, pushed_at = {older}"
    ));
    // Act
    let stats = pull_as(&b, &remote, MACHINE_B).unwrap();
    // Assert
    assert_eq!(stats.rows_written, 1);
    assert_eq!(
        count(&b, "SELECT output_tokens FROM usage_events_remote"),
        77
    );
}

#[test]
fn a_local_writer_between_pages_commits_without_error() {
    // Arrange
    let remote = SqliteFake::new();
    let _a = pushed(SYNC_BATCH_ROWS + 1, MACHINE_A, &remote);
    let path = std::env::temp_dir().join(format!("pbsync-pull-{}.db", std::process::id()));
    let b = local::open_at(&path);
    let writer = Connection::open(&path).unwrap();
    let settings = PullSettings {
        machine: MACHINE_B.to_string(),
        salt: SALT.to_string(),
    };
    let mut writes = 0;
    // Act
    pull_with(
        &b,
        &remote,
        &usage_events(),
        &settings,
        &|| NOW,
        &mut || {
            writes += 1;
            local::insert_for(
                &writer,
                &format!("live-{writes}"),
                NOW,
                1,
                local::ACCOUNT,
                MACHINE_B,
            );
        },
    )
    .unwrap();
    // Assert
    assert_eq!(count(&b, "SELECT COUNT(*) FROM usage_events"), writes);
    assert_eq!(remote_rows(&b), (SYNC_BATCH_ROWS + 1) as i64);
    drop((b, writer));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn pulled_rows_carry_no_session_or_agent() {
    // Arrange
    let remote = SqliteFake::new();
    let _a = pushed(1, MACHINE_A, &remote);
    let b = local::open();
    // Act
    pull_as(&b, &remote, MACHINE_B).unwrap();
    // Assert
    let dump: String = b
        .query_row(
            "SELECT machine || repo || branch || repo_hash || account FROM usage_events_remote",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let columns: Vec<String> = b
        .prepare("SELECT name FROM pragma_table_info('usage_events_remote')")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(!columns.iter().any(|c| c == "session_id" || c == "agent"));
    assert!(
        !dump.contains(local::SESSION)
            && !dump.contains(local::REPO)
            && !dump.contains(local::BRANCH)
    );
    assert!(!remote
        .log()
        .iter()
        .any(|s| s.contains("session_id") && s.contains("SELECT")));
}

#[test]
fn a_newer_remote_schema_stops_with_zero_local_writes() {
    // Arrange
    let remote = SqliteFake::new();
    remote.seed(
        "CREATE TABLE shared_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO shared_meta VALUES ('schema_version', '99');",
    );
    let b = local::open();
    // Act
    let result = pull_as(&b, &remote, MACHINE_B);
    // Assert
    assert!(matches!(
        result,
        Err(PullError::Remote(RemoteError::RemoteNewer {
            remote: 99,
            ..
        }))
    ));
    assert_eq!(remote_rows(&b), 0);
    assert_eq!(count(&b, "SELECT COUNT(*) FROM sync_cursors"), 0);
    assert_eq!(count(&b, "SELECT COUNT(*) FROM account_aliases"), 0);
}

#[test]
fn a_local_database_without_the_contract_tables_is_refused_by_name() {
    // Arrange
    let remote = SqliteFake::new();
    let bare = Connection::open_in_memory().unwrap();
    bare.execute_batch(
        "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO meta VALUES ('schema_version', '5');",
    )
    .unwrap();
    // Act
    let result = pull_as(&bare, &remote, MACHINE_B);
    // Assert
    assert!(matches!(
        result,
        Err(PullError::LocalNotReady {
            table: "sync_state"
        })
    ));
}

#[test]
fn a_usage_db_newer_or_older_than_sync_understands_stops_before_any_request() {
    for (version, expected) in [(6, "update pbsync"), (4, "run any playbook command")] {
        // Arrange
        let remote = SqliteFake::new();
        let b = local::open();
        local::stamp(&b, version);
        // Act
        let result = pull_as(&b, &remote, MACHINE_B);
        // Assert
        let error = result.unwrap_err();
        assert!(matches!(error, PullError::LocalSchema(_)));
        assert!(error.to_string().contains(expected), "{error}");
        assert!(remote.log().is_empty());
    }
}
