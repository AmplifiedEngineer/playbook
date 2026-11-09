// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The central invariant a `settings.json` command must satisfy: it
//! RESOLVES. Two defects shipped, or nearly shipped, while every existing
//! test stayed green, because every one of them asserted only what was
//! WRITTEN to `settings.json`, never that the written thing exists and
//! actually runs:
//!
//! - `playbook setup` once wrote four `~/.claude/hooks/<name>.sh` guard
//!   commands into `settings.json` while placing none of the scripts.
//! - `settings.shared.json` once seeded those same commands directly,
//!   bypassing the gate meant to stop exactly that.
//!
//! A `settings.json` command naming a missing script fails OPEN and silent:
//! Claude Code invokes it, the file is not there, nothing runs, nothing is
//! reported. This module runs a real `playbook setup` against a scratch
//! `$HOME`, walks EVERY command anywhere under the resulting `.hooks`, and
//! asserts each one resolves:
//! - a bare `playbook hook <name>` command: `<name>` must be a hook the
//!   compiled binary actually accepts, derived from the binary's own
//!   `playbook hook --help` output rather than a hardcoded list here, so a
//!   hook added or renamed in `HookName` without ever being wired to match is
//!   caught the same way a user's shell would catch it: the command runs and
//!   clap rejects it.
//! - any command naming a filesystem path: that path must exist on disk and
//!   be executable.
//! - anything in neither shape fails the test loudly rather than being
//!   skipped, since an unrecognised command shape is exactly how something
//!   slips through unresolved.
//!
//! The walk itself is asserted non-vacuous: a walk that silently visits zero
//! commands would pass every assertion below trivially, which is the same
//! class of bug as the two defects this module exists to catch.

#[path = "support/isolated.rs"]
mod isolated;

use serde_json::Value;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// The repo checkout root, standing in for `CLAUDE_PLUGIN_ROOT` on a real
/// install, matching `tests/init_run.rs::checkout_root`.
fn checkout_root() -> PathBuf {
    PathBuf::from(env!("PLAYBOOK_REPO_ROOT"))
}

