// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for `playbook::init::wire`, the module that writes
//! every hook Claude Code can invoke into `settings.json` as a bare
//! `playbook hook <name>` command, retiring `hooks/hooks.json`. Every entry
//! `wire` manages, hooks and guards alike, is upserted unconditionally. Only
//! a command that is exactly `playbook hook <name>` is ours: a user's own
//! script command is never rewritten.
//!
//! **Test isolation:** every test here operates on a fresh scratch directory
//! under the OS temp dir (`scratch_settings_path`), never on a real
//! `~/.claude/settings.json`. A test that wrote a developer's live settings
//! file would be a defect regardless of whether it passed.
//!
//! Coverage map:
//! - Idempotence (assert on file bytes, not a return value):
//!   `running_wire_twice_writes_nothing_the_second_time`
//! - Every written command for a ported hook resolves to a real `HookName`,
//!   and is a bare name rather than a path (the "bare-name assumption"),
//!   and all 26 ported hooks (the 20 functional hooks plus the 5 safety
//!   guards) are exactly the ones wired that way:
//!   `every_ported_hook_command_is_a_bare_playbook_hook_invocation_that_resolves`
//! - A pre-existing user hook entry survives wiring, unclobbered:
//!   `pre_existing_user_hook_entry_is_preserved_not_clobbered`
//! - Every guard gets its bare command, and a user's script command under
//!   `~/.claude/hooks/` stays:
//!   `every_guard_gets_its_bare_command_and_a_user_script_command_stays`
//! - The bare-name form survives a write-then-read round trip:
//!   `bare_name_form_survives_write_then_read_round_trip`
//! - Backup is timestamped and taken only when a change actually lands:
//!   `settings_json_is_backed_up_before_a_real_change_and_not_on_a_no_op`
//! - Regression pin for the defect itself: every hook name wired in binary
//!   form has a real (non-stub) Rust implementation behind it:
//!   `every_hook_wired_in_binary_form_has_a_non_stub_implementation`
//! - A user command named like a guard script (`<name>.sh`, `<name>.py`) is
//!   kept, and the bare form is added next to it:
//!   `a_user_command_named_like_a_guard_script_is_kept_and_the_bare_form_is_added`

