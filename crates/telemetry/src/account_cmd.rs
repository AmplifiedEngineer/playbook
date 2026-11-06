// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The text of `playbook usage account ...`.

use crate::account_move::{self as mv, Scope};
use crate::account_sessions as sess;
use crate::account_store as store;
use crate::command::AccountCommand;
use crate::run::Paths;
use rusqlite::Connection;

const WEEK: i64 = 7 * 86_400;

pub fn run(paths: &Paths, command: &AccountCommand) -> Result<String, String> {
    let conn = crate::db::open_db(&paths.db)?;
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
            target,
            was,
            confirm,
        } => {
            let scope = scope(
                &conn,
                [from, to, from_session, before_session, through_session],
                zone,
            )?;
            let target = store::resolve(&conn, target)?;
            let was = was
                .as_deref()
                .map(|w| store::resolve(&conn, w))
                .transpose()?;
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

/// The scope of an `assign` from its five options: `[from, to, from_session,
/// before_session, through_session]`.
fn scope(
    conn: &Connection,
    [from, to, from_session, before, through]: [&Option<String>; 5],
    zone: crate::tz::Zone,
) -> Result<Scope, String> {
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
            Zone::Utc,
        )
        .unwrap();
        assert_eq!((s.from, s.to, s.by_session), (0, 300, true));
        // through s2 includes it.
        let s = scope(
            &c,
            [&none, &none, &some("s1"), &none, &some("s2")],
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
        assert!(scope(&c, [&some("1970-01-01"), &none, &none, &none, &none], t).is_err());
        assert!(scope(&c, [&none, &none, &none, &some("s2"), &none], t).is_err());
        assert!(scope(&c, [&none, &none, &some("s3"), &some("s2"), &none], t).is_err());
        assert!(scope(&c, [&none, &none, &none, &some("nope"), &none], t).is_err());
        let both = scope(
            &c,
            [
                &some("1970-01-02"),
                &some("1970-01-01"),
                &none,
                &none,
                &none,
            ],
            t,
        );
        assert!(both.is_err());
    }
}
