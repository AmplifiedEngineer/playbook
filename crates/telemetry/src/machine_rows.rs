// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `machine` column of the six usage and telemetry tables: its migration,
//! the id new rows carry, and the one-time backfill of rows written before it.
//!
//! The local machine id is mirrored into the `meta` table when the usage
//! database opens, so every insert stamps it in SQL without carrying the id
//! through each call.

use playbook_store::{Connection, OptionalExtension};

/// The tables whose rows each belong to one machine.
pub const MACHINE_TABLES: [&str; 6] = [
    "usage_events",
    "tool_invocation_events",
    "telemetry_requests",
    "telemetry_tools",
    "telemetry_errors",
    "telemetry_metrics",
];

/// Rows stamped per write, so one backfill never holds a long write lock
/// against hooks and the statusline.
pub const BACKFILL_BATCH_ROWS: usize = 5000;

/// SQL for the id of this machine as an insert value; empty when none is recorded.
pub const LOCAL_MACHINE_SQL: &str =
    "COALESCE((SELECT value FROM meta WHERE key = 'machine_id'), '')";

/// Opens a usage database for this machine: reads or makes the machine id next
/// to the database, records it for new rows and stamps older rows. An id that
/// cannot be read leaves rows unstamped until `playbook usage machine` shows
/// why, and the next open stamps them.
pub fn adopt_local_machine(conn: &Connection, data_dir: &std::path::Path) -> Result<(), String> {
    let hostname = crate::machine::current_hostname();
    let Ok(machine) = crate::machine::load_or_create(data_dir, uuid::Uuid::new_v4, &hostname)
    else {
        return Ok(());
    };
    record_local_machine(conn, &machine.id)?;
    backfill_machine(conn, &machine.id, &mut |_| true).map(drop)
}

const MACHINE_ID_KEY: &str = "machine_id";
const BACKFILL_DONE_KEY: &str = "machine_backfill_done";

/// Adds `machine` to each of the six tables that exists and lacks it. Old rows
/// keep the empty default until the backfill stamps them, so a new column
/// also re-arms the backfill.
pub fn ensure_machine_column(conn: &Connection) -> Result<(), String> {
    create_meta(conn)?;
    for table in MACHINE_TABLES {
        let columns = column_count(conn, table)?;
        if columns == 0 || has_machine_column(conn, table)? {
            continue;
        }
        match conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN machine TEXT NOT NULL DEFAULT ''"),
            [],
        ) {
            Ok(_) => {}
            // A concurrent opener won the race.
            Err(e) if e.to_string().contains("duplicate column") => {}
            Err(e) => return Err(format!("failed to add machine to {table}: {e}")),
        }
        delete_meta(conn, BACKFILL_DONE_KEY)?;
    }
    Ok(())
}

/// Records `machine_id` as the id new rows carry.
pub fn record_local_machine(conn: &Connection, machine_id: &str) -> Result<(), String> {
    create_meta(conn)?;
    if read_meta(conn, MACHINE_ID_KEY)?.as_deref() == Some(machine_id) {
        return Ok(());
    }
    write_meta(conn, MACHINE_ID_KEY, machine_id)
}

/// Stamps `machine_id` on every row whose machine is empty, `BACKFILL_BATCH_ROWS`
/// at a time. `between_batches(batch)` runs after each committed batch and
/// returns false to stop; a stopped or killed run resumes where it ended.
/// Returns the rows written, which is 0 once the backfill has completed.
pub fn backfill_machine(
    conn: &Connection,
    machine_id: &str,
    between_batches: &mut dyn FnMut(usize) -> bool,
) -> Result<usize, String> {
    if read_meta(conn, BACKFILL_DONE_KEY)?.as_deref() == Some(machine_id) {
        return Ok(0);
    }
    let mut written = 0;
    let mut batches = 0;
    for table in MACHINE_TABLES {
        if !has_machine_column(conn, table)? {
            continue;
        }
        loop {
            let stamped = stamp_batch(conn, table, machine_id)?;
            if stamped == 0 {
                break;
            }
            written += stamped;
            batches += 1;
            if !between_batches(batches) {
                return Ok(written);
            }
        }
    }
    write_meta(conn, BACKFILL_DONE_KEY, machine_id)?;
    Ok(written)
}

fn stamp_batch(conn: &Connection, table: &str, machine_id: &str) -> Result<usize, String> {
    conn.execute(
        &format!(
            "UPDATE {table} SET machine = ?1 WHERE rowid IN
             (SELECT rowid FROM {table} WHERE machine = '' LIMIT {BACKFILL_BATCH_ROWS})"
        ),
        [machine_id],
    )
    .map_err(|e| format!("failed to stamp the machine on {table}: {e}"))
}