use clap::ValueEnum;
use playbook::init::wire::{wire, wire_at};
use playbook::HookName;
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh scratch directory under the OS temp dir, unique per call so
/// parallel tests never collide and none of them ever touch a real
/// `~/.claude/settings.json`.
fn scratch_dir(tag: &str) -> PathBuf {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = env::temp_dir().join(format!(
        "playbook-init-wire-{}-{tag}-{n}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&dir).expect("scratch dir should be creatable");
    dir
}

/// A `settings.json` path inside a fresh scratch directory; the file itself
/// is not created here, callers write it (or leave it absent to exercise
/// the fresh-install path).
fn scratch_settings_path(tag: &str) -> PathBuf {
    scratch_dir(tag).join("settings.json")
}

fn write_json(path: &PathBuf, value: &Value) {
    fs::write(path, serde_json::to_string_pretty(value).unwrap())
        .expect("scratch settings.json should be writable");
}

fn read_json(path: &PathBuf) -> Value {
    let text = fs::read_to_string(path).expect("wired settings.json should be readable");
    serde_json::from_str(&text).expect("wired settings.json should be valid JSON")
}

/// Every `command` string found anywhere under `.hooks` in `settings`,
/// walking every event, every group, and every hook entry.
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

/// A `settings.json` with four guards wired directly with script paths under
/// `~/.claude/hooks/*.sh`, which `wire` treats as the user's own commands, and
/// none of the functional hooks, which `wire` adds.
fn unwired_fixture() -> Value {
    json!({
        "$schema": "https://json.schemastore.org/claude-code-settings.json",
        "cleanupPeriodDays": 14,
        "permissions": {"allow": ["Read"], "deny": [], "ask": []},
        "hooks": {
            "PreToolUse": [
                {
                    "matcher": "Bash",
                    "hooks": [
                        {
                            "type": "command",
                            "command": "~/.claude/hooks/rm-workspace-guard.sh",
                            "if": "Bash(rm:*)",
                            "timeout": 10
                        },
                        {
                            "type": "command",
                            "command": "~/.claude/hooks/bg-await-guard.sh",
                            "timeout": 10
                        },
                        {
                            "type": "command",
                            "command": "~/.claude/hooks/no-dash-guard.sh",
                            "timeout": 10
                        },
                        {
                            "type": "command",
                            "command": "~/.claude/hooks/precommit-check.sh",
                            "if": "Bash(git commit:*)",
                            "timeout": 10
                        }
                    ]
                }
            ]
        },
        "skipAutoPermissionPrompt": false
    })
}

#[test]
fn running_wire_twice_writes_nothing_the_second_time() {
    // Arrange
    let path = scratch_settings_path("idempotence");
    write_json(&path, &unwired_fixture());

    // Act
    wire(&path).expect("first wire should succeed");
    let bytes_after_first = fs::read(&path).expect("settings.json should exist after wiring");
    wire(&path).expect("second wire should succeed");
    let bytes_after_second = fs::read(&path).expect("settings.json should still exist");

    // Assert: file bytes, not a return value.
    assert_eq!(
        bytes_after_first, bytes_after_second,
        "a second wire() call must not change settings.json at all"
    );
}

#[test]
fn a_backup_already_taken_under_the_same_stamp_is_kept_not_overwritten() {
    // Arrange: an earlier step of the same run already backed up the original.
    let path = scratch_settings_path("backup-kept");
    write_json(&path, &unwired_fixture());
    let backup = path.with_file_name("settings.json.bak.1700000000");
    fs::write(&backup, "pre-run original").unwrap();

    // Act
    let outcome = wire_at(&path, 1_700_000_000).expect("wire_at should succeed");

    // Assert
    assert!(outcome.changed);
    assert_eq!(outcome.backup_path.as_deref(), Some(backup.as_path()));
    assert_eq!(fs::read_to_string(&backup).unwrap(), "pre-run original");
}

#[test]
fn running_wire_twice_from_a_fresh_install_writes_nothing_the_second_time() {
    // Arrange: no settings.json exists yet at all.
    let path = scratch_settings_path("idempotence-fresh");

    // Act
    wire(&path).expect("first wire should succeed on a fresh install");
    let bytes_after_first = fs::read(&path).expect("settings.json should now exist");
    wire(&path).expect("second wire should succeed");
    let bytes_after_second = fs::read(&path).expect("settings.json should still exist");

    // Assert
    assert_eq!(
        bytes_after_first, bytes_after_second,
        "a second wire() call on a freshly wired file must be a no-op"
    );
}

#[test]
fn every_ported_hook_command_is_a_bare_playbook_hook_invocation_that_resolves() {
    // Arrange
    let path = scratch_settings_path("resolves");
    write_json(&path, &unwired_fixture());

    // Act
    wire(&path).expect("wire should succeed");
    let settings = read_json(&path);
    let commands = all_hook_commands(&settings);

    // Assert: every command that takes the bare form is bare (no path
    // separator, no absolute path, no legacy python/shell script suffix)
    // and its hook name resolves to a real HookName the same way clap would
    // parse `playbook hook <name>`. Every guard takes this form too.
    assert!(!commands.is_empty(), "wiring should write hook commands");
    let mut resolved_names = std::collections::HashSet::new();
    for cmd in &commands {
        let Some(name) = cmd.strip_prefix("playbook hook ") else {
            continue;
        };
        assert!(
            !name.contains('/') && !name.contains('\\'),
            "hook name segment should carry no path separators: {cmd}"
        );
        HookName::from_str(name, false)
            .unwrap_or_else(|_| panic!("'{name}' should resolve to a real HookName: {cmd}"));
        resolved_names.insert(name.to_string());
    }

    // Every HookName variant should be wired in binary form at least once,
    // so pin it directly rather than only checking the commands that happen
    // to be present.
    let expected: std::collections::HashSet<&str> = [
        "session-init",
        "preread-edit-check",
        "preread-size-check",
        "search-counter",
        "memory-anchors",
        "post-edit-track",
        "rebuild-memory-graph",
        "auto-model-detect",
        "auto-guard",
        "auto-cost",
        "precompact-warn",
        "session-clean-exit",
        "memory-capture",
        "context-hygiene",
        "doc-read-hint",
        "usage-warm",
        "handoff-on-clear",
        "session-activity",
        "worktree-create",
        "worktree-remove",
        "rm-workspace-guard",
        "bg-await-guard",
        "precommit-check",
        "commit-message-sanitizer",
        "policy-guard",
    ]
    .into_iter()
    .collect();
    let resolved_names: std::collections::HashSet<&str> =
        resolved_names.iter().map(String::as_str).collect();
    assert_eq!(
        resolved_names, expected,
        "wiring should register exactly all 24 ported HookName variants in binary form"
    );
}

#[test]
fn pre_existing_user_hook_entry_is_preserved_not_clobbered() {
    // Arrange: the same unwired fixture, plus a hand-added hook entry in the
    // very same PreToolUse/Bash group the guards live in, and a second
    // hand-added entry on an event/matcher pair wire() never manages at all.
    let mut fixture = unwired_fixture();
    let bash_hooks = fixture["hooks"]["PreToolUse"][0]["hooks"]
        .as_array_mut()
        .unwrap();
    bash_hooks.push(json!({
        "type": "command",
        "command": "my-custom-bash-guard.sh"
    }));
    fixture["hooks"]["Notification"] = json!([
        {
            "hooks": [
                {"type": "command", "command": "my-custom-notifier.sh"}
            ]
        }
    ]);
    let path = scratch_settings_path("preserve-user-entry");
    write_json(&path, &fixture);

    // Act
    wire(&path).expect("wire should succeed");
    let settings = read_json(&path);

    // Assert
    let bash_hooks_after = settings["hooks"]["PreToolUse"][0]["hooks"]
        .as_array()
        .unwrap();
    assert!(
        bash_hooks_after
            .iter()
            .any(|h| h["command"] == "my-custom-bash-guard.sh"),
        "a user's hand-added Bash guard hook must survive wiring: {bash_hooks_after:?}"
    );
    assert_eq!(
        settings["hooks"]["Notification"][0]["hooks"][0]["command"], "my-custom-notifier.sh",
        "an entire event wire() does not manage must survive untouched"
    );
}

#[test]
fn every_guard_gets_its_bare_command_and_a_user_script_command_stays() {
    // Arrange: a fixture whose guards are the user's own scripts under
    // ~/.claude/hooks/.
    let path = scratch_settings_path("guard-bare-and-user-script");
    write_json(&path, &unwired_fixture());

    // Act
    wire(&path).expect("wire should succeed");
    let settings = read_json(&path);
    let commands = all_hook_commands(&settings);

    // Assert: the user's own script commands stay, and every guard is wired in
    // its bare form next to them.
    assert!(commands.contains(&"~/.claude/hooks/rm-workspace-guard.sh".to_string()));
    for name in [
        "rm-workspace-guard",
        "bg-await-guard",
        "precommit-check",
        "commit-message-sanitizer",
        "policy-guard",
    ] {
        assert!(
            commands.contains(&format!("playbook hook {name}")),
            "guard '{name}' should be wired to its bare binary form: {commands:?}"
        );
    }
}

#[test]
fn bare_name_form_survives_write_then_read_round_trip() {
    // Arrange
    let path = scratch_settings_path("round-trip");
    write_json(&path, &unwired_fixture());

    // Act
    wire(&path).expect("wire should succeed");
    let reread = read_json(&path);

    // Assert: settings.json accepts a bare-name hook command from a
    // plugin-independent entry, the same shape a hand-written
    // "rtk hook claude" entry already takes; the write survives being read
    // back byte-for-string exactly, not just structurally.
    let session_start_command = reread["hooks"]["SessionStart"][0]["hooks"][0]["command"].clone();
    assert_eq!(
        session_start_command,
        Value::String("playbook hook session-init".to_string())
    );
}

#[test]
fn settings_json_is_backed_up_before_a_real_change_and_not_on_a_no_op() {
    // Arrange
    let dir = scratch_dir("backup");
    let path = dir.join("settings.json");
    write_json(&path, &unwired_fixture());
    let original_bytes = fs::read(&path).unwrap();

    // Act: first call changes the file, so it must back it up first.
    let first = wire(&path).expect("first wire should succeed");

    // Assert
    let backup_path = first
        .backup_path
        .expect("a real change must produce a backup path");
    assert!(
        backup_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("settings.json.bak."),
        "backup file name should be timestamped: {backup_path:?}"
    );
    let backup_bytes = fs::read(&backup_path).expect("backup file should exist on disk");
    assert_eq!(
        backup_bytes, original_bytes,
        "the backup should be a snapshot of settings.json before wiring changed it"
    );

    // Act: second call is a no-op, so it must take no further backup.
    let entries_before = fs::read_dir(&dir).unwrap().count();
    let second = wire(&path).expect("second wire should succeed");

    // Assert
    assert!(
        second.backup_path.is_none(),
        "a no-op wire() call should not report a new backup"
    );
    let entries_after = fs::read_dir(&dir).unwrap().count();
    assert_eq!(
        entries_before, entries_after,
        "a no-op wire() call should not create a second backup file"
    );
}

/// The literal body every stub hook module in `src/hooks/` starts life
/// with (it takes the parsed payload and does nothing). A module whose source
/// still contains this exact line has not been ported yet, regardless of
/// what `wire()` claims to route it to.
const STUB_HOOK_BODY: &str = "pub fn run(_payload: &Payload) {}";

/// Regression pin: wiring a hook to the bare `playbook hook <name>` binary
/// form while its Rust module is still an empty stub silently disables it
/// (a shell guard denies a dangerous command; the Rust stub prints nothing
/// and exits 0). A test asserting only the command STRING that was written
/// would not catch this, since the string is correct even when the module
/// behind it is not. This test instead reads `wire()`'s own output back and
/// checks each hook it wired in binary form against its actual Rust source,
/// with no hardcoded hook-name list to keep in sync: the moment a
/// `HookSpec` flips to `ported: true`, this test starts checking it too.
#[test]
fn every_hook_wired_in_binary_form_has_a_non_stub_implementation() {
    // Arrange: wire a fresh install, so every hook wire() currently wires
    // in binary form shows up in the output, without hardcoding which
    // those are.
    let path = scratch_settings_path("non-stub-binary-form");

    // Act
    wire(&path).expect("wire should succeed on a fresh install");
    let settings = read_json(&path);
    let commands = all_hook_commands(&settings);

    // Assert
    let manifest_dir = PathBuf::from(env!("PLAYBOOK_REPO_ROOT"));
    let mut checked = 0;
    for cmd in &commands {
        let Some(name) = cmd.strip_prefix("playbook hook ") else {
            continue; // not wired in binary form, e.g. a guard still on its .sh script
        };
        checked += 1;
        let module_path = manifest_dir
            .join("crates/cli/src/hooks")
            .join(format!("{}.rs", name.replace('-', "_")));
        let source = fs::read_to_string(&module_path).unwrap_or_else(|err| {
            panic!("hook module for '{name}' should exist at {module_path:?}: {err}")
        });
        assert!(
            !source.contains(STUB_HOOK_BODY),
            "'{name}' is wired to `playbook hook {name}` in settings.json, but \
             {module_path:?} is still the empty stub. Wiring a hook to its binary \
             form before its Rust port lands silently disables it. Either port \
             the hook first, or leave it on its legacy command until the port lands."
        );
    }
    assert!(
        checked > 0,
        "wiring a fresh install should write at least one hook in binary form"
    );
}

/// A user's own hook command is never ours, even when its script is named
/// `<hook>.sh` or `<hook>.py` in the same event and matcher group. Wiring
/// leaves it as it is and adds the bare command next to it.
#[test]
fn a_user_command_named_like_a_guard_script_is_kept_and_the_bare_form_is_added() {
    // Arrange: the fixture's four commands are the user's own scripts.
    let path = scratch_settings_path("user-script-kept");
    write_json(&path, &unwired_fixture());
    let mut with_py = read_json(&path);
    with_py["hooks"]["PreToolUse"][0]["hooks"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type": "command", "command": "\"/opt/me/hooks/policy-guard.py\""}));
    write_json(&path, &with_py);

    // Act
    wire(&path).expect("wire should succeed");
    let settings = read_json(&path);
    let commands: Vec<String> = settings["hooks"]["PreToolUse"][0]["hooks"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|h| h["command"].as_str().map(str::to_string))
        .collect();

    // Assert: every user command survives untouched, and the bare form of each
    // guard is present exactly once.
    for kept in [
        "~/.claude/hooks/rm-workspace-guard.sh",
        "~/.claude/hooks/bg-await-guard.sh",
        "~/.claude/hooks/no-dash-guard.sh",
        "~/.claude/hooks/precommit-check.sh",
        "\"/opt/me/hooks/policy-guard.py\"",
    ] {
        assert!(
            commands.iter().any(|c| c == kept),
            "{kept} was rewritten: {commands:?}"
        );
    }
    for wired in [
        "rm-workspace-guard",
        "bg-await-guard",
        "precommit-check",
        "policy-guard",
    ] {
        let bare = format!("playbook hook {wired}");
        assert_eq!(
            commands.iter().filter(|c| **c == bare).count(),
            1,
            "{bare}: {commands:?}"
        );
    }
}

