// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The bridge from the code index to the playbook program. What the index
//! needs from playbook (its name, where it keeps state, repo identity, file
//! privacy and, in later Segments, config values and process policy) goes
//! through this module. Shared helpers come from `playbook_common`.

use std::path::{Path, PathBuf};

/// The program name used in user facing messages.
pub const PROGRAM: &str = "playbook";

/// This tool's version, stored in the index metadata.
pub fn tool_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The state folder for the current worktree, where earlier versions kept the
/// index cache (`code/index.sqlite3`) and where the fallback index lives. Fails with an actionable message
/// when the repo has no way to name itself.
pub fn state_dir() -> Result<PathBuf, String> {
    playbook_common::paths::repo_scoped_dir(playbook_common::paths::RepoScope::Worktree).ok_or_else(
        || {
            "could not resolve where to keep the code index; this repo needs a git 'origin' \
             remote and a resolvable worktree root"
                .to_string()
        },
    )
}

/// The file name of the index at the state folder fallback path.
pub const FALLBACK_FILE: &str = "index.sqlite3";

/// The repo's shared store inside the data folder. A repo whose name has no
/// safe data folder path (`scoped` is `None`) keeps one index per worktree at
/// the state folder path `old`.
pub fn choose_index_path(scoped: Option<PathBuf>, old: PathBuf) -> PathBuf {
    scoped.unwrap_or(old)
}

/// The code index the current worktree writes to: the repo's shared store, or
/// the state folder fallback. An index file kept by an earlier version stays
/// where it is.
pub fn index_db() -> Result<PathBuf, String> {
    let old = state_dir()?.join("code").join(FALLBACK_FILE);
    let scoped = playbook_data::code_store(super::db::SCHEMA_VERSION);
    Ok(choose_index_path(scoped, old))
}

/// A stable identity for the current worktree.
pub fn worktree_id() -> String {
    playbook_common::paths::worktree_id()
}

/// Creates `dir` and its parents, owner only on unix.
pub fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    playbook_common::atomic::ensure_private_dir(dir)
}

/// Creates the file owner only on unix before SQLite opens it, and tightens a
/// wider existing file.
pub fn make_file_private(path: &Path) -> Result<(), String> {
    playbook_common::atomic::make_db_file_private(path)
}

/// The command line a user runs for `args`, for error messages.
pub fn command(args: &str) -> String {
    format!("{PROGRAM} code {args}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scoped_path_wins_over_the_state_folder_path() {
        // Arrange
        let scoped = PathBuf::from("/data/code/o/r/store-v4.sqlite3");
        let old = PathBuf::from("/state/code/index.sqlite3");

        // Act
        let chosen = choose_index_path(Some(scoped.clone()), old);

        // Assert
        assert_eq!(chosen, scoped);
    }

    #[test]
    fn without_a_scoped_path_the_state_folder_path_is_used() {
        // Arrange
        let old = PathBuf::from("/state/code/index.sqlite3");

        // Act
        let chosen = choose_index_path(None, old.clone());

        // Assert
        assert_eq!(chosen, old);
    }
}
