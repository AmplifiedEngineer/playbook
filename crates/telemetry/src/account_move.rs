// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Reversible moves of past events between accounts.
//!
//! `assign` moves every event in a time range to another key. A dry run
//! counts what would move. An applied move records the previous label of each
//! row, so `undo` puts it back. A row already on the target is left alone, so
//! applying the same move twice changes nothing the second time.

use crate::account_store::err;
use crate::tz::Zone;
use chrono::{NaiveDate, NaiveDateTime};
use playbook_store::{
    named_params, Connection, OptionalExtension, Transaction, TransactionBehavior,
};

pub const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS account_assignments (
    id INTEGER PRIMARY KEY,
    created_at INTEGER NOT NULL,
    from_ts INTEGER NOT NULL,
    to_ts INTEGER NOT NULL,
    target TEXT NOT NULL,
    undone INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS account_assignment_rows (
    assignment_id INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('usage','tool')),
    event_id TEXT NOT NULL,
    previous TEXT NOT NULL,
    PRIMARY KEY (assignment_id, kind, event_id)
);";

/// The row kind recorded for each table that carries an account.
const TABLES: [(&str, &str); 2] = [
    ("usage", "usage_events"),
    ("tool", "tool_invocation_events"),
];

/// `YYYY-MM-DD`, `YYYY-MM-DDTHH:MM` or `YYYY-MM-DDTHH:MM:SS` on the clock of
/// `zone`, as epoch seconds.
pub fn parse_time(text: &str, zone: Zone) -> Result<i64, String> {
    let text = text.trim();
    let naive = NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M"))
        .or_else(|_| {
            NaiveDate::parse_from_str(text, "%Y-%m-%d")
                .map(|d| d.and_hms_opt(0, 0, 0).unwrap_or_default())
        })
        .map_err(|_| format!("cannot read the time {text:?}; use 2026-11-01T00:00"))?;
    let guess = naive.and_utc().timestamp();
    Ok(guess - zone.offset_at(guess - zone.offset_at(guess)))
}

/// What a move would change, or did.
#[derive(Debug, Clone, PartialEq)]
pub struct Moved {
    pub events: i64,
    pub tool_events: i64,
    pub cost_usd: f64,
}

/// What a move covers: the events whose own time is in `[from, to)`, or all
/// events of the sessions whose first event is in `[from, to)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scope {
    pub from: i64,
    pub to: i64,
    pub by_session: bool,
}

impl Scope {
    pub fn time(from: i64, to: i64) -> Scope {
        Scope {
            from,
            to,
            by_session: false,
        }
    }

    pub fn sessions(from: i64, to: i64) -> Scope {
        Scope {
            from,
            to,
            by_session: true,
        }
    }

    fn clause(self) -> &'static str {
        if self.by_session {
            "session_id IN (SELECT session_id FROM usage_events GROUP BY session_id
                HAVING MIN(timestamp) >= :from AND MIN(timestamp) < :to)"
        } else {
            "timestamp >= :from AND timestamp < :to"
        }
    }
}

/// The rows of `scope` that are not on `target`, and are on `was` when given.
fn selection(table: &str, scope: Scope) -> String {
    format!(
        "FROM {table} WHERE {} AND account <> :target AND (:was IS NULL OR account = :was)",
        scope.clause()
    )
}

/// Counts what `assign` would move. Changes nothing.
pub fn plan(
    conn: &Connection,
    scope: Scope,
    target: &str,
    was: Option<&str>,
) -> Result<Moved, String> {
    let (from, to) = (scope.from, scope.to);
    let (events, cost_usd): (i64, f64) = conn
        .query_row(
            &format!(
                "SELECT COUNT(*), COALESCE(SUM(cost_usd), 0) {}",
                selection("usage_events", scope)
            ),
            named_params! {":from": from, ":to": to, ":target": target, ":was": was},
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| err("count the events", e))?;
    let tool_events = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) {}",
                selection("tool_invocation_events", scope)
            ),
            named_params! {":from": from, ":to": to, ":target": target, ":was": was},
            |r| r.get(0),
        )
        .map_err(|e| err("count the events", e))?;
    Ok(Moved {
        events,
        tool_events,
        cost_usd,
    })
}