/// Every `(matcher, command)` pair registered under `event`, with the matcher
/// as `None` for a group that has none.
fn event_registrations(settings: &Value, event: &str) -> Vec<(Option<String>, String)> {
    let mut found = Vec::new();
    let groups = settings["hooks"][event]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for group in groups {
        let matcher = group
            .get("matcher")
            .and_then(Value::as_str)
            .map(String::from);
        for entry in group["hooks"].as_array().cloned().unwrap_or_default() {
            if let Some(cmd) = entry.get("command").and_then(Value::as_str) {
                found.push((matcher.clone(), cmd.to_string()));
            }
        }
    }
    found
}

#[test]
fn auto_guard_is_wired_on_ask_user_question_and_on_every_prompt() {
    // Arrange
    let path = scratch_settings_path("auto-guard");
    let command = "playbook hook auto-guard";

    // Act
    wire(&path).expect("wire should succeed on a fresh install");
    let settings = read_json(&path);

    // Assert
    let pre_tool = event_registrations(&settings, "PreToolUse");
    assert!(
        pre_tool.contains(&(Some("AskUserQuestion".to_string()), command.to_string())),
        "auto-guard must sit under PreToolUse matcher AskUserQuestion: {pre_tool:?}"
    );
    let prompt = event_registrations(&settings, "UserPromptSubmit");
    assert!(
        prompt.contains(&(None, command.to_string())),
        "auto-guard must sit under UserPromptSubmit with no matcher: {prompt:?}"
    );
}

