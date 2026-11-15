// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The session closer: plans which finished sessions to close and, for each,
//! removes a landed worktree behind a fixed order of guards. Git, the
//! filesystem and the database sit behind [`Guards`] and [`AuditSink`], so the
//! order of every call is testable on fakes.

use super::state;
use super::state::derive;
use super::{Pr, Settings, State, ACTIVE_SECONDS};
use crate::worktree::{
    Convention, Removal, ERR_NOT_MANAGED, ERR_NOT_REGISTERED, REPORT_CLEANUP_DISABLED,
    REPORT_DIRTY, REPORT_IN_USE, REPORT_LOCKED_LIVE, REPORT_LOCK_NO_OWNER, REPORT_NOT_LANDED,
    REPORT_WOULD_REMOVE,
};
use std::path::Path;

/// Seconds a blocked session waits before the closer looks at it again.
pub(crate) const CLOSE_RETRY_SECS: i64 = 86_400;

/// A session row as the closer reads it.
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub id: String,
    pub state: state::Row,
    pub pr: Option<Pr>,
    pub cwd: Option<String>,
    pub repo: Option<String>,
    pub worktree: Option<String>,
    pub close_blocked_at: Option<i64>,
    pub close_blocked_reason: Option<String>,
}

/// What the worktree guards say about a path, parsed from a report line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    Landed,
    Dirty,
    NotLanded,
    Locked,
    InUse,
    Disabled,
    NotRegistered,
    Unmanaged,
    Unknown(String),
}

/// One `lifecycle_actions` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Action {
    pub session_id: String,
    pub at: i64,
    pub action: &'static str,
    pub target: Option<String>,
    pub result: String,
}

/// Where a path check is asked from, so a fake can answer by call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Site {
    Step1,
    AfterRemove,
}

/// The git and filesystem calls of a close.
pub(crate) trait Guards {
    fn verdict(&self, row: &Candidate, path: &Path, now: i64) -> Verdict;
    fn convention(&self, path: &Path) -> Convention;
    fn in_use(&self, path: &Path) -> bool;
    fn path_exists(&self, path: &Path, site: Site) -> bool;
    fn remove(&self, row: &Candidate, path: &Path, now: i64) -> Result<Removal, String>;
}

/// The database writes and reads of a close.
pub(crate) trait AuditSink {
    fn write(&self, action: &Action) -> Result<(), String>;
    /// Matches only a row whose `closed_at` is null.
    fn set_blocked(&self, id: &str, code: &str, now: i64) -> bool;
    /// Matches only a row whose `closed_at` is null.
    fn set_closed(&self, id: &str, now: i64) -> bool;
}

/// The sessions to close at `now`: `Done`, quiet for over `ACTIVE_SECONDS`,
/// not blocked within `CLOSE_RETRY_SECS`, and not `own_id`, oldest first.
pub(crate) fn plan<'a>(
    rows: &'a [Candidate],
    now: i64,
    own_id: Option<&str>,
    session_stale_days: i64,
) -> Vec<&'a Candidate> {
    let settings = Settings {
        stale_after_days: session_stale_days,
        ..Settings::default()
    };
    let mut picked: Vec<&Candidate> = rows
        .iter()
        .filter(|r| Some(r.id.as_str()) != own_id)
        .filter(|r| derive(&r.state, r.pr, now, settings) == State::Done)
        .filter(|r| {
            r.state
                .last_event()
                .is_some_and(|t| now - t > ACTIVE_SECONDS)
        })
        .filter(|r| {
            r.close_blocked_at
                .is_none_or(|at| now - at > CLOSE_RETRY_SECS)
        })
        .collect();
    picked.sort_by(|a, b| (a.state.started_at, &a.id).cmp(&(b.state.started_at, &b.id)));
    picked
}

/// Reads the verdict out of a worktree report line or error text. A text it
/// does not know is `Unknown`, which blocks the close.
pub(crate) fn verdict_of(report: &str) -> Verdict {
    const BY_TAIL: [(&str, Verdict); 9] = [
        (REPORT_NOT_LANDED, Verdict::NotLanded),
        (REPORT_IN_USE, Verdict::InUse),
        (REPORT_DIRTY, Verdict::Dirty),
        (REPORT_LOCK_NO_OWNER, Verdict::Locked),
        (REPORT_LOCKED_LIVE, Verdict::Locked),
        (REPORT_WOULD_REMOVE, Verdict::Landed),
        (REPORT_CLEANUP_DISABLED, Verdict::Disabled),
        (ERR_NOT_REGISTERED, Verdict::NotRegistered),
        (ERR_NOT_MANAGED, Verdict::Unmanaged),
    ];
    BY_TAIL
        .into_iter()
        .find(|(tail, _)| report.ends_with(tail))
        .map_or_else(|| Verdict::Unknown(report.to_string()), |(_, v)| v)
}

