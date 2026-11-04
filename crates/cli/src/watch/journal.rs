// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The step an action reached, written before each side effect, and what to
//! do about a `running` item found after a kill. The caller holds the
//! instance lock, so no live process owns a `running` item.

use super::store::{Store, StoreError, WorkRow};

/// A worktree exists. Nothing has been pushed.
pub const WORKTREE: &str = "worktree";
/// The branch was pushed. The marker comment may not be posted yet.
pub const PUSHED: &str = "pushed";

/// The effects recovery may need. A fake stands in for git and GitHub in tests.
pub trait RecoverOps {
    fn remove_worktree(&self, path: &str) -> Result<(), String>;
    fn branch_pushed(&self, repo: &str, branch: &str) -> Result<bool, String>;
    /// Posts the idempotency marker unless it is already there.
    fn ensure_marker(&self, row: &WorkRow) -> Result<(), String>;
}

/// What recovery did with one item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovered {
    /// The worktree was removed and the item is queued again.
    Requeued,
    /// The push had happened, so the marker was posted and the item is done.
    Completed,
    /// The step is not one recovery knows, so the item is dead and its
    /// worktree stays for you.
    Dead,
}

/// Looks at every `running` item and puts it in a state the next cycle reads.
pub fn recover(
    store: &mut Store,
    ops: &dyn RecoverOps,
) -> Result<Vec<(i64, Recovered)>, StoreError> {
    let mut out = Vec::new();
    for id in store.ids_in_state("running")? {
        let (Some(row), journal) = (store.work(id)?, store.journal(id)?) else {
            continue;
        };
        let step = journal.as_ref().map(|j| j.step.as_str());
        let pushed = match (&journal, step) {
            (Some(j), Some(PUSHED)) => ops.branch_pushed(&row.repo, &j.branch).unwrap_or(false),
            _ => false,
        };
        let result = match (&journal, step) {
            (Some(_), Some(PUSHED)) if pushed => {
                if ops.ensure_marker(&row).is_ok() {
                    store.transaction(|tx| {
                        tx.set_state(id, "done", Some("recovered"))?;
                        tx.clear_journal(id)
                    })?;
                    Recovered::Completed
                } else {
                    // The marker could not be posted: leave it for the next start.
                    continue;
                }
            }
            (Some(j), Some(WORKTREE | PUSHED)) => {
                if ops.remove_worktree(&j.worktree).is_ok() {
                    store.transaction(|tx| {
                        tx.set_state(id, "queued", None)?;
                        tx.clear_journal(id)
                    })?;
                    Recovered::Requeued
                } else {
                    store.transaction(|tx| tx.set_state(id, "dead", Some("recovery_failed")))?;
                    Recovered::Dead
                }
            }
            _ => {
                store.transaction(|tx| tx.set_state(id, "dead", Some("unknown_step")))?;
                Recovered::Dead
            }
        };
        out.push((id, result));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::store::NewWork;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Ops {
        removed: RefCell<Vec<String>>,
        markers: RefCell<usize>,
        pushed: bool,
    }

    impl RecoverOps for Ops {
        fn remove_worktree(&self, path: &str) -> Result<(), String> {
            self.removed.borrow_mut().push(path.to_string());
            Ok(())
        }
        fn branch_pushed(&self, _: &str, _: &str) -> Result<bool, String> {
            Ok(self.pushed)
        }
        fn ensure_marker(&self, _: &WorkRow) -> Result<(), String> {
            *self.markers.borrow_mut() += 1;
            Ok(())
        }
    }

    /// A store with one running item whose journal says `step`.
    fn running(tag: &str, step: Option<&str>) -> (Store, i64) {
        let dir = crate::common::test_support::scratch_dir(tag);
        let mut store = Store::open(&dir.join("data/watch.db")).unwrap();
        let item = NewWork {
            key: "issue:1:opened:r",
            rule: "r",
            class: "label",
            repo: "o/r",
            object: 1,
            body_hash: [0; 32],
        };
        let id = store
            .transaction(|tx| {
                let crate::watch::store::Insert::Queued(id) = tx.insert_work(&item, 1)? else {
                    panic!("not queued");
                };
                tx.set_state(id, "running", None)?;
                if let Some(step) = step {
                    tx.set_journal(id, step, "/tmp/wt", "watch/branch")?;
                }
                Ok(id)
            })
            .unwrap();
        (store, id)
    }

    fn state(store: &Store, id: i64) -> (String, Option<String>) {
        let row = store.work(id).unwrap().unwrap();
        (row.state, row.reason)
    }

    #[test]
    fn a_kill_before_any_push_removes_the_worktree_and_requeues() {
        let (mut store, id) = running("watch-journal-before", Some(WORKTREE));
        let ops = Ops::default();
        let got = recover(&mut store, &ops).unwrap();
        assert_eq!(got, vec![(id, Recovered::Requeued)]);
        assert_eq!(ops.removed.borrow().as_slice(), ["/tmp/wt"]);
        assert_eq!(state(&store, id).0, "queued");
        assert!(store.journal(id).unwrap().is_none());
    }

    #[test]
    fn a_kill_after_the_push_posts_the_marker_and_records_done() {
        let (mut store, id) = running("watch-journal-pushed", Some(PUSHED));
        let ops = Ops {
            pushed: true,
            ..Ops::default()
        };
        let got = recover(&mut store, &ops).unwrap();
        assert_eq!(got, vec![(id, Recovered::Completed)]);
        assert_eq!(*ops.markers.borrow(), 1);
        assert_eq!(state(&store, id), ("done".into(), Some("recovered".into())));
        assert!(ops.removed.borrow().is_empty());
    }

    #[test]
    fn a_pushed_step_with_no_branch_on_the_remote_is_treated_as_before_the_push() {
        let (mut store, id) = running("watch-journal-unpushed", Some(PUSHED));
        let ops = Ops::default();
        assert_eq!(
            recover(&mut store, &ops).unwrap(),
            vec![(id, Recovered::Requeued)]
        );
    }

    #[test]
    fn an_unknown_step_is_dead_and_the_worktree_stays() {
        for step in [Some("mystery"), None] {
            let (mut store, id) = running("watch-journal-unknown", step);
            let ops = Ops::default();
            let got = recover(&mut store, &ops).unwrap();
            assert_eq!(got, vec![(id, Recovered::Dead)]);
            assert_eq!(
                state(&store, id),
                ("dead".into(), Some("unknown_step".into()))
            );
            assert!(ops.removed.borrow().is_empty());
        }
    }
}
