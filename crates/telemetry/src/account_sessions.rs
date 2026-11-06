// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Sessions as boundaries for an account move. A user who does not know when
//! the credential changed can list the sessions of a window, pick the first
//! one that belonged to the other account, and compare the cost on each side
//! of every candidate boundary with the daily totals of an account dashboard.

use crate::account_store::err;
use rusqlite::{Connection, OptionalExtension};
use std::collections::BTreeMap;

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

/// The time of the first event of session `id`. A boundary is a second, so a
/// session that shares its first second with another session is refused: the
/// list order between the two would not decide which side of the boundary
/// each falls on.
pub fn session_start(conn: &Connection, id: &str) -> Result<i64, String> {
    let start = conn
        .query_row(
            "SELECT MIN(timestamp) FROM usage_events WHERE session_id = ?1",
            [id],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()
        .map_err(|e| err("find the session", e))?
        .flatten()
        .ok_or_else(|| {
            format!("no session {id} in the usage store; list them with `account sessions`")
        })?;
    let other: Option<String> = conn
        .query_row(
            "SELECT session_id FROM usage_events WHERE session_id <> ?1
             GROUP BY session_id HAVING MIN(timestamp) = ?2 LIMIT 1",
            rusqlite::params![id, start],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| err("find the session", e))?;
    match other {
        Some(other) => Err(format!(
            "session {id} starts in the same second as session {other}; give --from and --to times instead"
        )),
        None => Ok(start),
    }
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

/// Parses `YYYY-MM-DD=amount`.
pub fn parse_dashboard(text: &str) -> Result<(i64, f64), String> {
    let bad = || format!("cannot read {text:?}; use 2026-11-02=45.10");
    let (day, amount) = text.split_once('=').ok_or_else(bad)?;
    let start = crate::account_move::parse_time(day, crate::tz::current())?;
    let amount: f64 = amount.trim().parse().map_err(|_| bad())?;
    if !amount.is_finite() || amount < 0.0 {
        return Err(bad());
    }
    Ok((crate::tz::day_of(start), amount))
}

/// A session: its first time and cost per day.
struct Costed {
    id: String,
    first: i64,
    days: BTreeMap<i64, f64>,
}

fn costed(conn: &Connection) -> Result<Vec<Costed>, String> {
    let mut stmt = conn
        .prepare("SELECT session_id, timestamp, cost_usd FROM usage_events")
        .map_err(|e| err("read the costs", e))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, f64>(2)?,
            ))
        })
        .map_err(|e| err("read the costs", e))?;
    let mut by_id: BTreeMap<String, Costed> = BTreeMap::new();
    for row in rows {
        let (id, ts, cost) = row.map_err(|e| err("read the costs", e))?;
        let s = by_id.entry(id.clone()).or_insert_with(|| Costed {
            id,
            first: i64::MAX,
            days: BTreeMap::new(),
        });
        s.first = s.first.min(ts);
        *s.days.entry(crate::tz::day_of(ts)).or_insert(0.0) += cost;
    }
    Ok(by_id.into_values().collect())
}