impl Verdict {
    /// The block code, or `None` for `Landed`. The text of an `Unknown`
    /// verdict is the detail.
    fn block(&self) -> Option<Block> {
        let (code, detail) = match self {
            Verdict::Landed => return None,
            Verdict::Dirty => (CODE_DIRTY, ""),
            Verdict::NotLanded => (CODE_NOT_LANDED, ""),
            Verdict::Locked => (CODE_LOCKED, ""),
            Verdict::InUse => (CODE_IN_USE, ""),
            Verdict::Disabled => (CODE_CLEANUP_DISABLED, ""),
            Verdict::NotRegistered => (CODE_NOT_REGISTERED, ""),
            Verdict::Unmanaged => (CODE_UNMANAGED, ""),
            Verdict::Unknown(text) => (CODE_NOT_CLOSABLE, text.as_str()),
        };
        Some(Block::new(code, detail))
    }
}

/// Reason stored on the audit row of a worktree removal.
const REASON_LANDED: &str = "landed";
/// The `lifecycle_actions.action` of a worktree removal and of a block.
const ACTION_WORKTREE: &str = "worktree";
const ACTION_BLOCKED: &str = "blocked";

const CODE_DIRTY: &str = "dirty";
const CODE_NOT_LANDED: &str = "not_landed";
const CODE_LOCKED: &str = "locked";
const CODE_IN_USE: &str = "in_use";
const CODE_CLEANUP_DISABLED: &str = "cleanup_disabled";
const CODE_NOT_REGISTERED: &str = "not_registered";
const CODE_UNMANAGED: &str = "unmanaged";
const CODE_NOT_CLOSABLE: &str = "not_closable";

/// Why a session stays open: the stable code stored on the row, and the
/// detail that only the audit row keeps.
struct Block {
    code: &'static str,
    detail: String,
}

impl Block {
    fn new(code: &'static str, detail: impl Into<String>) -> Block {
        Block {
            code,
            detail: detail.into(),
        }
    }
}

/// Closes each planned session. Every session ends closed, blocked with a
/// stored code, or untouched.
pub(crate) fn run(plan: &[&Candidate], now: i64, guards: &dyn Guards, audit: &dyn AuditSink) {
    for row in plan {
        if let Err(block) = close_one(row, now, guards, audit) {
            record_block(row, &block, now, audit);
        }
    }
}

/// Runs the guards of one session in order, then removes its worktree. `Ok`
/// means closed or left untouched, `Err` means blocked.
fn close_one(
    row: &Candidate,
    now: i64,
    guards: &dyn Guards,
    audit: &dyn AuditSink,
) -> Result<(), Block> {
    let Some(worktree) = row.worktree.as_deref() else {
        return Ok(());
    };
    let path = Path::new(worktree);
    if let Some(block) = guards.verdict(row, path, now).block() {
        return Err(block);
    }
    if guards.in_use(path) {
        return Err(Block::new(CODE_IN_USE, ""));
    }
    let written = audit.write(&Action {
        session_id: row.id.clone(),
        at: now,
        action: ACTION_WORKTREE,
        target: Some(worktree.to_string()),
        result: REASON_LANDED.to_string(),
    });
    if written.is_err() {
        return Ok(());
    }
    // Only a confirmed removal closes the row; anything else stays open.
    if let Ok(Removal::Removed(_)) = guards.remove(row, path, now) {
        audit.set_closed(&row.id, now);
    }
    Ok(())
}

/// Stores the block code on the row and, when the code differs from the one
/// already stored, one audit row. Storing refreshes the retry clock either way.
fn record_block(row: &Candidate, block: &Block, now: i64, audit: &dyn AuditSink) {
    if row.close_blocked_reason.as_deref() != Some(block.code) {
        write_block_audit(row, block, now, audit);
    }
    audit.set_blocked(&row.id, block.code, now);
}

