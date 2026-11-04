// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook gate` subcommand family, backed by a local SQLite database under
//! `~/.config/playbook`, scoped per repo and worktree. This module holds the schema and connection layer
//! (`db`), the `gate record` CLI entry point (`record`), the
//! `gate check` CLI entry point (`check`), shared content hashing
//! (`hash`), and the gate source file names plus `gate snapshot` (`snapshot`).

pub mod check;
pub mod db;
pub mod hash;
pub mod record;
pub mod snapshot;

use crate::config::keys::GATE_MAX_ROUNDS_DEFAULT;
use std::path::Path;

/// The configured `gate.maxRounds`, or the default when it is unset, unreadable
/// or out of range.
pub fn max_rounds(home: &Path, repo_slug: Option<&str>) -> i64 {
    crate::config::resolve("gate.maxRounds", home, repo_slug)
        .ok()
        .and_then(|(v, _)| v.as_i64())
        .filter(|n| (1..=crate::config::keys::GATE_MAX_ROUNDS_CEILING).contains(n))
        .unwrap_or(GATE_MAX_ROUNDS_DEFAULT)
}
