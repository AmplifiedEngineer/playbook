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

/// Whether this platform can clone a folder copy-on-write.
pub(crate) const CLONES_TARGET: bool = cfg!(target_os = "macos");
/// The build folder Cargo uses unless told otherwise.
const TARGET_DIR: &str = "target";
/// A clone is metadata only, so even a large `target` finishes well within this.
const CLONE_TIMEOUT: Duration = Duration::from_secs(120);

/// Clones `<main_root>/target` to `<worktree>/target` when the main checkout
/// has a real `target` folder (not a symlink), the worktree's checkout ignores
/// it, the worktree has none, and the platform and filesystem support clones. Returns whether a clone was made.
pub(crate) fn clone_warm_target(main_root: &Path, worktree: &Path) -> bool {
    if !CLONES_TARGET {
        return false;
    }
    let source = main_root.join(TARGET_DIR);
    let dest = worktree.join(TARGET_DIR);
    // An untracked clone would make the new worktree look dirty, and
    // `worktree-remove` keeps a dirty worktree. The worktree's own checkout
    // decides, because its `.gitignore` may differ from main's.
    // `cp -R` copies a symlink as a link, which would point the worktree's
    // builds at the main checkout's artifacts.
    let real_dir = source
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_dir());
    if !real_dir || dest.symlink_metadata().is_ok() || !is_ignored(worktree) {
        return false;
    }
    // `cp -c` uses clonefile(2) and fails, rather than copying the data, on a
    // filesystem without clone support. It can leave empty directories behind,
    // so a failure removes the whole destination below.
    let mut copy = Command::new("cp");
    copy.arg("-cR").arg(&source).arg(&dest);
    let cloned = run_with_timeout(&mut copy, CLONE_TIMEOUT).is_some_and(|o| o.status.success());
    if !cloned {
        let _ = std::fs::remove_dir_all(&dest);
    }
    cloned
}

fn is_ignored(dir: &Path) -> bool {
    git(dir, &["check-ignore", "-q", &format!("{TARGET_DIR}/")]).is_ok()
}