/// Takes each session that starts in `[from, to)` as the boundary. For each,
/// it sums the cost of the listed days over all sessions that start before it
/// and over those that start at or after it, and sets the later side against
/// the dashboard amounts of the account that owns the later sessions. The
/// best `top` boundaries by total difference print, or all when `top` is 0.
pub fn reconcile(
    conn: &Connection,
    from: i64,
    to: i64,
    dashboard: &[(i64, f64)],
    top: usize,
) -> Result<String, String> {
    let all = costed(conn)?;
    let mut scored = Vec::new();
    for boundary in all
        .iter()
        .filter(|s| s.first >= from && s.first < to && s.days.values().sum::<f64>() > 0.0)
        // A boundary is a second, so a start shared with another session
        // cannot be applied by `assign`.
        .filter(|b| all.iter().all(|s| s.id == b.id || s.first != b.first))
    {
        let mut sides = [vec![0.0; dashboard.len()], vec![0.0; dashboard.len()]];
        for s in &all {
            let side = &mut sides[usize::from(s.first >= boundary.first)];
            for (i, (day, _)) in dashboard.iter().enumerate() {
                side[i] += s.days.get(day).copied().unwrap_or(0.0);
            }
        }
        let diff: f64 = dashboard
            .iter()
            .zip(&sides[1])
            .map(|((_, want), got)| (got - want).abs())
            .sum();
        scored.push((diff, boundary, sides));
    }
    if scored.is_empty() {
        return Ok("No sessions start in that window.".to_string());
    }
    scored.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.first.cmp(&b.1.first)));
    let shown = if top == 0 { scored.len() } else { top };
    let mut out = String::from("Each boundary is the first session of the later account.\n");
    for (diff, boundary, [before, after]) in scored.into_iter().take(shown) {
        out.push_str(&format!(
            "\nboundary {} ({}), total difference ${diff:.2}\n",
            boundary.id,
            crate::tz::iso(boundary.first)
        ));
        for (i, (day, want)) in dashboard.iter().enumerate() {
            out.push_str(&format!(
                "  {}  before ${:.2}  from here ${:.2}  dashboard ${want:.2}  difference {:+.2}\n",
                crate::aggregate::date_key(*day),
                before[i],
                after[i],
                after[i] - want
            ));
        }
    }
    Ok(out.trim_end().to_string())
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
    fn a_boundary_session_that_shares_its_start_second_is_refused() {
        let conn = store();
        conn.execute(
            "INSERT INTO usage_events (event_id, timestamp, session_id, account, model, effort,
                repo, branch, input_tokens, output_tokens, cache_creation_tokens,
                cache_read_tokens, cost_usd) VALUES ('9', ?1, 'd', 'w', 'm', '', 'r', 'b', 1, 1, 0, 0, 1.0)",
            [2 * DAY + 300],
        )
        .unwrap();
        let err = session_start(&conn, "c").unwrap_err();
        assert!(err.contains("same second as session d"), "{err}");
        assert!(session_start(&conn, "b").is_ok());
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

    #[test]
    fn reconcile_ranks_the_boundary_that_matches_the_dashboard_first() {
        let conn = store();
        let day2 = parse_dashboard("1970-01-03=12.00").unwrap();
        assert_eq!(day2.0, 2);
        let text = reconcile(&conn, 0, 3 * DAY, &[day2], 1).unwrap();
        assert!(text.contains("boundary b ("), "{text}");
        assert!(text.contains("total difference $0.00"), "{text}");
        assert!(
            text.contains("before $2.00  from here $12.00  dashboard $12.00"),
            "{text}"
        );
        assert!(!text.contains("boundary c"), "{text}");
    }

    #[test]
    fn a_dashboard_value_must_be_a_date_and_a_non_negative_amount() {
        for bad in [
            "1970-01-03",
            "x=1",
            "1970-01-03=-1",
            "1970-01-03=abc",
            "1970-01-03=NaN",
            "1970-01-03=inf",
            "1970-13-40=1",
        ] {
            assert!(parse_dashboard(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn sessions_before_the_window_count_on_the_before_side() {
        let conn = store();
        // The window starts at c, so a and b start before it.
        let day2 = parse_dashboard("1970-01-03=8.00").unwrap();
        let text = reconcile(&conn, 2 * DAY + 300, 3 * DAY, &[day2], 0).unwrap();
        assert!(
            text.contains("before $6.00  from here $8.00  dashboard $8.00"),
            "{text}"
        );
    }

    #[test]
    fn a_boundary_that_shares_its_second_with_another_session_is_not_offered() {
        let conn = store();
        conn.execute(
            "INSERT INTO usage_events (event_id, timestamp, session_id, account, model, effort,
                repo, branch, input_tokens, output_tokens, cache_creation_tokens,
                cache_read_tokens, cost_usd) VALUES ('9', ?1, 'd', 'w', 'm', '', 'r', 'b', 1, 1, 0, 0, 1.0)",
            [2 * DAY + 300],
        )
        .unwrap();
        let day2 = parse_dashboard("1970-01-03=12.00").unwrap();
        let text = reconcile(&conn, 0, 3 * DAY, &[day2], 0).unwrap();
        assert!(text.contains("boundary b ("), "{text}");
        assert!(!text.contains("boundary c"), "{text}");
        assert!(!text.contains("boundary d"), "{text}");
    }

    #[test]
    fn an_empty_store_has_no_boundary() {
        let conn = open_db(&scratch_dir("account-sessions-empty").join("u.db")).unwrap();
        let day = parse_dashboard("1970-01-03=1").unwrap();
        assert_eq!(
            reconcile(&conn, 0, DAY, &[day], 0).unwrap(),
            "No sessions start in that window."
        );
    }
}
