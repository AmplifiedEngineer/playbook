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
/// index cache (`code/index.sqlite3`). Fails with an actionable message
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

/// The code index of the current worktree inside the data folder, after
/// moving the one earlier versions kept under the worktree state folder. A
/// failed move is not an error: the index is a cache and is rebuilt. A repo
/// whose name has no safe data folder path keeps its index at the old path.
pub fn index_db() -> Result<PathBuf, String> {
    let old = state_dir()?.join("code").join("index.sqlite3");
    let Some(new) = playbook_data::scoped_db("code") else {
        return Ok(old);
    };
    let step = playbook_data::migrate::Move {
        old,
        new: new.clone(),
        policy: playbook_data::migrate::Policy::Cache,
    };
    let _ = playbook_data::migrate::migrate_with_notice(&step, "code index");
    Ok(new)
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

/// What the language server layer reads from playbook's config. The program
/// resolves these from its config store and passes them in, so this crate
/// never reads the store. The default is the safe one: untrusted, no paths.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LspSettings {
    /// `code.lsp.trustWorkspace`: whether servers may run in this repo.
    pub trust_workspace: bool,
    /// The config tier that supplied the trust value; `None` for the default.
    pub trust_tier: Option<String>,
    /// `code.lsp.<id>.path` values that are not empty, by server id.
    pub server_paths: Vec<(String, PathBuf)>,
    /// `code.python.interpreter`, when set.
    pub python_interpreter: Option<PathBuf>,
}

impl LspSettings {
    /// The configured path for server `id`, if any.
    pub fn server_path(&self, id: &str) -> Option<&Path> {
        self.server_paths
            .iter()
            .find(|(name, _)| name == id)
            .map(|(_, path)| path.as_path())
    }
}
