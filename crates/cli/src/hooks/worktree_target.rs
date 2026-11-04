// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Seeds a new worktree's `target` folder with a copy-on-write clone of the
//! main checkout's, so a Rust build in the worktree reuses the dependency
//! artifacts instead of compiling them again. The clone shares disk blocks
//! until either side writes. It is an optimisation only: any problem leaves
//! the worktree without a `target` and never fails the hook.

use crate::common::run_with_timeout;
use crate::hooks::worktree_create::git;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// The build folder Cargo uses unless told otherwise.
const TARGET_DIR: &str = "target";
/// A clone is metadata only, so even a large `target` finishes well within this.
const CLONE_TIMEOUT: Duration = Duration::from_secs(120);

/// Clones `<main_root>/target` to `<worktree>/target` when the main checkout
/// has one that git ignores, the worktree has none, and the platform and
/// filesystem support clones. Returns whether a clone was made.
pub(crate) fn clone_warm_target(main_root: &Path, worktree: &Path) -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    let source = main_root.join(TARGET_DIR);
    let dest = worktree.join(TARGET_DIR);
    // An untracked clone would make the new worktree look dirty, and
    // `worktree-remove` keeps a dirty worktree.
    if !source.is_dir() || dest.symlink_metadata().is_ok() || !is_ignored(main_root) {
        return false;
    }
    // `cp -c` uses clonefile(2) and fails, rather than copying the data, on a
    // filesystem without clone support.
    let mut copy = Command::new("cp");
    copy.arg("-cR").arg(&source).arg(&dest);
    let cloned = run_with_timeout(&mut copy, CLONE_TIMEOUT).is_some_and(|o| o.status.success());
    if !cloned {
        let _ = std::fs::remove_dir_all(&dest);
    }
    cloned
}

fn is_ignored(main_root: &Path) -> bool {
    git(
        main_root,
        &["check-ignore", "-q", &format!("{TARGET_DIR}/")],
    )
    .is_ok()
}
