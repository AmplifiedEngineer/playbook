// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Registers the playbook guards that work in Codex in `~/.codex/hooks.json`.
//! Codex reads the same `{"hooks": {Event: [{matcher, hooks: [...]}]}}` shape
//! as Claude Code, and the Bash guards read the same stdin fields and print the
//! same output shapes, so the entries are written by the same upsert as
//! `settings.json`.
//!
//! Only `rm-workspace-guard` and `precommit-check` are registered. The
//! sanitizer is left out until a live Codex confirms that its rewrite is
//! applied and that Codex still asks to approve the rewritten command. No entry
//! carries an `if` field, because Codex lists none: `precommit-check` runs on
//! every Bash call and stays quiet unless the command is a commit. Codex skips a
//! hook until the user trusts its exact definition in `/hooks`.

use super::wire::{
    audit_specs, load_settings, remove_managed_entries, upsert_specs, write_with_fresh_backup,
    Group, HookAudit, HookSpec, WireError, WireOutcome,
};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The guards registered in Codex, kept apart from `wired_hooks()` because the
/// hook reference and `playbook hooks` describe the Claude Code set.
pub const CODEX_HOOK_SPECS: &[HookSpec] = &[
    HookSpec {
        event: "PreToolUse",
        matcher: Some("Bash"),
        name: "rm-workspace-guard",
        group: Group::Safety,
        purpose: "Stops an `rm` whose target is the filesystem root or your home folder.",
        if_cond: None,
        timeout: Some(10),
        also_replaces: None,
    },
    HookSpec {
        event: "PreToolUse",
        matcher: Some("Bash"),
        name: "precommit-check",
        group: Group::Safety,
        purpose: "Warns about debug leftovers, secret-shaped file names and oversized commits in the staged diff.",
        if_cond: None,
        timeout: Some(10),
        also_replaces: None,
    },
];

/// `hooks.json` under the Codex home directory.
pub fn hooks_path(home: &Path) -> PathBuf {
    home.join(".codex").join("hooks.json")
}

fn specs() -> Vec<&'static HookSpec> {
    CODEX_HOOK_SPECS.iter().collect()
}

fn managed() -> Vec<String> {
    CODEX_HOOK_SPECS
        .iter()
        .map(|s| super::wire::bare_command(s.name))
        .collect()
}

/// Writes the guards to `hooks_path`, keeping every entry that is not ours.
/// Idempotent, and backs the file up first when it changes.
pub fn wire(hooks_path: &Path, epoch: u64) -> Result<WireOutcome, WireError> {
    upsert_specs(hooks_path, epoch, &specs())
}

/// Removes the guards `wire` wrote and leaves every other key and entry.
/// Returns how many entries went. `dry_run` only counts.
pub fn unwire(hooks_path: &Path, epoch: u64, dry_run: bool) -> Result<usize, WireError> {
    let (mut root, original) = load_settings(hooks_path)?;
    let removed = remove_managed_entries(&mut root, &managed());
    if removed == 0 || dry_run {
        return Ok(removed);
    }
    let rendered = format!("{:#}\n", Value::Object(root));
    if rendered != original {
        write_with_fresh_backup(hooks_path, &rendered, epoch)?;
    }
    Ok(removed)
}

/// What a read-only look at `hooks_path` found about the guards `wire` owns.
pub fn audit(hooks_path: &Path) -> HookAudit {
    audit_specs(hooks_path, &specs())
}