#[test]
fn commit_message_sanitizer_is_wired_on_bash_before_and_after_the_call() {
    // Arrange
    let path = scratch_settings_path("commit-message-sanitizer");
    let command = "playbook hook commit-message-sanitizer";

    // Act
    wire(&path).expect("wire should succeed on a fresh install");
    let settings = read_json(&path);

    // Assert: the rewrite before the call, the backstop after it, both on Bash
    for event in ["PreToolUse", "PostToolUse"] {
        let registrations = event_registrations(&settings, event);
        let wired: Vec<_> = registrations
            .iter()
            .filter(|(_, cmd)| cmd == command)
            .collect();
        assert_eq!(
            wired,
            vec![&(Some("Bash".to_string()), command.to_string())],
            "the sanitizer must sit under {event} matcher Bash exactly once: {registrations:?}"
        );
    }
}

#[test]
fn auto_cost_is_wired_on_every_pre_tool_use_with_no_matcher() {
    // Arrange
    let path = scratch_settings_path("auto-cost");
    let command = "playbook hook auto-cost";

    // Act
    wire(&path).expect("wire should succeed on a fresh install");
    let settings = read_json(&path);

    // Assert: exactly one registration, and it has no matcher, so it fires on
    // every tool call
    let pre_tool = event_registrations(&settings, "PreToolUse");
    let wired: Vec<_> = pre_tool.iter().filter(|(_, cmd)| cmd == command).collect();
    assert_eq!(
        wired,
        vec![&(None, command.to_string())],
        "auto-cost must sit under PreToolUse with no matcher: {pre_tool:?}"
    );
    let anywhere_else = ["UserPromptSubmit", "SessionStart", "Stop", "PostToolUse"]
        .into_iter()
        .any(|event| {
            event_registrations(&settings, event)
                .iter()
                .any(|(_, cmd)| cmd == command)
        });
    assert!(!anywhere_else, "auto-cost is a PreToolUse hook only");
}

