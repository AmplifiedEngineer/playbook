// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Sessions as boundaries for an account move. A user who does not know when
//! the credential changed can list the sessions of a window, pick the first
//! one that belonged to the other account, and move the sessions before it.

use crate::account_store::err;
use rusqlite::{Connection, OptionalExtension};

/// One session with its first event in the window.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub id: String,
    pub first: i64,
    pub last: i64,
    pub repo: String,
    pub events: i64,
    pub cost_usd: f64,
    pub accounts: String,
}

/// The sessions whose first event is in `[from, to)`, oldest first. The repo
/// is that of the session's first event; accounts lists every label in use.
pub fn sessions(conn: &Connection, from: i64, to: i64) -> Result<Vec<SessionRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT session_id, MIN(timestamp), MAX(timestamp), COUNT(*), COALESCE(SUM(cost_usd), 0),
                    GROUP_CONCAT(DISTINCT account),
                    (SELECT repo FROM usage_events e WHERE e.session_id = u.session_id
                     ORDER BY timestamp, event_id LIMIT 1)
             FROM usage_events u GROUP BY session_id
             HAVING MIN(timestamp) >= ?1 AND MIN(timestamp) < ?2
             ORDER BY MIN(timestamp), session_id",
        )
        .map_err(|e| err("list the sessions", e))?;
    let rows = stmt
        .query_map([from, to], |r| {
            Ok(SessionRow {
                id: r.get(0)?,
                first: r.get(1)?,
                last: r.get(2)?,
                events: r.get(3)?,
                cost_usd: r.get(4)?,
                accounts: r.get(5)?,
                repo: r.get(6)?,
            })
        })
        .map_err(|e| err("list the sessions", e))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| err("list the sessions", e))
}

/// The time of the first event of session `id`.
pub fn session_start(conn: &Connection, id: &str) -> Result<i64, String> {
    conn.query_row(
        "SELECT MIN(timestamp) FROM usage_events WHERE session_id = ?1",
        [id],
        |r| r.get::<_, Option<i64>>(0),
    )
    .optional()
    .map_err(|e| err("find the session", e))?
    .flatten()
    .ok_or_else(|| format!("no session {id} in the usage store; list them with `account sessions`"))
}

/// The text of `playbook usage account sessions`.
pub fn list_text(rows: &[SessionRow]) -> String {
    if rows.is_empty() {
        return "No sessions start in that window.".to_string();
    }
    let mut out = String::new();
    for r in rows {
        let last = if crate::tz::day_of(r.first) == crate::tz::day_of(r.last) {
            crate::tz::clock(r.last)
        } else {
            crate::tz::iso(r.last)
        };
        out.push_str(&format!(
            "{}\n  {} to {last}, {}, {} events, ${:.2}, {}\n",
            r.id,
            crate::tz::iso(r.first),
            r.repo,
            r.events,
            r.cost_usd,
            r.accounts
        ));
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_db;
    use crate::util::scratch_dir;

    const DAY: i64 = 86_400;

    /// Three sessions on day 1 and day 2: a ($1, then $2 next day),
    /// b ($4 on day 2) and c ($8 on day 2).
    fn store() -> Connection {
        crate::tz::set(crate::tz::Zone::Utc);
        let conn = open_db(&scratch_dir("account-sessions").join("u.db")).unwrap();
        for (id, ts, session, repo, cost) in [
            ("1", DAY + 100, "a", "r1", 1.0),
            ("2", 2 * DAY + 100, "a", "r1", 2.0),
            ("3", 2 * DAY + 200, "b", "r2", 4.0),
            ("4", 2 * DAY + 300, "c", "r3", 8.0),
        ] {
            conn.execute(
                "INSERT INTO usage_events (event_id, timestamp, session_id, account, model, effort,
                    repo, branch, input_tokens, output_tokens, cache_creation_tokens,
                    cache_read_tokens, cost_usd) VALUES (?1, ?2, ?3, 'w', 'm', '', ?4, 'b', 1, 1, 0, 0, ?5)",
                rusqlite::params![id, ts, session, repo, cost],
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn sessions_list_with_first_last_repo_events_and_cost() {
        let rows = sessions(&store(), 0, 3 * DAY).unwrap();
        let ids: Vec<_> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        assert_eq!((rows[0].events, rows[0].repo.as_str()), (2, "r1"));
        assert_eq!((rows[0].first, rows[0].last), (DAY + 100, 2 * DAY + 100));
        assert!((rows[0].cost_usd - 3.0).abs() < 1e-9);
        assert!(sessions(&store(), 2 * DAY, 3 * DAY).unwrap().len() == 2);
        assert!(list_text(&rows).starts_with("a\n  1970-01-02"));
    }

    #[test]
    fn an_unknown_session_is_an_error_and_a_known_one_gives_its_start() {
        let conn = store();
        assert_eq!(session_start(&conn, "b").unwrap(), 2 * DAY + 200);
        assert!(session_start(&conn, "zzz")
            .unwrap_err()
            .contains("no session zzz"));
    }

    #[test]
    fn a_session_move_takes_whole_sessions_that_start_in_the_window() {
        let conn = store();
        // Sessions starting before b: only a, including its event after b's start.
        let scope = crate::account_move::Scope::sessions(0, 2 * DAY + 200);
        let m = crate::account_move::plan(&conn, scope, "p", None).unwrap();
        assert_eq!(m.events, 2);
        crate::account_move::assign(&conn, scope, "p", None, 1).unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM usage_events WHERE account = 'p'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2);
    }
}
