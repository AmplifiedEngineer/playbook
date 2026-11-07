// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The text of `playbook usage account ...`.

use crate::account_move::{self as mv, Scope};
use crate::account_sessions as sess;
use crate::account_store as store;
use crate::command::AccountCommand;
use crate::run::Paths;
use playbook_store::Connection;

const WEEK: i64 = 7 * 86_400;

pub fn run(paths: &Paths, command: &AccountCommand) -> Result<String, String> {
    run_on(crate::db::open_db(&paths.db)?, command)
}

/// Runs `command` on an open database.
pub fn run_on(conn: Connection, command: &AccountCommand) -> Result<String, String> {
    let zone = crate::tz::current();
    let now = crate::util::time::now_secs();
    let time = |text: &str| mv::parse_time(text, zone);
    match command {
        AccountCommand::List => store::list_text(&conn),
        AccountCommand::Fingerprint => super::session_account::env_fingerprint().ok_or_else(|| {
            format!(
                "{} is not set in this shell",
                super::session_account::TOKEN_ENV
            )
        }),
        AccountCommand::Name { key, name } => {
            store::set_name(&conn, key, name)?;
            let note = if store::has_events(&conn, key)? {
                ""
            } else {
                " (no events carry this key yet, check the spelling)"
            };
            Ok(if name.trim().is_empty() {
                format!("cleared the name of {key}")
            } else {
                format!("{key} is shown as {}{note}", name.trim())
            })
        }
        AccountCommand::Sessions { from, to } => {
            let from = from.as_deref().map_or(Ok(now - WEEK), time)?;
            let to = to.as_deref().map_or(Ok(now + 1), time)?;
            Ok(sess::list_text(&sess::sessions(&conn, from, to)?))
        }
        AccountCommand::Reconcile {
            from,
            to,
            dashboard,
            top,
        } => {
            let days = dashboard
                .iter()
                .map(|d| sess::parse_dashboard(d))
                .collect::<Result<Vec<_>, _>>()?;
            let mut seen = std::collections::BTreeSet::new();
            if let Some((day, _)) = days.iter().find(|(day, _)| !seen.insert(*day)) {
                return Err(format!(
                    "{} is listed twice in --dashboard",
                    crate::aggregate::date_key(*day)
                ));
            }
            let (from, to) = (time(from)?, time(to)?);
            if from >= to {
                return Err("--from must be earlier than --to".to_string());
            }
            sess::reconcile(&conn, from, to, &days, *top)
        }
        AccountCommand::Assign {
            from,
            to,
            from_session,
            before_session,
            through_session,
            session,
            target,
            was,
            confirm,
        } => {
            let scope = scope(
                &conn,
                [from, to, from_session, before_session, through_session],
                session.as_deref(),
                zone,
            )?;
            let target = store::resolve(&conn, target)?;
            let was = was
                .as_deref()
                .map(|w| store::resolve(&conn, w))
                .transpose()?;
            let (what, span) = describe(scope);
            if !confirm {
                let m = mv::plan(&conn, scope, &target, was.as_deref())?;
                return Ok(format!(
                    "dry run: {} events (${:.2}) and {} skill and agent records {what} {span} would move to {target}.\nAdd --confirm to apply.",
                    m.events, m.cost_usd, m.tool_events
                ));
            }
            Ok(
                match mv::assign(&conn, scope, &target, was.as_deref(), now)? {
                    Some((id, m)) => format!(
                        "move {id}: {} events (${:.2}) and {} skill and agent records {what} {span} now belong to {target}.\nUndo with: playbook usage account undo {id}",
                        m.events, m.cost_usd, m.tool_events
                    ),
                    None => {
                        format!("nothing to move {what} {span}: it already belongs to {target}.")
                    }
                },
            )
        }
        AccountCommand::Undo { id } => Ok(match mv::undo(&conn, *id)? {
            Some((id, n, 0)) => format!("undid move {id}: restored {n} records."),
            Some((id, n, skipped)) => format!(
                "undid move {id}: restored {n} records and left {skipped} that were relabeled since."
            ),
            None => match id {
                Some(id) => format!("move {id} does not exist or is already undone."),
                None => "no applied move to undo.".to_string(),
            },
        }),
    }
}

/// The words that name what a move covers, as a lead-in and the span.
fn describe(scope: Scope<'_>) -> (String, String) {
    let bounds = match (scope.from, scope.to) {
        (mv::UNBOUNDED_FROM, mv::UNBOUNDED_TO) => String::new(),
        (mv::UNBOUNDED_FROM, to) => format!(" before {}", crate::tz::iso(to)),
        (from, mv::UNBOUNDED_TO) => format!(" from {}", crate::tz::iso(from)),
        (from, to) => format!(" from {} to {}", crate::tz::iso(from), crate::tz::iso(to)),
    };
    if let Some(id) = scope.session {
        return ("of session".to_string(), format!("{id}{bounds}"));
    }
    let span = format!(
        "{} to {}",
        crate::tz::iso(scope.from),
        crate::tz::iso(scope.to)
    );
    let what = if scope.by_session {
        "of the sessions that start from"
    } else {
        "from"
    };
    (what.to_string(), span)
}