/// Moves the rows of `scope` to `target` and records the old labels.
/// Returns the id of the move, or `None` when nothing needed to move.
pub fn assign(
    conn: &Connection,
    scope: Scope,
    target: &str,
    was: Option<&str>,
    now: i64,
) -> Result<Option<(i64, Moved)>, String> {
    let (from, to) = (scope.from, scope.to);
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| err("begin the move", e))?;
    let moved = plan(&tx, scope, target, was)?;
    if moved.events + moved.tool_events == 0 {
        return Ok(None);
    }
    tx.execute(
        "INSERT INTO account_assignments (created_at, from_ts, to_ts, target) VALUES (?1, ?2, ?3, ?4)",
        playbook_store::params![now, from, to, target],
    )
    .map_err(|e| err("record the move", e))?;
    let id = tx.last_insert_rowid();
    for (kind, table) in TABLES {
        tx.execute(
            &format!(
                "INSERT INTO account_assignment_rows (assignment_id, kind, event_id, previous)
                 SELECT :id, :kind, event_id, account {}",
                selection(table, scope)
            ),
            named_params! {":id": id, ":kind": kind, ":from": from, ":to": to, ":target": target, ":was": was},
        )
        .map_err(|e| err("record the old labels", e))?;
        tx.execute(
            &format!(
                "UPDATE {table} SET account = :target WHERE event_id IN
                 (SELECT event_id FROM account_assignment_rows WHERE assignment_id = :id AND kind = :kind)"
            ),
            named_params! {":id": id, ":kind": kind, ":target": target},
        )
        .map_err(|e| err("move the events", e))?;
    }
    tx.commit().map_err(|e| err("commit the move", e))?;
    Ok(Some((id, moved)))
}