static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh scratch directory under the OS temp dir, standing in for `$HOME`,
/// unique per call so parallel tests never collide and none of them ever
/// touch a real `~/.claude`.
fn scratch_home(tag: &str) -> PathBuf {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
    let home = env::temp_dir().join(format!(
        "playbook-init-hooks-resolve-{}-{tag}-{n}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&home).expect("scratch home should be creatable");
    home
}

/// Every `command` string found anywhere under `.hooks`, walking every
/// event, every group, and every hook entry. Mirrors `tests/init_wire.rs`'s
/// and `tests/init_run.rs`'s helper of the same name; duplicated rather than
/// shared, since integration test binaries in this crate each compile
/// standalone.
fn all_hook_commands(settings: &Value) -> Vec<String> {
    let mut commands = Vec::new();
    let Some(hooks) = settings.get("hooks").and_then(Value::as_object) else {
        return commands;
    };
    for groups in hooks.values() {
        let Some(groups) = groups.as_array() else {
            continue;
        };
        for group in groups {
            let Some(entries) = group.get("hooks").and_then(Value::as_array) else {
                continue;
            };
            for entry in entries {
                if let Some(cmd) = entry.get("command").and_then(Value::as_str) {
                    commands.push(cmd.to_string());
                }
            }
        }
    }
    commands
}

/// Spawns the real compiled binary's `hook --help` and parses clap's own
/// `[possible values: ...]` line into the exact set of hook names the
/// binary accepts today. Deliberately not read from `HookName` directly:
/// parsing the same text a user's terminal would show is what catches a
/// hook renamed in `HookName` (`src/lib.rs`) without its `wire.rs` spec
/// following, which importing the enum in-process would not, since both
/// would still agree with each other while disagreeing with what `wire`
/// actually wrote.
fn accepted_hook_names() -> Vec<String> {
    let out = isolated::playbook()
        .env_remove("CI")
        .env_remove("PLAYBOOK_HEADLESS")
        .args(["hook", "--help"])
        .output()
        .expect("playbook binary should spawn");
    let help = String::from_utf8_lossy(&out.stdout);
    let marker = "possible values: ";
    let start = help.find(marker).unwrap_or_else(|| {
        panic!("'playbook hook --help' printed no possible-values list: {help}")
    }) + marker.len();
    let rest = &help[start..];
    let end = rest.find(']').unwrap_or_else(|| {
        panic!("'playbook hook --help' possible-values list has no closing ']': {help}")
    });
    rest[..end]
        .split(", ")
        .map(str::trim)
        .map(str::to_string)
        .collect()
}

/// Whether `token`, the first whitespace-separated word of a `.hooks`
/// `command` string, names a filesystem path rather than a bare
/// `PATH`-resolved executable name: it carries a path separator, or starts
/// with `~` for home-relative expansion, the two forms every legacy hook or
/// guard command in this repo has ever taken.
fn looks_like_a_path(token: &str) -> bool {
    token.starts_with('~') || token.contains('/') || token.contains('\\')
}

/// `token` with a leading `~/` (or a bare `~`) expanded against `home`, the
/// same expansion a shell performs before actually invoking a command shaped
/// like this.
fn expand_tilde(token: &str, home: &Path) -> PathBuf {
    match token.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if token == "~" => home.to_path_buf(),
        None => PathBuf::from(token),
    }
}

/// Whether `path` is a regular file with some execute bit set, the same
/// check `init::guards::verify_executable` performs before trusting a
/// placed guard.
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Regression pin for both defects at once, since they share one root
/// cause: a `settings.json` command naming something that does not resolve.
#[test]
fn every_wired_command_after_a_real_init_resolves_to_something_that_actually_runs() {
    // Arrange: the accepted hook-name set, read from the binary itself, not
    // from a list hand-copied into this test.
    let accepted = accepted_hook_names();
    let home = scratch_home("resolves");

    // Act: a real `playbook setup` against a clean scratch HOME, the exact
    // machine shape both defects reached production on.
    let status = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .env_remove("CI")
        .env_remove("PLAYBOOK_HEADLESS")
        .args(["setup", "--yes"])
        .env("PLAYBOOK_SKIP_PLUGIN_UPDATE", "1")
        .env("HOME", &home)
        .env("CLAUDE_PLUGIN_ROOT", checkout_root())
        .env("SHELL", "/bin/bash")
        .status()
        .expect("playbook binary should spawn");
    assert!(
        status.success(),
        "playbook setup should exit 0 against a clean scratch HOME"
    );

    let settings_path = home.join(".claude").join("settings.json");
    let settings: Value = serde_json::from_str(
        &fs::read_to_string(&settings_path)
            .unwrap_or_else(|err| panic!("settings.json should exist after init: {err}")),
    )
    .expect("settings.json should be valid JSON");
    let commands = all_hook_commands(&settings);

    // Assert: the walk itself is not vacuous.
    assert!(
        !commands.is_empty(),
        "a real `playbook setup` should have written at least one .hooks command"
    );

    for cmd in &commands {
        let first_token = cmd.split_whitespace().next().unwrap_or(cmd);

        if let Some(rest) = cmd.strip_prefix("playbook hook ") {
            let name = rest.trim();
            assert!(
                accepted.iter().any(|n| n == name),
                "'{cmd}' names hook '{name}', which 'playbook hook --help' does not \
                 list as accepted today: {accepted:?}"
            );
            continue;
        }

        if looks_like_a_path(first_token) {
            let resolved = expand_tilde(first_token, &home);
            assert!(
                resolved.is_file(),
                "'{cmd}' names a script at {resolved:?} that does not exist on disk; \
                 a settings.json command naming a missing script fails open and silent"
            );
            assert!(
                is_executable(&resolved),
                "'{cmd}' names a script at {resolved:?} that exists but is not executable"
            );
            continue;
        }

        panic!(
            "'{cmd}' is neither a bare 'playbook hook <name>' invocation nor a \
             filesystem path; an unrecognised command shape is exactly how a hook \
             can slip through wired but unresolvable, so this must fail loudly \
             rather than being skipped"
        );
    }
}
