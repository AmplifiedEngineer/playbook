// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook sessions sweep --dry-run` (ADR-0024, stage 4): for each finished
//! session, what a sweep would do with its worktree, judged by the same guards
//! as `playbook worktree remove` (landed, clean, unlocked, not in use). It only
//! reads. It removes nothing, stops no process and closes no tab or pane.

use super::{link_label, Record, State, ACTIVE_SECONDS};
use std::fmt::Write as _;
use std::path::Path;

/// The dry run report for `records`. `judge` gives the verdict line for one
/// worktree path, from the worktree sweep guards.
pub(crate) fn report(records: &[Record], now: i64, judge: &dyn Fn(&Path) -> String) -> String {
    let mut out = String::from("Dry run: nothing is removed, stopped or closed.\n");
    let mut listed = 0;
    for r in records {
        let verdict = match r.state {
            State::Done if now - r.last_activity < ACTIVE_SECONDS => {
                "active in the last 15 minutes, nothing to do".to_string()
            }
            State::Done if r.place.gone => "worktree already gone: close the session".to_string(),
            State::Done => match &r.place.worktree {
                Some(path) => judge(Path::new(path)),
                None => "no worktree to remove: close the session".to_string(),
            },
            State::Stale => "stale, not swept: resume it or close it by hand".to_string(),
            State::Working | State::Waiting | State::Idle | State::Crashed | State::Closed => {
                continue
            }
        };
        listed += 1;
        let id: String = r.id.chars().take(8).collect();
        let _ = writeln!(
            out,
            "{id} {} {} ({}): {verdict}",
            r.repo,
            r.branch,
            link_label(r)
        );
    }
    if listed == 0 {
        out.push_str("No finished or stale sessions.\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::{Place, Pr, PrKind};

    const NOW: i64 = 1_800_000_000;

    fn rec(id: &str, state: State, ago: i64, worktree: Option<&str>) -> Record {
        Record {
            id: id.into(),
            agent: "claude".into(),
            repo: "playbook".into(),
            branch: "feat/536-x".into(),
            place: Place {
                worktree: worktree.map(str::to_string),
                ..Place::default()
            },
            issue: Some(536),
            pr: (state == State::Done).then_some(Pr {
                number: 812,
                kind: PrKind::Merged,
            }),
            state,
            last_activity: NOW - ago,
            context_tokens: 0,
            cost_usd: 0.0,
            unpriced: 0,
            messages: 1,
            needs_attention: false,
            hint: String::new(),
        }
    }

    fn judge(path: &Path) -> String {
        format!("worktree {}: would remove (dry run)", path.display())
    }

    #[test]
    fn a_done_session_with_a_worktree_gets_the_worktree_guard_verdict() {
        let text = report(
            &[rec("aaaaaaaa-1", State::Done, 3600, Some("/w/x"))],
            NOW,
            &judge,
        );
        assert_eq!(
            text,
            "Dry run: nothing is removed, stopped or closed.\n\
             aaaaaaaa playbook feat/536-x (PR #812 merged): worktree /w/x: would remove (dry run)\n"
        );
    }

    #[test]
    fn a_recent_session_is_never_judged_even_when_done() {
        let never = |_: &Path| -> String { panic!("an active session must not be judged") };
        let text = report(&[rec("a", State::Done, 60, Some("/w/x"))], NOW, &never);
        assert!(text.contains("active in the last 15 minutes, nothing to do"));
    }

    #[test]
    fn a_done_session_without_a_worktree_is_only_told_to_close() {
        let never = |_: &Path| -> String { panic!("no worktree to judge") };
        let mut gone = rec("b", State::Done, 3600, Some("/w/y"));
        gone.place.gone = true;
        let text = report(&[rec("a", State::Done, 3600, None), gone], NOW, &never);
        assert!(text.contains("no worktree to remove: close the session"));
        assert!(text.contains("worktree already gone: close the session"));
    }

    #[test]
    fn a_stale_session_is_listed_but_never_judged() {
        let never = |_: &Path| -> String { panic!("a stale session is never swept") };
        let text = report(
            &[rec("a", State::Stale, 9 * 86_400, Some("/w/x"))],
            NOW,
            &never,
        );
        assert!(text.contains("stale, not swept: resume it or close it by hand"));
    }

    #[test]
    fn working_idle_waiting_crashed_and_closed_sessions_are_left_out() {
        let never = |_: &Path| -> String { panic!("not finished") };
        let text = report(
            &[
                rec("a", State::Working, 10, Some("/w/x")),
                rec("b", State::Idle, 3600, Some("/w/y")),
                rec("c", State::Waiting, 10, Some("/w/z")),
                rec("d", State::Crashed, 3600, Some("/w/a")),
                rec("e", State::Closed, 3600, Some("/w/b")),
            ],
            NOW,
            &never,
        );
        assert_eq!(
            text,
            "Dry run: nothing is removed, stopped or closed.\nNo finished or stale sessions.\n"
        );
    }
}
