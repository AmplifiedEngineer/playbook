// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! One module per hook name, matching the `HookName` set exactly.
//! Each module owns its hook's behaviour, and `dispatch` below is exhaustive
//! over `HookName`.

use crate::common::payload::Payload;
use crate::HookName;

pub mod auto_cost;
pub mod auto_guard;
pub mod auto_model_detect;
pub mod bg_await_guard;
pub mod commit_message_sanitizer;
pub mod context_hygiene;
pub mod doc_read_hint;
pub mod memory_anchors;
pub mod memory_capture;
pub mod memory_signals;
pub mod policy_guard;
pub mod post_edit_track;
pub mod precommit_check;
pub mod precompact_warn;
pub mod preread_edit_check;
pub mod preread_reread;
pub mod preread_size_check;
pub mod rebuild_memory_graph;
pub mod rm_workspace_guard;
pub mod search_counter;
pub mod session_activity;
pub mod session_clean_exit;
pub mod session_init;
pub mod staleness;
pub mod usage_warm;
pub mod worktree_create;
pub mod worktree_remove;
pub mod worktree_setup;
pub mod worktree_target;

/// `git rev-parse --show-toplevel`, trimmed. Empty outside a repo or on any
/// failure. Never panics.
pub(crate) fn git_toplevel() -> String {
    if let Some(top) = std::env::current_dir()
        .ok()
        .and_then(|cwd| crate::common::gitfacts::toplevel(&cwd))
    {
        return top.to_string_lossy().into_owned();
    }
    let args = ["--no-optional-locks", "rev-parse", "--show-toplevel"];
    crate::common::git::trimmed(crate::common::git::run(
        None,
        &args,
        crate::common::git::TIMEOUT,
    ))
    .unwrap_or_default()
}

/// True when the token begins a command: first overall, or preceded by a
/// separator. Without this, `tsc` appearing as an argument would match.
pub(crate) fn starts_a_command(tokens: &[&str], i: usize) -> bool {
    match i.checked_sub(1).and_then(|p| tokens.get(p)) {
        None => true,
        Some(prev) => prev.ends_with([';', '&', '|', '(']),
    }
}

/// Dispatch a parsed hook payload to the named hook's entry point.
/// Exhaustive over `HookName`, so adding a new variant fails the build here
/// instead of silently doing nothing at runtime.
pub fn dispatch(name: HookName, payload: &Payload) {
    match name {
        HookName::SessionInit => session_init::run(payload),
        HookName::PrereadEditCheck => preread_edit_check::run(payload),
        HookName::PrereadSizeCheck => preread_size_check::run(payload),
        HookName::SearchCounter => search_counter::run(payload),
        HookName::MemoryAnchors => memory_anchors::run(payload),
        HookName::PostEditTrack => post_edit_track::run(payload),
        HookName::RebuildMemoryGraph => rebuild_memory_graph::run(payload),
        HookName::AutoModelDetect => auto_model_detect::run(payload),
        HookName::PrecompactWarn => precompact_warn::run(payload),
        HookName::SessionActivity => session_activity::run(payload),
        HookName::SessionCleanExit => session_clean_exit::run(payload),
        HookName::MemoryCapture => memory_capture::run(payload),
        HookName::RmWorkspaceGuard => rm_workspace_guard::run(payload),
        HookName::BgAwaitGuard => bg_await_guard::run(payload),
        // Retired: allow, so an old settings entry stays quiet.
        HookName::NoSlopGuard => {}
        HookName::CommitMessageSanitizer => commit_message_sanitizer::run(payload),
        HookName::PolicyGuard => policy_guard::run(payload),
        HookName::PrecommitCheck => precommit_check::run(payload),
        HookName::AutoGuard => auto_guard::run(payload),
        HookName::AutoCost => auto_cost::run(payload),
        HookName::WorktreeCreate => worktree_create::run(payload),
        HookName::WorktreeRemove => worktree_remove::run(payload),
        HookName::ContextHygiene => context_hygiene::run(payload),
        HookName::DocReadHint => doc_read_hint::run(payload),
        HookName::UsageWarm => usage_warm::run(payload),
        // Retired. Old plugin caches and settings.json entries still call
        // these; succeed without output.
        HookName::MigrationCheck | HookName::DiskGuard => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::ValueEnum;
    use std::panic;

    /// Every hook name must survive malformed input without panicking, so a
    /// bug in one hook can never break the PreToolUse hot path. Table-driven
    /// over every declared `HookName` and every malformed-input shape, so a
    /// future variant cannot escape the check.
    #[test]
    fn every_hook_survives_malformed_stdin() {
        // The hooks write session state under `$HOME/.config/playbook`, so
        // this runs in a child with a scratch home, never the real one.
        if !crate::testing::in_scratch_home("hooks::tests::every_hook_survives_malformed_stdin") {
            return;
        }
        // Arrange
        let raw_inputs: [(&str, &str); 4] = [
            ("truncated JSON", r#"{"session_id":"abc","tool_input":{"#),
            ("empty input", ""),
            (
                "valid JSON missing expected field",
                r#"{"unexpected":"value"}"#,
            ),
            ("valid JSON that is not an object", "[1,2,3]"),
        ];
        let mut failures = Vec::new();

        for name in HookName::value_variants() {
            let name = *name;
            for (label, raw) in raw_inputs {
                // Act
                let result = panic::catch_unwind(|| {
                    let payload = Payload::parse(raw);
                    dispatch(name, &payload);
                });

                // Assert (collected, so one failure does not hide the rest)
                if result.is_err() {
                    failures.push(format!("{name:?} panicked on {label}"));
                }
            }
        }

        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// The guard behind the scratch home: it refuses a `$HOME` that is the
    /// account's real home. `in_scratch_home` runs it in its child, so a hook
    /// test built on that helper fails instead of writing there.
    #[test]
    fn the_scratch_home_guard_refuses_the_real_home() {
        // Arrange: whatever `$HOME` is here counts as the account's home.
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let Some(home) = home.filter(|h| h.exists()) else {
            return;
        };
        let _pretend = crate::common::real_home::pretend_account_home(&home);

        // Act
        let result = panic::catch_unwind(crate::testing::assert_home_is_scratch);

        // Assert
        assert!(result.is_err(), "the guard must refuse {}", home.display());
    }
}