/// The scope of an `assign` from its five options: `[from, to, from_session,
/// before_session, through_session]`, and the session that limits it, if any.
fn scope<'a>(
    conn: &Connection,
    [from, to, from_session, before, through]: [&Option<String>; 5],
    session: Option<&'a str>,
    zone: crate::tz::Zone,
) -> Result<Scope<'a>, String> {
    if let Some(id) = session {
        return session_scope(conn, id, from.as_deref(), to.as_deref(), zone);
    }
    let start = |id: &str| sess::session_start(conn, id);
    if before.is_none() && through.is_none() {
        let (Some(from), Some(to)) = (from, to) else {
            return Err("give --from and --to, or a session boundary".to_string());
        };
        let (from, to) = (mv::parse_time(from, zone)?, mv::parse_time(to, zone)?);
        if from >= to {
            return Err("--from must be earlier than --to".to_string());
        }
        return Ok(Scope::time(from, to));
    }
    let lo = match (from, from_session) {
        (Some(t), _) => mv::parse_time(t, zone)?,
        (None, Some(id)) => start(id)?,
        (None, None) => {
            return Err("a session boundary needs --from or --from-session as the start".into())
        }
    };
    let hi = match (before, through) {
        (Some(id), _) => start(id)?,
        (None, Some(id)) => start(id)? + 1,
        (None, None) => lo,
    };
    if lo >= hi {
        return Err("the start must be earlier than the boundary session".to_string());
    }
    Ok(Scope::sessions(lo, hi))
}
/// One session, limited to `[from, to)` when either is given.
fn session_scope<'a>(
    conn: &Connection,
    id: &'a str,
    from: Option<&str>,
    to: Option<&str>,
    zone: crate::tz::Zone,
) -> Result<Scope<'a>, String> {
    if !mv::session_exists(conn, id)? {
        return Err(format!(
            "no session {id} in the usage store; list them with `account sessions`"
        ));
    }
    let bound = |text: Option<&str>, open: i64| text.map_or(Ok(open), |t| mv::parse_time(t, zone));
    let (from, to) = (
        bound(from, mv::UNBOUNDED_FROM)?,
        bound(to, mv::UNBOUNDED_TO)?,
    );
    if from >= to {
        return Err("--from must be earlier than --to".to_string());
    }
    Ok(Scope::in_session(id, from, to))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tz::Zone;

    fn conn() -> Connection {
        crate::account_store::tests::store()
    }

    fn some(s: &str) -> Option<String> {
        Some(s.to_string())
    }

    #[test]
    fn a_session_boundary_reads_the_session_start_and_the_start_option() {
        let c = conn();
        let none = None;
        // before s2 (starts at 300): sessions starting from 0 are s1 only.
        let s = scope(
            &c,
            [&some("1970-01-01"), &none, &none, &some("s2"), &none],
            None,
            Zone::Utc,
        )
        .unwrap();
        assert_eq!((s.from, s.to, s.by_session), (0, 300, true));
        // through s2 includes it.
        let s = scope(
            &c,
            [&none, &none, &some("s1"), &none, &some("s2")],
            None,
            Zone::Utc,
        )
        .unwrap();
        assert_eq!((s.from, s.to), (100, 301));
    }

    #[test]
    fn a_move_needs_a_complete_and_ordered_scope() {
        let c = conn();
        let none = None;
        let t = Zone::Utc;
        let bad = |opts: [&Option<String>; 5]| scope(&c, opts, None, t).is_err();
        assert!(bad([&some("1970-01-01"), &none, &none, &none, &none]));
        assert!(bad([&none, &none, &none, &some("s2"), &none]));
        assert!(bad([&none, &none, &some("s3"), &some("s2"), &none]));
        assert!(bad([&none, &none, &none, &some("nope"), &none]));
        let both = scope(
            &c,
            [
                &some("1970-01-02"),
                &some("1970-01-01"),
                &none,
                &none,
                &none,
            ],
            None,
            t,
        );
        assert!(both.is_err());
    }

    #[test]
    fn a_session_scope_takes_optional_bounds_and_needs_a_known_session() {
        let c = conn();
        let none = None;
        let t = Zone::Utc;
        let whole = scope(&c, [&none; 5], Some("s1"), t).unwrap();
        assert_eq!(
            (whole.session, whole.from, whole.to),
            (Some("s1"), mv::UNBOUNDED_FROM, mv::UNBOUNDED_TO)
        );
        let from = scope(
            &c,
            [&some("1970-01-02"), &none, &none, &none, &none],
            Some("s1"),
            t,
        );
        assert_eq!(from.unwrap().from, 86_400);
        let backwards = [
            &some("1970-01-02"),
            &some("1970-01-01"),
            &none,
            &none,
            &none,
        ];
        assert!(scope(&c, backwards, Some("s1"), t).is_err());
        let err = scope(&c, [&none; 5], Some("nope"), t).unwrap_err();
        assert!(err.contains("no session nope"), "{err}");
    }
}