/// Puts the old labels back for move `id`, or the newest move not undone.
/// A row whose label changed since the move is left as it is. Returns the id
/// the rows restored and the rows skipped, or `None` when there is no move to undo.
pub fn undo(conn: &Connection, id: Option<i64>) -> Result<Option<(i64, usize, usize)>, String> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|e| err("begin the undo", e))?;
    let row: Option<(i64, String)> = tx
        .query_row(
            "SELECT id, target FROM account_assignments
             WHERE undone = 0 AND (?1 IS NULL OR id = ?1) ORDER BY id DESC LIMIT 1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| err("find the move", e))?;
    let Some((id, target)) = row else {
        return Ok(None);
    };
    let mut restored = 0;
    for (kind, table) in TABLES {
        restored += tx
            .execute(
                &format!(
                    "UPDATE {table} SET account = (SELECT previous FROM account_assignment_rows r
                        WHERE r.assignment_id = :id AND r.kind = :kind AND r.event_id = {table}.event_id)
                     WHERE account = :target AND event_id IN
                        (SELECT event_id FROM account_assignment_rows WHERE assignment_id = :id AND kind = :kind)"
                ),
                named_params! {":id": id, ":kind": kind, ":target": target},
            )
            .map_err(|e| err("restore the labels", e))?;
    }
    let recorded: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM account_assignment_rows WHERE assignment_id = ?1",
            [id],
            |r| r.get(0),
        )
        .map_err(|e| err("count the moved records", e))?;
    tx.execute(
        "UPDATE account_assignments SET undone = 1 WHERE id = ?1",
        [id],
    )
    .map_err(|e| err("mark the move undone", e))?;
    tx.commit().map_err(|e| err("commit the undo", e))?;
    Ok(Some((
        id,
        restored,
        usize::try_from(recorded)
            .unwrap_or(0)
            .saturating_sub(restored),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_store::tests::store;

    fn labels(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT account FROM usage_events ORDER BY timestamp")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn a_dry_run_counts_the_events_and_the_cost_and_changes_nothing() {
        let conn = store();
        let m = plan(&conn, Scope::time(100, 300), "personal", None).unwrap();
        assert_eq!((m.events, m.tool_events), (2, 1));
        assert!((m.cost_usd - 3.0).abs() < 1e-9);
        assert_eq!(labels(&conn)[0], "work@x.com");
    }

    #[test]
    fn apply_moves_the_half_open_range_and_the_tool_events() {
        let conn = store();
        let (_, m) = assign(&conn, Scope::time(100, 300), "personal", None, 1)
            .unwrap()
            .unwrap();
        assert_eq!(m.events, 2);
        assert_eq!(
            labels(&conn),
            ["personal", "personal", "work@x.com", "other@x.com"]
        );
        let tool: String = conn
            .query_row("SELECT account FROM tool_invocation_events", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(tool, "personal");
    }

    #[test]
    fn applying_the_same_move_twice_moves_nothing_the_second_time() {
        let conn = store();
        assert!(assign(&conn, Scope::time(100, 300), "personal", None, 1)
            .unwrap()
            .is_some());
        assert!(assign(&conn, Scope::time(100, 300), "personal", None, 2)
            .unwrap()
            .is_none());
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM account_assignments", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn the_was_filter_leaves_other_accounts_alone() {
        let conn = store();
        let m = assign(
            &conn,
            Scope::time(0, 1000),
            "personal",
            Some("other@x.com"),
            1,
        )
        .unwrap()
        .unwrap()
        .1;
        assert_eq!((m.events, m.tool_events), (1, 0));
        assert_eq!(labels(&conn)[0], "work@x.com");
    }

    #[test]
    fn undo_restores_every_previous_label_once() {
        let conn = store();
        let before = labels(&conn);
        assign(&conn, Scope::time(0, 1000), "personal", None, 1).unwrap();
        assert!(labels(&conn).iter().all(|l| l == "personal"));
        let (_, restored, skipped) = undo(&conn, None).unwrap().unwrap();
        assert_eq!((restored, skipped), (5, 0));
        assert_eq!(labels(&conn), before);
        assert!(undo(&conn, None).unwrap().is_none());
    }

    #[test]
    fn undo_skips_a_row_relabeled_after_the_move() {
        let conn = store();
        assign(&conn, Scope::time(0, 1000), "personal", None, 1).unwrap();
        conn.execute(
            "UPDATE usage_events SET account = 'later' WHERE event_id = 'a'",
            [],
        )
        .unwrap();
        let (_, restored, skipped) = undo(&conn, None).unwrap().unwrap();
        assert_eq!((restored, skipped), (4, 1));
        assert_eq!(labels(&conn)[0], "later");
        assert_eq!(labels(&conn)[1], "work@x.com");
    }

    #[test]
    fn undo_by_id_picks_that_move_and_an_overlapping_later_move_skips_its_rows() {
        let conn = store();
        let before = labels(&conn);
        let (first, _) = assign(&conn, Scope::time(0, 1000), "personal", None, 1)
            .unwrap()
            .unwrap();
        assign(&conn, Scope::time(0, 1000), "team", Some("personal"), 2).unwrap();
        let (id, restored, skipped) = undo(&conn, Some(first)).unwrap().unwrap();
        assert_eq!((id, restored, skipped), (first, 0, 5));
        assert!(labels(&conn).iter().all(|l| l == "team"));
        assert!(undo(&conn, Some(first)).unwrap().is_none());
        undo(&conn, None).unwrap().unwrap();
        assert!(labels(&conn).iter().all(|l| l == "personal"));
        assert_ne!(labels(&conn), before);
    }

    #[test]
    fn times_read_on_the_clock_of_the_zone() {
        assert_eq!(parse_time("1970-01-02", Zone::Utc).unwrap(), 86_400);
        assert_eq!(parse_time("1970-01-01T01:30", Zone::Utc).unwrap(), 5_400);
        assert_eq!(
            parse_time("1970-01-01T00:00:10", Zone::Fixed(3600)).unwrap(),
            -3_590
        );
        assert!(parse_time("yesterday", Zone::Utc).is_err());
    }
}
