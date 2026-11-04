// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The worktree sweep behind the gc run lock. Callers go through
//! [`sweep_locked`], so a sweep never overlaps a gc run (or another sweep).

use std::path::Path;
use std::time::Duration;

use super::lock::{with_run_lock, NoHook, RunOutcome};
use super::{Clock, GC_DIR};
use crate::common::paths::playbook_root_from;

/// What a locked sweep did.
#[derive(Debug, PartialEq, Eq)]
pub enum SweepRun {
    /// The sweep ran. One line per worktree it looked at.
    Swept(Vec<String>),
    /// A gc run or another sweep holds the lock, so nothing was swept.
    Busy,
}

/// Runs [`crate::worktree::sweep`] while holding the gc run lock, without
/// waiting for it. A lock folder older than the stale limit is taken over, so
/// a crashed run never disables the sweep.
pub fn sweep_locked(
    home: &Path,
    clock: &dyn Clock,
    (repo_root, repo_slug): (&Path, Option<&str>),
    dry_run: bool,
) -> Result<SweepRun, String> {
    let gc_dir = playbook_root_from(home).join(GC_DIR);
    let ran = with_run_lock(&gc_dir, Duration::ZERO, clock, &NoHook, |_guard| {
        crate::worktree::sweep(repo_root, home, repo_slug, dry_run, clock.now())
    });
    match ran {
        Ok(RunOutcome::Ran(report)) => report.map(SweepRun::Swept),
        Ok(RunOutcome::AlreadyRunning) => Ok(SweepRun::Busy),
        Err(err) => Err(format!("could not take the gc lock: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gc::{FixedClock, RUN_LOCK_STALE};
    use crate::worktree::{classify, Convention};
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    fn scratch(tag: &str) -> PathBuf {
        let dir = crate::common::test_support::scratch_dir(tag);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn git_repo(dir: &Path) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["init", "-q"])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn lock_folder(home: &Path) -> PathBuf {
        let lock = playbook_root_from(home).join(GC_DIR).join(".lock");
        fs::create_dir_all(&lock).unwrap();
        lock
    }

    #[test]
    fn hand_made_worktree_is_unmanaged() {
        // Arrange
        let home = scratch("wt-unmanaged-home");
        let hand_made = scratch("wt-unmanaged-checkout");

        // Act
        let convention = classify(&hand_made, &home);

        // Assert
        assert_eq!(convention, Convention::Unmanaged);
    }

    #[test]
    fn fresh_lock_makes_direct_callers_skip() {
        // Arrange
        let home = scratch("wt-fresh-lock-home");
        let repo = scratch("wt-fresh-lock-repo");
        git_repo(&repo);
        let lock = lock_folder(&home);
        let lock_time =
            crate::common::time::epoch_secs_of(fs::metadata(&lock).unwrap().modified().unwrap())
                .unwrap();

        // Act
        let run = sweep_locked(&home, &FixedClock(lock_time), (&repo, None), false);

        // Assert
        assert_eq!(run, Ok(SweepRun::Busy));
    }

    #[test]
    fn stale_lock_does_not_block_direct_callers() {
        // Arrange
        let home = scratch("wt-stale-lock-home");
        let repo = scratch("wt-stale-lock-repo");
        git_repo(&repo);
        let lock = lock_folder(&home);
        let lock_time =
            crate::common::time::epoch_secs_of(fs::metadata(&lock).unwrap().modified().unwrap())
                .unwrap();
        let later = lock_time + i64::try_from(RUN_LOCK_STALE.as_secs()).unwrap() + 1;

        // Act
        let run = sweep_locked(&home, &FixedClock(later), (&repo, None), true);

        // Assert
        assert!(matches!(run, Ok(SweepRun::Swept(_))), "{run:?}");
    }
}