#[test]
fn auto_guard_and_auto_cost_carry_the_ten_second_timeout_of_the_other_guards() {
    // Arrange
    let path = scratch_settings_path("auto-timeouts");

    // Act
    wire(&path).expect("wire should succeed on a fresh install");
    let settings = read_json(&path);

    // Assert
    let mut checked = 0;
    for groups in settings["hooks"]
        .as_object()
        .expect("hooks is an object")
        .values()
    {
        for group in groups.as_array().expect("event holds groups") {
            for entry in group["hooks"].as_array().expect("group holds hooks") {
                let command = entry["command"].as_str().unwrap_or_default();
                if command == "playbook hook auto-guard" || command == "playbook hook auto-cost" {
                    assert_eq!(entry["timeout"], 10, "{command} needs a timeout: {entry}");
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 3, "auto-guard on two events and auto-cost on one");
}

#[test]
fn audit_of_a_missing_or_malformed_file_reports_every_hook_missing() {
    let total = playbook::init::wire::wired_hooks().count();
    let missing = scratch_settings_path("audit-missing");
    let bad = scratch_settings_path("audit-bad");
    fs::write(&bad, "not json {").unwrap();

    for path in [&missing, &bad] {
        let found = playbook::init::wire::audit(path);
        assert_eq!(found.present, 0);
        assert!(!found.missing.is_empty() && found.missing.len() <= total);
        assert!(found.stale.is_empty());
    }
}

#[test]
fn audit_of_a_fully_wired_file_finds_nothing_missing_or_stale() {
    let path = scratch_settings_path("audit-wired");
    wire(&path).unwrap();

    let found = playbook::init::wire::audit(&path);

    assert!(found.missing.is_empty() && found.stale.is_empty());
    assert_eq!(found.present, playbook::init::wire::wired_hooks().count());
}

#[test]
fn audit_counts_a_hook_under_the_wrong_matcher_as_missing_and_names_it_once() {
    let path = scratch_settings_path("audit-matcher");
    wire(&path).unwrap();
    let mut settings = read_json(&path);
    let groups = settings["hooks"]["PreToolUse"].as_array_mut().unwrap();
    for group in groups.iter_mut() {
        if group["matcher"] == "Read" {
            group["matcher"] = json!("Other");
        }
    }
    write_json(&path, &settings);

    let found = playbook::init::wire::audit(&path);

    assert!(found.missing.contains(&"preread-edit-check"), "{found:?}");
    assert_eq!(
        found
            .missing
            .iter()
            .filter(|n| **n == "preread-edit-check")
            .count(),
        1
    );
}

#[test]
fn audit_dedupes_a_stale_entry_that_appears_twice() {
    let path = scratch_settings_path("audit-stale");
    wire(&path).unwrap();
    let mut settings = read_json(&path);
    let stale = json!({"hooks":[{"type":"command","command":"playbook hook retired-hook"}]});
    let stop = settings["hooks"]["Stop"].as_array_mut().unwrap();
    stop.push(stale.clone());
    stop.push(stale);
    write_json(&path, &settings);

    let found = playbook::init::wire::audit(&path);

    assert_eq!(found.stale, vec!["playbook hook retired-hook".to_string()]);
}