fn write_block_audit(row: &Candidate, block: &Block, now: i64, audit: &dyn AuditSink) {
    let result = if block.detail.is_empty() {
        block.code.to_string()
    } else {
        format!("{}: {}", block.code, block.detail)
    };
    audit
        .write(&Action {
            session_id: row.id.clone(),
            at: now,
            action: ACTION_BLOCKED,
            target: row.worktree.clone(),
            result,
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::session_view::PrKind;
    use std::cell::RefCell;

    const NOW: i64 = 10_000_000;
    const STALE_DAYS: i64 = 3;
    const WORKTREE: &str = "/r/wt";

    #[derive(Clone, Copy)]
    enum Outcome {
        Removed,
        Nothing,
        Failed,
    }

    struct Fake {
        log: RefCell<Vec<String>>,
        actions: RefCell<Vec<Action>>,
        closed: RefCell<Vec<(String, i64)>>,
        blocked: RefCell<Vec<(String, String, i64)>>,
        verdict: Verdict,
        in_use: bool,
        removal: Outcome,
        worktree_write_fails: bool,
    }

    impl Fake {
        fn allowing() -> Fake {
            Fake {
                log: RefCell::new(Vec::new()),
                actions: RefCell::new(Vec::new()),
                closed: RefCell::new(Vec::new()),
                blocked: RefCell::new(Vec::new()),
                verdict: Verdict::Landed,
                in_use: false,
                removal: Outcome::Removed,
                worktree_write_fails: false,
            }
        }
        fn note(&self, entry: impl Into<String>) {
            self.log.borrow_mut().push(entry.into());
        }
        fn log(&self) -> Vec<String> {
            self.log.borrow().clone()
        }
    }

    impl Guards for Fake {
        fn verdict(&self, _: &Candidate, _: &Path, _: i64) -> Verdict {
            self.note("verdict");
            self.verdict.clone()
        }
        fn convention(&self, _: &Path) -> Convention {
            Convention::CcLauncher
        }
        fn in_use(&self, _: &Path) -> bool {
            self.note("in_use");
            self.in_use
        }
        fn path_exists(&self, _: &Path, _: Site) -> bool {
            true
        }
        fn remove(&self, _: &Candidate, path: &Path, _: i64) -> Result<Removal, String> {
            self.note("remove");
            let shown = path.display().to_string();
            match self.removal {
                Outcome::Removed => Ok(Removal::Removed(shown)),
                Outcome::Nothing => Ok(Removal::Nothing(shown)),
                Outcome::Failed => Err(shown),
            }
        }
    }

    impl AuditSink for Fake {
        fn write(&self, action: &Action) -> Result<(), String> {
            self.note(format!("audit {} {}", action.action, action.result));
            self.actions.borrow_mut().push(action.clone());
            if self.worktree_write_fails && action.action == ACTION_WORKTREE {
                return Err("disk full".to_string());
            }
            Ok(())
        }
        fn set_blocked(&self, id: &str, code: &str, now: i64) -> bool {
            self.note(format!("set_blocked({code})"));
            self.blocked
                .borrow_mut()
                .push((id.to_string(), code.to_string(), now));
            true
        }
        fn set_closed(&self, id: &str, now: i64) -> bool {
            self.note("set_closed");
            self.closed.borrow_mut().push((id.to_string(), now));
            true
        }
    }

    fn done(id: &str) -> Candidate {
        Candidate {
            id: id.to_string(),
            state: state::Row {
                started_at: Some(NOW - 5000),
                last_stop_at: Some(NOW - 901),
                ..Default::default()
            },
            pr: Some(Pr {
                number: 1,
                kind: PrKind::Merged,
            }),
            cwd: Some(WORKTREE.to_string()),
            repo: Some("/r".to_string()),
            worktree: Some(WORKTREE.to_string()),
            close_blocked_at: None,
            close_blocked_reason: None,
        }
    }

    fn state_of(row: &Candidate) -> State {
        let settings = Settings {
            stale_after_days: STALE_DAYS,
            ..Settings::default()
        };
        derive(&row.state, row.pr, NOW, settings)
    }

    #[test]
    fn a_done_session_with_a_clean_landed_worktree_is_closed_and_audited() {
        // Arrange
        let fake = Fake::allowing();
        let row = done("s1");

        // Act
        run(&[&row], NOW, &fake, &fake);

        // Assert
        assert_eq!(
            fake.log(),
            [
                "verdict",
                "in_use",
                "audit worktree landed",
                "remove",
                "set_closed",
            ]
        );
        assert_eq!(
            *fake.actions.borrow(),
            [Action {
                session_id: "s1".to_string(),
                at: NOW,
                action: ACTION_WORKTREE,
                target: Some(WORKTREE.to_string()),
                result: REASON_LANDED.to_string(),
            }]
        );
        assert_eq!(*fake.closed.borrow(), [("s1".to_string(), NOW)]);
    }

    #[test]
    fn a_removal_that_did_not_remove_leaves_the_session_open() {
        for removal in [Outcome::Nothing, Outcome::Failed] {
            // Arrange
            let fake = Fake {
                removal,
                ..Fake::allowing()
            };
            let row = done("s1");

            // Act
            run(&[&row], NOW, &fake, &fake);

            // Assert
            assert_eq!(
                fake.log(),
                ["verdict", "in_use", "audit worktree landed", "remove"]
            );
            assert!(fake.closed.borrow().is_empty());
        }
    }

    #[test]
    fn a_failed_worktree_audit_write_stops_before_the_removal() {
        // Arrange
        let fake = Fake {
            worktree_write_fails: true,
            ..Fake::allowing()
        };
        let row = done("s1");

        // Act
        run(&[&row], NOW, &fake, &fake);

        // Assert
        assert_eq!(fake.log(), ["verdict", "in_use", "audit worktree landed"]);
        assert!(fake.closed.borrow().is_empty());
    }

    fn planned(rows: &[Candidate], own_id: Option<&str>) -> Vec<String> {
        plan(rows, NOW, own_id, STALE_DAYS)
            .iter()
            .map(|r| r.id.clone())
            .collect()
    }

    #[test]
    fn plan_picks_a_done_session_only_once_it_has_been_quiet_over_900_seconds() {
        for (quiet, expected) in [(899, 0), (900, 0), (901, 1)] {
            // Arrange
            let mut row = done("s1");
            row.state.last_stop_at = Some(NOW - quiet);

            // Act
            let picked = planned(&[row], None);

            // Assert
            assert_eq!(picked.len(), expected, "quiet for {quiet}s");
        }
    }

    #[test]
    fn plan_skips_every_state_but_done() {
        type Tweak = fn(&mut Candidate);
        let cases: [(State, Tweak); 6] = [
            (State::Working, |r| {
                r.pr = None;
                r.state.last_stop_at = Some(NOW - 60);
            }),
            (State::Waiting, |r| {
                r.pr = None;
                r.state.waiting_after_prompt = true;
            }),
            (State::Idle, |r| {
                r.pr = None;
                r.state.ended_at = Some(NOW - 901);
            }),
            (State::Stale, |r| {
                r.pr = None;
                r.state.started_at = Some(NOW - 5 * 86_400);
                r.state.last_stop_at = Some(NOW - 4 * 86_400);
            }),
            (State::Crashed, |r| {
                r.pr = None;
                r.state.crashed_at = Some(NOW - 901);
            }),
            (State::Closed, |r| r.state.closed_at = Some(NOW - 1)),
        ];
        for (expected, tweak) in cases {
            // Arrange
            let mut row = done("s1");
            tweak(&mut row);
            assert_eq!(state_of(&row), expected);

            // Act
            let picked = planned(&[row], None);

            // Assert
            assert!(picked.is_empty(), "{expected:?} must not be planned");
        }
    }

    #[test]
    fn plan_orders_by_start_then_id() {
        // Arrange
        let mut late = done("a");
        late.state.started_at = Some(NOW - 1000);
        let mut tie_b = done("b");
        tie_b.state.started_at = Some(NOW - 5000);
        let tie_a = done("c");

        // Act
        let picked = planned(&[late, tie_b, tie_a], None);

        // Assert
        assert_eq!(picked, ["b", "c", "a"]);
    }

    #[test]
    fn plan_leaves_out_the_calling_session() {
        // Arrange
        let rows = [done("own"), done("other")];

        // Act
        let picked = planned(&rows, Some("own"));

        // Assert
        assert_eq!(picked, ["other"]);
    }

    #[test]
    fn plan_retries_a_blocked_session_only_after_a_day_and_a_second() {
        for (offset, expected) in [(100, 0), (CLOSE_RETRY_SECS, 0), (CLOSE_RETRY_SECS + 1, 1)] {
            // Arrange
            let mut row = done("s1");
            row.close_blocked_at = Some(NOW - offset);

            // Act
            let picked = planned(&[row], None);

            // Assert
            assert_eq!(picked.len(), expected, "blocked {offset}s ago");
        }
    }

    #[test]
    fn a_repeat_of_the_same_refusal_refreshes_the_time_without_a_second_audit_row() {
        // Arrange
        let fake = Fake {
            verdict: Verdict::Dirty,
            ..Fake::allowing()
        };
        let first = done("s1");
        run(&[&first], NOW, &fake, &fake);
        let mut again = done("s1");
        again.close_blocked_at = Some(NOW);
        again.close_blocked_reason = Some(CODE_DIRTY.to_string());
        let later = NOW + CLOSE_RETRY_SECS + 1;
        let rows = [again];
        let picked = plan(&rows, later, None, STALE_DAYS);

        // Act
        run(&picked, later, &fake, &fake);

        // Assert
        assert_eq!(fake.actions.borrow().len(), 1);
        assert_eq!(
            *fake.blocked.borrow(),
            [
                ("s1".to_string(), "dirty".to_string(), NOW),
                ("s1".to_string(), "dirty".to_string(), later),
            ]
        );
    }

    #[test]
    fn a_different_refusal_than_the_stored_one_writes_an_audit_row() {
        // Arrange
        let fake = Fake {
            verdict: Verdict::Dirty,
            ..Fake::allowing()
        };
        let mut row = done("s1");
        row.close_blocked_reason = Some(CODE_NOT_LANDED.to_string());

        // Act
        run(&[&row], NOW, &fake, &fake);

        // Assert
        assert_eq!(
            fake.log(),
            ["verdict", "audit blocked dirty", "set_blocked(dirty)"]
        );
    }

    #[test]
    fn verdict_of_maps_each_report_text_and_fails_closed() {
        // The texts are written out so a changed constant fails here. The
        // end-to-end cases through the real `would_remove_with` come with the
        // real-git tests.
        let cases = [
            ("worktree /x/wt: not landed, skipping", Verdict::NotLanded),
            (
                "worktree /x/wt: a process is using it, skipping",
                Verdict::InUse,
            ),
            (
                "worktree /x/wt: has uncommitted changes, skipping",
                Verdict::Dirty,
            ),
            (
                "worktree /x/wt: locked with no readable owner pid, skipping",
                Verdict::Locked,
            ),
            (
                "worktree /x/wt: locked by a live process, skipping",
                Verdict::Locked,
            ),
            ("worktree /x/wt: would remove (dry run)", Verdict::Landed),
            (
                "worktree remove: worktree.cleanup.enabled is false, nothing to do",
                Verdict::Disabled,
            ),
            (
                "/x/wt is not a registered git worktree",
                Verdict::NotRegistered,
            ),
            (
                "/x/wt is not a playbook-managed worktree",
                Verdict::Unmanaged,
            ),
            (
                "worktree /x/wt: something new",
                Verdict::Unknown("worktree /x/wt: something new".to_string()),
            ),
        ];
        for (text, expected) in cases {
            // Act
            let verdict = verdict_of(text);

            // Assert
            assert_eq!(verdict, expected, "{text}");
        }
    }

    #[test]
    fn a_verdict_that_is_not_landed_blocks_with_its_code_and_stops() {
        let cases = [
            (Verdict::Dirty, "dirty", "dirty"),
            (Verdict::NotLanded, "not_landed", "not_landed"),
            (Verdict::Locked, "locked", "locked"),
            (Verdict::InUse, "in_use", "in_use"),
            (Verdict::Disabled, "cleanup_disabled", "cleanup_disabled"),
            (Verdict::NotRegistered, "not_registered", "not_registered"),
            (Verdict::Unmanaged, "unmanaged", "unmanaged"),
            (
                Verdict::Unknown("odd".to_string()),
                "not_closable",
                "not_closable: odd",
            ),
        ];
        for (verdict, code, audited) in cases {
            // Arrange
            let fake = Fake {
                verdict,
                ..Fake::allowing()
            };
            let row = done("s1");

            // Act
            run(&[&row], NOW, &fake, &fake);

            // Assert
            assert_eq!(
                fake.log(),
                [
                    "verdict".to_string(),
                    format!("audit blocked {audited}"),
                    format!("set_blocked({code})"),
                ]
            );
        }
    }

    #[test]
    fn a_worktree_in_use_blocks_after_the_in_use_check() {
        // Arrange
        let fake = Fake {
            in_use: true,
            ..Fake::allowing()
        };
        let row = done("s1");

        // Act
        run(&[&row], NOW, &fake, &fake);

        // Assert
        assert_eq!(
            fake.log(),
            [
                "verdict",
                "in_use",
                "audit blocked in_use",
                "set_blocked(in_use)"
            ]
        );
    }
}