fn column_count(conn: &Connection, table: &str) -> Result<i64, String> {
    conn.query_row("SELECT COUNT(*) FROM pragma_table_info(?1)", [table], |r| {
        r.get(0)
    })
    .map_err(|e| format!("failed to inspect {table}: {e}"))
}

fn has_machine_column(conn: &Connection, table: &str) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name = 'machine')",
        [table],
        |r| r.get(0),
    )
    .map_err(|e| format!("failed to inspect {table}: {e}"))
}

fn create_meta(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID",
    )
    .map_err(|e| format!("failed to create the meta table: {e}"))
}

fn read_meta(conn: &Connection, key: &str) -> Result<Option<String>, String> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()
        .map_err(|e| format!("failed to read {key}: {e}"))
}

fn write_meta(conn: &Connection, key: &str, value: &str) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        [key, value],
    )
    .map(drop)
    .map_err(|e| format!("failed to write {key}: {e}"))
}

fn delete_meta(conn: &Connection, key: &str) -> Result<(), String> {
    conn.execute("DELETE FROM meta WHERE key = ?1", [key])
        .map(drop)
        .map_err(|e| format!("failed to clear {key}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry_db::{
        insert_error, insert_metric, insert_request, insert_tool, TelemetryError, TelemetryMetric,
        TelemetryRequest, TelemetryTool,
    };
    use crate::util::scratch_dir;
    use crate::{db, ToolInvocationEvent, ToolKind, UsageEvent};

    const LOCAL: &str = "11111111-1111-4111-8111-111111111111";

    fn open(tag: &str) -> Connection {
        let conn = db::open_db(&scratch_dir(tag).join("usage.db")).unwrap();
        record_local_machine(&conn, LOCAL).unwrap();
        conn
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn has_machine(conn: &Connection, table: &str) -> bool {
        count(
            conn,
            &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = 'machine'"),
        ) == 1
    }

    fn usage(id: &str, output_tokens: u64) -> UsageEvent {
        UsageEvent {
            event_id: id.into(),
            timestamp: 10,
            session_id: "s".into(),
            account: "a".into(),
            model: "m".into(),
            agent: "claude-code".into(),
            output_tokens,
            ..UsageEvent::default()
        }
    }

    /// Fills `usage_events` with `rows` rows whose machine is empty.
    fn fill_unstamped(conn: &Connection, rows: usize) {
        conn.execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 WHERE ?1 > 0 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
             INSERT INTO usage_events (event_id, timestamp, session_id, account, model, effort,
                repo, branch, input_tokens, output_tokens, cache_creation_tokens,
                cache_read_tokens, cost_usd, machine)
             SELECT 'e' || i, i, 's', 'a', 'm', '', 'r', 'b', 0, 0, 0, 0, 0, '' FROM n",
            [rows as i64],
        )
        .unwrap();
    }

    fn backfill_counting(conn: &Connection) -> (usize, usize) {
        let mut batches = 0;
        let written = backfill_machine(conn, LOCAL, &mut |_| {
            batches += 1;
            true
        })
        .unwrap();
        (written, batches)
    }

    #[test]
    fn an_old_database_gains_the_column_and_its_rows_get_the_local_id() {
        // Arrange
        let path = scratch_dir("machine-old").join("usage.db");
        let conn = db::open_db(&path).unwrap();
        for table in MACHINE_TABLES {
            conn.execute(&format!("ALTER TABLE {table} DROP COLUMN machine"), [])
                .unwrap();
        }
        conn.execute(
            "UPDATE meta SET value = '1' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tool_invocation_events VALUES ('t', 1, 's', 'a', 'skill', 'n')",
            [],
        )
        .unwrap();
        drop(conn);

        // Act
        let conn = db::open_db(&path).unwrap();

        // Assert
        assert!(MACHINE_TABLES.iter().all(|t| has_machine(&conn, t)));
        let machine: String = conn
            .query_row("SELECT machine FROM tool_invocation_events", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(!machine.is_empty());
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM tool_invocation_events WHERE machine = ''"
            ),
            0
        );
    }

    #[test]
    fn a_reingest_adds_no_duplicates_and_the_larger_output_wins() {
        // Arrange
        let conn = open("machine-upsert");

        // Act
        db::upsert_usage_event(&conn, &usage("e", 5)).unwrap();
        db::upsert_usage_event(&conn, &usage("e", 9)).unwrap();
        let smaller = db::upsert_usage_event(&conn, &usage("e", 3)).unwrap();

        // Assert
        assert_eq!(smaller, 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM usage_events"), 1);
        assert_eq!(count(&conn, "SELECT output_tokens FROM usage_events"), 9);
        assert_eq!(
            count(
                &conn,
                &format!("SELECT COUNT(*) FROM usage_events WHERE machine = '{LOCAL}'")
            ),
            1
        );
    }

    #[test]
    fn every_table_stamps_the_local_id_on_a_new_row() {
        // Arrange
        let conn = open("machine-stamp");

        // Act
        db::upsert_usage_event(&conn, &usage("e", 1)).unwrap();
        db::insert_tool_event(
            &conn,
            &ToolInvocationEvent {
                event_id: "t".into(),
                timestamp: 1,
                session_id: "s".into(),
                account: "a".into(),
                kind: ToolKind::Skill,
                name: "n".into(),
            },
        )
        .unwrap();
        insert_request(&conn, &TelemetryRequest::default()).unwrap();
        insert_tool(&conn, &TelemetryTool::default()).unwrap();
        insert_error(&conn, &TelemetryError::default()).unwrap();
        insert_metric(&conn, &TelemetryMetric::default()).unwrap();

        // Assert
        for table in MACHINE_TABLES {
            let found = count(
                &conn,
                &format!("SELECT COUNT(*) FROM {table} WHERE machine = '{LOCAL}'"),
            );
            assert_eq!(found, 1, "{table}");
        }
    }

    #[test]
    fn running_the_backfill_twice_writes_nothing_the_second_time() {
        // Arrange
        let conn = open("machine-twice");
        fill_unstamped(&conn, 3);

        // Act
        let (first, _) = backfill_counting(&conn);
        let (second, _) = backfill_counting(&conn);

        // Assert
        assert_eq!((first, second), (3, 0));
        assert_eq!(
            count(
                &conn,
                &format!("SELECT COUNT(*) FROM usage_events WHERE machine = '{LOCAL}'")
            ),
            3
        );
    }

    #[test]
    fn the_backfill_runs_in_batches_of_the_named_size() {
        for (rows, batches) in [
            (0, 0),
            (1, 1),
            (BACKFILL_BATCH_ROWS, 1),
            (BACKFILL_BATCH_ROWS + 1, 2),
        ] {
            // Arrange
            let conn = open("machine-batches");
            fill_unstamped(&conn, rows);

            // Act
            let (written, ran) = backfill_counting(&conn);

            // Assert
            assert_eq!((written, ran), (rows, batches), "{rows} rows");
        }
    }

    #[test]
    fn a_row_written_between_batches_commits_and_keeps_its_own_machine() {
        // Arrange
        let conn = open("machine-between");
        fill_unstamped(&conn, BACKFILL_BATCH_ROWS + 1);

        // Act
        let mut seen = None;
        backfill_machine(&conn, LOCAL, &mut |batch| {
            if batch == 1 {
                db::upsert_usage_event(&conn, &usage("during", 1)).unwrap();
                seen = Some(count(
                    &conn,
                    "SELECT COUNT(*) FROM usage_events WHERE event_id = 'during'",
                ));
            }
            true
        })
        .unwrap();

        // Assert
        assert_eq!(seen, Some(1));
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM usage_events WHERE machine = ''"
            ),
            0
        );
    }

    #[test]
    fn a_stopped_backfill_resumes_without_stamping_a_row_twice() {
        // Arrange
        let conn = open("machine-resume");
        fill_unstamped(&conn, BACKFILL_BATCH_ROWS + 1);
        let first = backfill_machine(&conn, LOCAL, &mut |_| false).unwrap();

        // Act
        let (rest, _) = backfill_counting(&conn);

        // Assert
        assert_eq!((first, rest), (BACKFILL_BATCH_ROWS, 1));
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM usage_events WHERE machine = ''"
            ),
            0
        );
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM usage_events"),
            (BACKFILL_BATCH_ROWS + 1) as i64
        );
    }

    #[test]
    #[ignore = "nightly: 200,000 rows"]
    fn a_large_table_backfills_in_whole_batches() {
        // Arrange
        let conn = open("machine-large");
        fill_unstamped(&conn, 200_000);

        // Act
        let (written, batches) = backfill_counting(&conn);

        // Assert
        assert_eq!((written, batches), (200_000, 200_000 / BACKFILL_BATCH_ROWS));
    }
}
