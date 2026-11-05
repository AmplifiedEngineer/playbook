// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for `playbook::init::run`, the module that composes
//! `merge`, `wire` and `statusline` into `Command::Init`. Coverage
//! map:
//! - a fresh config gets fully wired: `fresh_config_gets_fully_wired`
//! - running twice is idempotent, no second-run changes:
//!   `running_init_twice_is_idempotent_with_no_second_run_changes`
//! - a hand-added user hook entry survives: `hand_added_hook_entry_survives_composed_init`
//! - a malformed `settings.json` fails cleanly, never panics, and never
//!   writes: `malformed_settings_json_fails_without_panicking_or_writing`
//!   (library level) and `binary_malformed_settings_json_exits_non_zero`
//!   (the real compiled binary, since the exit code itself is `main.rs`'s
//!   contract, not `run`'s)
//! - the reported summary matches what changed on disk: asserted inline in
//!   every test above via the returned `StepStatus`, plus
//!   `missing_self_root_skips_template_dependent_steps` for a step that is
//!   legitimately skipped rather than wired or failed
//! - regression pin: a full composed `init` on a clean scratch HOME must
//!   never write a guard command
//!   in its old `~/.claude/hooks/<name>.sh` path form, even when that form
//!   would itself resolve (a guard's script still ships in this repo, so a
//!   regression here would not be caught by "does the path exist"; only
//!   "does the guard still take path form at all" catches it):
//!   `zero_hook_commands_point_under_claude_hooks_dir_after_a_full_init`
//! - the backup scheme and skip-report:
//!   a withheld customisation produces a readable skip-report alongside the
//!   backup:
//!   `withheld_customisation_produces_a_skip_report_alongside_the_backup`;
//!   stale backup and skip-report files beyond the retain-5 threshold, at
//!   and past the boundary, are pruned by one real write:
//!   `stale_backup_and_skip_report_files_beyond_five_are_pruned_after_a_real_merge`;
//!   an idempotent re-run creates no new file in either family and prunes
//!   nothing:
//!   `idempotent_rerun_creates_no_backup_or_skip_report_and_prunes_nothing`;
//!   a fresh install's placeholder-then-merge sequence produces exactly one
//!   backup and no skip-report:
//!   `fresh_install_placeholder_produces_exactly_one_backup_and_no_skip_report`
//!
//! Every test uses a scratch directory standing in for `$HOME`; none read or
//! write the developer's real `~/.claude`.

use playbook::init::run::{run, InitOutcome, InitPaths, StepReport, StepStatus};
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// The checkout's `seed/` folder, where `settings.shared.json` lives: the
/// seed root `init::run::run` reads.
fn self_root() -> PathBuf {
    PathBuf::from(env!("PLAYBOOK_REPO_ROOT")).join("seed")
}

/// The repo checkout root, standing in for `CLAUDE_PLUGIN_ROOT` on a real
/// install. Setup finds the seed in its `seed/` folder.
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
        "playbook-init-run-{}-{tag}-{n}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&home).expect("scratch home should be creatable");
    home
}

fn claude_home_of(home: &Path) -> PathBuf {
    home.join(".claude")
}

fn write_json(path: &Path, value: &Value) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent dir should be creatable");
    }
    fs::write(path, serde_json::to_string_pretty(value).unwrap())
        .expect("scratch settings.json should be writable");
}

fn write_text(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent dir should be creatable");
    }
    fs::write(path, content).expect("scratch file should be writable");
}

fn read_json(path: &Path) -> Value {
    let text = fs::read_to_string(path).expect("settings.json should be readable");
    serde_json::from_str(&text).expect("settings.json should be valid JSON")
}

/// Every `command` string found anywhere under `.hooks`, walking every
/// event, every group, and every hook entry. Mirrors `tests/init_wire.rs`'s
/// helper of the same name; duplicated rather than shared, since integration
/// test binaries in this crate each compile standalone.
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

/// The 19 functional hooks `wire` writes as a bare `playbook hook <name>` invocation.
const PORTED_HOOK_NAMES: &[&str] = &[
    "session-init",
    "preread-edit-check",
    "preread-size-check",
    "search-counter",
    "memory-anchors",
    "post-edit-track",
    "rebuild-memory-graph",
    "auto-model-detect",
    "precompact-warn",
    "session-clean-exit",
    "memory-capture",
    "context-hygiene",
    "doc-read-hint",
    "usage-warm",
    "session-activity",
    "auto-guard",
    "auto-cost",
    "worktree-create",
    "worktree-remove",
];

/// The 5 safety guards, wired the same as `PORTED_HOOK_NAMES`.
const GUARD_HOOK_NAMES: &[&str] = &[
    "rm-workspace-guard",
    "bg-await-guard",
    "precommit-check",
    "commit-message-sanitizer",
    "policy-guard",
];

/// File names directly under `dir` that start with `prefix`, for asserting
/// on the backup and skip-report families (`settings.json.bak.` and
/// `settings-merge-skipped.`) without depending on directory iteration
/// order.
fn matching_entries(dir: &Path, prefix: &str) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(prefix))
        .collect()
}

fn find_step<'a>(outcome: &'a InitOutcome, name: &str) -> &'a StepReport {
    outcome
        .steps
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| {
            panic!(
                "no '{name}' step in {:?}",
                outcome.steps.iter().map(|s| s.name).collect::<Vec<_>>()
            )
        })
}

fn base_paths(home: &Path) -> InitPaths {
    InitPaths {
        plugin_root: None,
        self_root: Some(self_root()),
        claude_home: claude_home_of(home),
        home: home.to_path_buf(),
        system_prompt: playbook::init::system_prompt::Answer::Unasked,
        hooks: true,
        statusline: true,
        security: false,
        telemetry: false,
        telemetry_replace_off: false,
        only: false,
        short_name: playbook::init::pb_link::Choice::Skip,
        policy_guard: false,
        codex_hooks: false,
        pb_target: None,
        completions: None,
        attribution: playbook::init::attribution::Choice::Off { replace: false },
        path_setup: None,
        repo: None,
    }
}

#[test]
fn fresh_config_gets_fully_wired() {
    // Arrange: a machine with no `~/.claude` at all.
    let home = scratch_home("fresh");
    let claude_home = claude_home_of(&home);
    let paths = base_paths(&home);

    // Act
    let outcome = run(&paths);

    // Assert: every step ran and actually changed something.
    assert!(
        outcome.ok(),
        "expected every step to succeed: {:?}",
        outcome
            .steps
            .iter()
            .map(StepReport::render)
            .collect::<Vec<_>>()
    );
    for step in &outcome.steps {
        // `system-prompt` is opt-in: `system_prompt: false` and no
        // pre-existing file correctly reports `Skipped` rather than
        // `Wired`, per `init::system_prompt`'s documented opt-in rules.
        // `tests/init_system_prompt.rs` covers this step's own scenarios.
        if step.name == "system-prompt" {
            assert_eq!(
                step.status,
                StepStatus::Skipped,
                "expected 'system-prompt' to be skipped without --system-prompt and no existing file: {}",
                step.detail
            );
            continue;
        }
        // `memory` has nothing to migrate on a fresh machine with no
        // legacy `~/.claude/memory`, and `auto budget` no stored budget.
        if [
            "memory",
            "auto budget",
            "trust",
            "path",
            "telemetry",
            "short-name",
            "completions",
            "policy-guard",
            "codex-hooks",
            "plugin update mode",
            "policy guard",
            "rm guard",
        ]
        .contains(&step.name)
        {
            assert_eq!(
                step.status,
                StepStatus::Skipped,
                "expected '{}' to be skipped with no legacy memory or ~/.claude.json: {}",
                step.name,
                step.detail
            );
            continue;
        }
        // `code index files` is wired while its migration is still `next` and
        // skipped once a release fills in `since`: a fresh config has no
        // per-worktree index files either way.
        if step.name == "code index files" {
            assert!(
                matches!(step.status, StepStatus::Wired | StepStatus::Skipped),
                "unexpected status for 'code index files': {}",
                step.detail
            );
            continue;
        }
        assert_eq!(
            step.status,
            StepStatus::Wired,
            "expected '{}' to be wired on a fresh config: {}",
            step.name,
            step.detail
        );
    }

    // Assert: every ported hook and every guard resolved into settings.json.
    let settings_path = claude_home.join("settings.json");
    let settings = read_json(&settings_path);
    let commands = all_hook_commands(&settings);
    for name in PORTED_HOOK_NAMES {
        let bare = format!("playbook hook {name}");
        assert!(
            commands.contains(&bare),
            "missing ported hook '{bare}' in {commands:?}"
        );
    }
    for name in GUARD_HOOK_NAMES {
        let bare = format!("playbook hook {name}");
        assert!(
            commands.contains(&bare),
            "missing guard '{bare}' in {commands:?}"
        );
    }

    // Assert: the merge baseline and the statusline landed, and no shell rc
    // file was written (ADR 0044).
    assert!(claude_home.join(".settings.base.json").is_file());
    assert!(!home.join(".bashrc").exists());
    assert!(!home.join(".zshrc").exists());
    assert_eq!(
        read_json(&settings_path)["statusLine"]["command"],
        "playbook statusline"
    );
    assert!(
        !home.join(".config/playbook/statusline.sh").exists(),
        "init no longer places statusline.sh"
    );
}

const OLD_STATUSLINE: &str = "bash $HOME/.config/playbook/statusline.sh";

#[test]
fn the_policy_guard_part_stores_nothing_unless_chosen_and_never_turns_it_off() {
    use playbook::hooks::policy_guard::stored_enabled;
    let home = scratch_home("policy-guard");

    let off = run(&base_paths(&home));
    assert_eq!(find_step(&off, "policy-guard").status, StepStatus::Skipped);
    assert!(!stored_enabled(&home));

    let on = run(&InitPaths {
        policy_guard: true,
        ..base_paths(&home)
    });
    assert_eq!(find_step(&on, "policy-guard").status, StepStatus::Wired);
    assert!(stored_enabled(&home));

    let again = run(&InitPaths {
        policy_guard: true,
        ..base_paths(&home)
    });
    assert_eq!(
        find_step(&again, "policy-guard").status,
        StepStatus::AlreadyCorrect
    );
    run(&base_paths(&home));
    assert!(
        stored_enabled(&home),
        "a run without the part kept the choice"
    );
}

#[test]
fn choosing_the_policy_guard_in_setup_skips_the_off_by_default_notice() {
    let home = scratch_home("policy-guard-no-notice");
    let settings = home.join(".claude/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    let doc = json!({"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [
        {"type": "command", "command": "playbook hook policy-guard"}]}]}});
    fs::write(&settings, doc.to_string()).unwrap();

    let report = run(&InitPaths {
        policy_guard: true,
        ..base_paths(&home)
    });

    assert!(
        !report
            .steps
            .iter()
            .any(|s| s.detail.contains("now off by default")),
        "{:?}",
        report.steps.iter().map(|s| &s.detail).collect::<Vec<_>>()
    );
}
fn status_line_command(claude_home: &Path) -> Value {
    read_json(&claude_home.join("settings.json"))["statusLine"]["command"].clone()
}

#[test]
fn init_rewrites_the_old_statusline_command_even_when_base_is_missing() {
    // Arrange: a user file holding the old command and no merge baseline, the
    // case the three-way merge would otherwise treat as a customisation.
    let home = scratch_home("sl-rewrite");
    let claude_home = claude_home_of(&home);
    write_json(
        &claude_home.join("settings.json"),
        &json!({"statusLine": {"type": "command", "command": OLD_STATUSLINE, "refreshInterval": 30}}),
    );

    // Act
    let outcome = run(&base_paths(&home));

    // Assert
    assert!(outcome.ok());
    assert_eq!(status_line_command(&claude_home), "playbook statusline");
}

#[test]
fn init_leaves_a_custom_statusline_command_alone() {
    // Arrange
    let home = scratch_home("sl-custom");
    let claude_home = claude_home_of(&home);
    write_json(
        &claude_home.join(".settings.base.json"),
        &json!({"statusLine": {"type": "command", "command": OLD_STATUSLINE}}),
    );
    write_json(
        &claude_home.join("settings.json"),
        &json!({"statusLine": {"type": "command", "command": "bash $HOME/my-statusline.sh"}}),
    );

    // Act
    let outcome = run(&base_paths(&home));

    // Assert
    assert!(outcome.ok());
    assert_eq!(
        status_line_command(&claude_home),
        "bash $HOME/my-statusline.sh"
    );
}

#[test]
fn init_keeps_the_old_command_when_the_user_edited_statusline_sh() {
    // Arrange: an older init recorded the shipped script, then the user edited it.
    let home = scratch_home("sl-edited");
    let claude_home = claude_home_of(&home);
    let paths = base_paths(&home);
    run(&paths);
    let script = home.join(".config/playbook/statusline.sh");
    write_text(&script, "#!/bin/sh\necho shipped\n");
    playbook::init::migrate::record_shipped(&home, "statusline", &script);
    let old = json!({"statusLine": {"type": "command", "command": OLD_STATUSLINE}});
    write_json(&claude_home.join("settings.json"), &old);
    write_json(&claude_home.join(".settings.base.json"), &old);
    write_text(
        &home.join(".config/playbook/statusline.sh"),
        "#!/bin/sh\necho mine\n",
    );

    // Act
    let outcome = run(&paths);

    // Assert: the edited script and the command that runs it both survive.
    assert!(outcome.ok());
    assert_eq!(status_line_command(&claude_home), OLD_STATUSLINE);
    assert_eq!(
        fs::read_to_string(home.join(".config/playbook/statusline.sh")).unwrap(),
        "#!/bin/sh\necho mine\n"
    );
    run(&paths);
    assert_eq!(status_line_command(&claude_home), OLD_STATUSLINE);
}

#[test]
fn statusline_command_migration_is_idempotent_across_inits() {
    // Arrange
    let home = scratch_home("sl-idem");
    let claude_home = claude_home_of(&home);
    write_json(
        &claude_home.join("settings.json"),
        &json!({"statusLine": {"type": "command", "command": OLD_STATUSLINE}}),
    );
    let paths = base_paths(&home);

    // Act
    run(&paths);
    let first = fs::read_to_string(claude_home.join("settings.json")).unwrap();
    let second_outcome = run(&paths);

    // Assert
    assert!(second_outcome.ok());
    assert_eq!(
        fs::read_to_string(claude_home.join("settings.json")).unwrap(),
        first
    );
    assert_eq!(status_line_command(&claude_home), "playbook statusline");
}

/// D6 regression pin: `wire`'s guard loop once gated a guard's bare
/// `playbook hook <name>` form on whether `init::guards` had actually placed
/// a script for it, so flipping a `GUARD_SPECS` entry's `ported` field to
/// `true` would have silently done nothing, since a ported guard is never
/// placed. All guards are ported now, so this must hold unconditionally
/// on a fresh install: not one `.hooks` command may name a path under
/// `~/.claude/hooks/`, regardless of whether that path happens to exist.
/// Asserting only "every command that IS a path resolves" would miss this
/// exact regression, since a guard's `.sh` script still ships in this repo
/// and would resolve if `wire` reverted to writing it; the invariant that
/// actually matters is that a ported guard never takes path form at all.
#[test]
fn zero_hook_commands_point_under_claude_hooks_dir_after_a_full_init() {
    // Arrange: a machine with no `~/.claude` at all, the same clean-install
    // shape `fresh_config_gets_fully_wired` exercises.
    let home = scratch_home("zero-hooks-dir");
    let claude_home = claude_home_of(&home);
    let paths = base_paths(&home);

    // Act
    let outcome = run(&paths);

    // Assert
    assert!(outcome.ok(), "expected every step to succeed");
    let settings = read_json(&claude_home.join("settings.json"));
    let commands = all_hook_commands(&settings);
    assert!(
        !commands.is_empty(),
        "a fresh init should write hook commands"
    );
    for cmd in &commands {
        assert!(
            !cmd.contains("/.claude/hooks/"),
            "no hook command may point under ~/.claude/hooks/ after a full init: {cmd}"
        );
    }
}

/// WU-14 scenario 1: a customisation that collides with a template update
/// must produce a readable skip-report JSON file, not just a printed count,
/// alongside the `.bak.<epoch>` backup the write itself already takes.
#[test]
fn withheld_customisation_produces_a_skip_report_alongside_the_backup() {
    // Arrange: BASE differs from both what the template ships
    // (the `statusLine` in `settings.shared.json`) and what the user
    // customised, so the key is genuinely contested.
    let home = scratch_home("skip-report");
    let claude_home = claude_home_of(&home);
    write_json(
        &claude_home.join(".settings.base.json"),
        &json!({"statusLine": {"type": "command", "command": "base-line"}}),
    );
    write_json(
        &claude_home.join("settings.json"),
        &json!({"statusLine": {"type": "command", "command": "my-line"}}),
    );
    let paths = base_paths(&home);

    // Act
    let outcome = run(&paths);

    // Assert
    assert!(
        outcome.ok(),
        "{:?}",
        outcome
            .steps
            .iter()
            .map(StepReport::render)
            .collect::<Vec<_>>()
    );
    assert_eq!(find_step(&outcome, "settings").status, StepStatus::Wired);

    let bak_files = matching_entries(&claude_home, "settings.json.bak.");
    assert_eq!(
        bak_files.len(),
        1,
        "expected exactly one backup file: {bak_files:?}"
    );

    let skip_files = matching_entries(&claude_home, "settings-merge-skipped.");
    assert_eq!(
        skip_files.len(),
        1,
        "expected exactly one skip-report file: {skip_files:?}"
    );
    let skip_content = fs::read_to_string(claude_home.join(&skip_files[0])).unwrap();
    let skip_json: Value = serde_json::from_str(&skip_content)
        .unwrap_or_else(|e| panic!("skip-report should be valid JSON: {e}: {skip_content}"));
    let entries = skip_json
        .as_array()
        .expect("skip-report should be a JSON array");
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one withheld key: {entries:?}"
    );
    assert_eq!(entries[0]["key"], "statusLine");
    assert_eq!(entries[0]["template_had"]["command"], "playbook statusline");
    assert_eq!(entries[0]["yours"]["command"], "my-line");
}

/// WU-14 scenarios 2, 3 and 4: table-driven, since all three share the same
/// "seed N fabricated-epoch files, run one real merge, count survivors"
/// shape and differ only in file family and prior count. Cases 1 and 2 pin
/// the exact 5-to-6 boundary for the backup family (5 prior -> 5 survive,
/// then 6 prior -> 5 survive); case 3 is the symmetric check for the
/// skip-report family.
#[test]
fn stale_backup_and_skip_report_files_beyond_five_are_pruned_after_a_real_merge() {
    struct Case {
        tag: &'static str,
        prior_count: u64,
        prefix: &'static str,
        suffix: &'static str,
        seed_content: &'static str,
    }

    let cases = [
        Case {
            tag: "backups-at-threshold",
            prior_count: 5,
            prefix: "settings.json.bak.",
            suffix: "",
            seed_content: "stale",
        },
        Case {
            tag: "backups-over-threshold",
            prior_count: 6,
            prefix: "settings.json.bak.",
            suffix: "",
            seed_content: "stale",
        },
        Case {
            tag: "skip-reports-over-threshold",
            prior_count: 6,
            prefix: "settings-merge-skipped.",
            suffix: ".json",
            seed_content: "[]",
        },
    ];

    for case in cases {
        // Arrange: fabricated, distinct epochs seeded directly as file
        // names, not by looping real writes, since `backup_then_write`'s
        // epoch has 1-second granularity and a tight loop can collide on the
        // same file name.
        let home = scratch_home(case.tag);
        let claude_home = claude_home_of(&home);
        fs::create_dir_all(&claude_home).unwrap();
        for i in 0..case.prior_count {
            let epoch = 1_000_000_000u64 + i;
            fs::write(
                claude_home.join(format!("{}{epoch}{}", case.prefix, case.suffix)),
                case.seed_content,
            )
            .unwrap();
        }
        // A genuinely contested key, so every case's run writes both a real
        // backup and a real skip-report, regardless of which family the
        // case is pinning.
        write_json(
            &claude_home.join(".settings.base.json"),
            &json!({"cleanupPeriodDays": 999}),
        );
        write_json(
            &claude_home.join("settings.json"),
            &json!({"cleanupPeriodDays": 500}),
        );
        let paths = base_paths(&home);

        // Act
        let outcome = run(&paths);

        // Assert
        assert!(
            outcome.ok(),
            "{}: {:?}",
            case.tag,
            outcome
                .steps
                .iter()
                .map(StepReport::render)
                .collect::<Vec<_>>()
        );
        let survivors = matching_entries(&claude_home, case.prefix);
        assert_eq!(
            survivors.len(),
            5,
            "{}: {} prior file(s) plus one new real write should retain exactly 5: {survivors:?}",
            case.tag,
            case.prior_count
        );
    }
}

/// WU-14 scenario 5: an idempotent re-run must create no new backup or
/// skip-report file, and must not prune either family, even when both
/// already sit well past the retain-5 threshold.
#[test]
fn idempotent_rerun_creates_no_backup_or_skip_report_and_prunes_nothing() {
    // Arrange: run once to reach a stable, merged state (a real write, one
    // backup taken, no skip-report since nothing was withheld on a fresh
    // install), then seed extra stale files in both families beyond the
    // retain-5 threshold.
    let home = scratch_home("idempotent-skip");
    let claude_home = claude_home_of(&home);
    let paths = base_paths(&home);
    let first = run(&paths);
    assert!(first.ok());

    for i in 0..6u64 {
        let epoch = 1_000_000_000u64 + i;
        fs::write(
            claude_home.join(format!("settings.json.bak.{epoch}")),
            "stale",
        )
        .unwrap();
        fs::write(
            claude_home.join(format!("settings-merge-skipped.{epoch}.json")),
            "[]",
        )
        .unwrap();
    }
    let bak_count_before = matching_entries(&claude_home, "settings.json.bak.").len();
    let skip_count_before = matching_entries(&claude_home, "settings-merge-skipped.").len();

    // Act: a second, idempotent run.
    let second = run(&paths);

    // Assert: settings reports no change, and neither seeded family was
    // touched at all, proving pruning did not fire.
    assert!(second.ok());
    assert_eq!(
        find_step(&second, "settings").status,
        StepStatus::AlreadyCorrect
    );
    let bak_count_after = matching_entries(&claude_home, "settings.json.bak.").len();
    let skip_count_after = matching_entries(&claude_home, "settings-merge-skipped.").len();
    assert_eq!(
        bak_count_after, bak_count_before,
        "an idempotent re-run must not create or prune backup files"
    );
    assert_eq!(
        skip_count_after, skip_count_before,
        "an idempotent re-run must not create or prune skip-report files"
    );
}

/// WU-14 scenario 6: a fresh install's placeholder-then-merge sequence still
/// backs up the placeholder (pre-existing behaviour, per `run.rs:336-345`),
/// producing exactly one backup file; nothing is withheld on a fresh
/// install, so no skip-report file is produced.
#[test]
fn fresh_install_placeholder_produces_exactly_one_backup_and_no_skip_report() {
    // Arrange: no prior `~/.claude` at all.
    let home = scratch_home("fresh-placeholder-backup");
    let claude_home = claude_home_of(&home);
    let paths = base_paths(&home);

    // Act
    let outcome = run(&paths);

    // Assert
    assert!(outcome.ok());
    assert_eq!(find_step(&outcome, "settings").status, StepStatus::Wired);
    let bak_files = matching_entries(&claude_home, "settings.json.bak.");
    assert_eq!(
        bak_files.len(),
        1,
        "expected exactly one backup file from the placeholder write: {bak_files:?}"
    );
    let skip_files = matching_entries(&claude_home, "settings-merge-skipped.");
    assert!(
        skip_files.is_empty(),
        "a fresh install should withhold nothing: {skip_files:?}"
    );
}

#[test]
fn running_init_twice_is_idempotent_with_no_second_run_changes() {
    // Arrange
    let home = scratch_home("idempotent");
    let claude_home = claude_home_of(&home);
    let settings_path = claude_home.join("settings.json");
    let base_path = claude_home.join(".settings.base.json");

    // Act: first run wires everything.
    let first = run(&base_paths(&home));
    assert!(first.ok(), "first run should succeed");
    let after_first = (
        fs::read(&settings_path).unwrap(),
        fs::read(&base_path).unwrap(),
    );

    // Act: second run against the exact same paths.
    let second = run(&base_paths(&home));

    // Assert: nothing is reported as a change the second time.
    assert!(second.ok());
    for step in &second.steps {
        // `system-prompt` and `memory` stay `Skipped` on both runs: neither
        // has anything to act on in this fixture.
        let expected = if [
            "system-prompt",
            "memory",
            "trust",
            "path",
            "telemetry",
            "short-name",
            "completions",
            "policy-guard",
            "codex-hooks",
            "plugin update mode",
            "policy guard",
            "rm guard",
        ]
        .contains(&step.name)
        {
            StepStatus::Skipped
        } else {
            StepStatus::AlreadyCorrect
        };
        assert_eq!(
            step.status, expected,
            "expected '{}' to report no change on a second run: {}",
            step.name, step.detail
        );
    }

    // Assert: every file this run touches is byte-identical to after the
    // first run. This is what actually catches a spurious second-run
    // rewrite; the status check above only catches a wrong REPORT.
    let after_second = (
        fs::read(&settings_path).unwrap(),
        fs::read(&base_path).unwrap(),
    );
    assert_eq!(
        after_first, after_second,
        "a second run must not rewrite any file"
    );
}

#[test]
fn the_entry_of_the_removed_disk_guard_hook_is_dropped_by_composed_init() {
    // Arrange: a 0.38.0 install, whose `settings.json` still runs the disk
    // guard on Bash and on agents, next to a hook that still exists.
    let home = scratch_home("retired-disk-guard");
    let claude_home = claude_home_of(&home);
    let retired = "playbook hook disk-guard";
    write_json(
        &claude_home.join("settings.json"),
        &json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [
                            {"type": "command", "command": retired},
                            {"type": "command", "command": "playbook hook policy-guard"}
                        ]
                    },
                    {
                        "matcher": "Agent|Task",
                        "hooks": [{"type": "command", "command": retired}]
                    }
                ]
            }
        }),
    );

    // Act
    let outcome = run(&base_paths(&home));

    // Assert
    assert!(outcome.ok());
    let commands = all_hook_commands(&read_json(&claude_home.join("settings.json")));
    assert!(!commands.contains(&retired.to_string()), "{commands:?}");
    assert!(commands.contains(&"playbook hook policy-guard".to_string()));
}

#[test]
fn hand_added_hook_entry_survives_composed_init() {
    // Arrange: a `settings.json` with a hook entry the template does not
    // know about, alongside the four guards a real unwired install ships.
    // Its script exists, so it is the user's own and not a dead entry.
    let home = scratch_home("hand-added");
    let claude_home = claude_home_of(&home);
    let hand_added_command = "~/.claude/hooks/my-custom-guard.sh";
    write_text(&claude_home.join("hooks/my-custom-guard.sh"), "#!/bin/sh\n");
    write_json(
        &claude_home.join("settings.json"),
        &json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [
                            {"type": "command", "command": hand_added_command}
                        ]
                    }
                ]
            }
        }),
    );

    // Act
    let outcome = run(&base_paths(&home));

    // Assert
    assert!(
        outcome.ok(),
        "{:?}",
        outcome
            .steps
            .iter()
            .map(StepReport::render)
            .collect::<Vec<_>>()
    );
    let settings = read_json(&claude_home.join("settings.json"));
    let commands = all_hook_commands(&settings);
    assert!(
        commands.contains(&hand_added_command.to_string()),
        "hand-added entry was clobbered: {commands:?}"
    );
    // The composed run still wired the ported hooks alongside it.
    assert!(commands.contains(&"playbook hook session-init".to_string()));
}

#[test]
fn malformed_settings_json_fails_without_panicking_or_writing() {
    // Arrange
    let home = scratch_home("malformed");
    let claude_home = claude_home_of(&home);
    let settings_path = claude_home.join("settings.json");
    fs::create_dir_all(&claude_home).unwrap();
    let malformed = "{not valid json";
    fs::write(&settings_path, malformed).unwrap();

    // Act
    let outcome = run(&base_paths(&home));

    // Assert: the run reports failure, with a useful message, not a panic
    // (reaching this line at all proves that), and the broken file is left
    // exactly as it was rather than being partially rewritten.
    assert!(!outcome.ok());
    let settings_step = find_step(&outcome, "settings");
    assert_eq!(settings_step.status, StepStatus::Failed);
    assert!(!settings_step.detail.is_empty());
    let hooks_step = find_step(&outcome, "hooks");
    assert_eq!(hooks_step.status, StepStatus::Failed);
    assert!(
        hooks_step.detail.contains("not valid JSON"),
        "{}",
        hooks_step.detail
    );
    assert_eq!(fs::read_to_string(&settings_path).unwrap(), malformed);
}

#[test]
fn missing_self_root_skips_template_dependent_steps() {
    // Arrange: `CLAUDE_PLUGIN_ROOT` unresolved, modelled as `self_root: None`.
    let home = scratch_home("no-root");
    let paths = InitPaths {
        plugin_root: None,
        self_root: None,
        claude_home: claude_home_of(&home),
        home: home.clone(),
        system_prompt: playbook::init::system_prompt::Answer::Unasked,
        hooks: true,
        statusline: true,
        security: false,
        telemetry: false,
        telemetry_replace_off: false,
        only: false,
        short_name: playbook::init::pb_link::Choice::Skip,
        policy_guard: false,
        codex_hooks: false,
        pb_target: None,
        completions: None,
        attribution: playbook::init::attribution::Choice::Skip,
        path_setup: None,
        repo: None,
    };

    // Act
    let outcome = run(&paths);

    // Assert: a missing template is not a failure, it is a clean skip, and
    // `hooks` still runs since `wire::wire` needs no template at all.
    assert!(outcome.ok());
    assert_eq!(find_step(&outcome, "settings").status, StepStatus::Skipped);
    assert_eq!(find_step(&outcome, "hooks").status, StepStatus::Wired);
}

/// The config hash runs inside the binary now, so init places no shell script.
#[test]
fn init_places_no_config_hash_script() {
    let home = scratch_home("config-hash");
    let outcome = run(&base_paths(&home));
    assert!(outcome.ok());
    assert!(!home
        .join(".config/playbook/hooks/lib/config-hash.sh")
        .exists());
}

/// Spawns the real compiled binary rather than calling `run` directly: the
/// non-zero exit code on failure is `main.rs`'s contract, not `init::run`'s,
/// so only a real process boundary proves it.
#[test]
fn binary_malformed_settings_json_exits_non_zero() {
    // Arrange
    let home = scratch_home("binary-malformed");
    let claude_home = claude_home_of(&home);
    fs::create_dir_all(&claude_home).unwrap();
    fs::write(claude_home.join("settings.json"), "{not valid json").unwrap();

    // Act
    let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .args(["setup", "--yes"])
        .env("HOME", &home)
        .env("CLAUDE_PLUGIN_ROOT", checkout_root())
        .env("SHELL", "/bin/bash")
        .output()
        .expect("playbook binary should spawn");

    // Assert
    assert_eq!(
        out.status.code(),
        Some(1),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("step(s) failed"), "{stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("[fail]"), "{stdout}");
}

/// The other half of the "fully wired" and "idempotent" library-level tests,
/// proven through the real binary: a clean scratch `$HOME` wires everything
/// on the first `playbook setup` and exits 0 unchanged on the second.
#[test]
fn binary_clean_init_exits_zero_and_is_idempotent() {
    // Arrange
    let home = scratch_home("binary-clean");
    let run_init = || {
        Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(["setup", "--yes"])
            .env("HOME", &home)
            .env("CLAUDE_PLUGIN_ROOT", checkout_root())
            .env("SHELL", "/bin/bash")
            .output()
            .expect("playbook binary should spawn")
    };

    // Act
    let first = run_init();
    let second = run_init();

    // Assert
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let first_out = String::from_utf8_lossy(&first.stdout);
    let second_out = String::from_utf8_lossy(&second.stdout);
    assert!(
        first_out.lines().all(|l| !l.starts_with("[=]")),
        "first run should report only changes: {first_out}"
    );
    assert!(
        second_out.lines().all(|l| !l.starts_with("[ok]")),
        "second run should report no changes: {second_out}"
    );
    assert!(
        !home.join(".config/playbook/embedded").exists(),
        "the checkout's seed/ folder should be the seed, not the embedded copies"
    );
}

/// The `memory` step must appear in `run()`'s reported steps.
#[test]
fn memory_migrate_step_appears_in_reported_steps() {
    // Arrange
    let home = scratch_home("memory-migrate-presence");
    let paths = base_paths(&home);

    // Act
    let outcome = run(&paths);

    // Assert
    find_step(&outcome, "memory");
}

/// `run()` calls `memory_migrate::migrate_memory(&paths.home, &paths.claude_home)`;
/// every other test in this file only exercises the no-op path (no legacy
/// `~/.claude/memory` present), which would stay green even if `home` and
/// `claude_home` were swapped at that call site. Seeding real content and
/// checking it actually lands at `home/.config/playbook/memory` closes that
/// gap.
#[test]
fn memory_step_actually_migrates_real_content_to_the_new_root() {
    // Arrange: a legacy graph.json under the pre-migration location.
    let home = scratch_home("memory-migrates-real-content");
    let claude_home = claude_home_of(&home);
    let mem_dir = claude_home.join("memory");
    std::fs::create_dir_all(&mem_dir).unwrap();
    std::fs::write(mem_dir.join("graph.json"), r#"{"nodes":[]}"#).unwrap();
    let paths = base_paths(&home);

    // Act
    let outcome = run(&paths);

    // Assert
    let memory_step = find_step(&outcome, "memory");
    assert_eq!(
        memory_step.status,
        StepStatus::Wired,
        "{}",
        memory_step.detail
    );
    let new_graph = home
        .join(".config")
        .join("playbook")
        .join("memory")
        .join("memory.graph.json");
    assert_eq!(
        std::fs::read_to_string(&new_graph)
            .unwrap_or_else(|e| panic!("expected content at {}: {e}", new_graph.display())),
        r#"{"nodes":[]}"#
    );
    assert!(
        !mem_dir.exists(),
        "the pre-migration tree should be gone once the move completes"
    );
}

/// Before this fix, only the SessionStart hook's own private fallback
/// repaired a stray `graph.json` left at the NEW root; `playbook setup` had
/// no equivalent, so this scenario would have left the stray file in place.
/// `migrate_memory` now folds that self-heal in for every caller, `run()`
/// included, closing that gap.
#[test]
fn memory_step_repairs_a_stray_graph_file_at_the_new_root() {
    // Arrange: a fully migrated store, except a stray old-named file also
    // sits at the new root (e.g. left behind by an older build).
    let home = scratch_home("memory-new-root-stray");
    let new_root = home.join(".config").join("playbook").join("memory");
    std::fs::create_dir_all(&new_root).unwrap();
    std::fs::write(new_root.join("graph.json"), r#"{"nodes":[]}"#).unwrap();
    std::fs::write(new_root.join(".migration-complete"), "migrated\n").unwrap();
    let paths = base_paths(&home);

    // Act
    let outcome = run(&paths);

    // Assert: the stray file is renamed away, content preserved. The store
    // is already fully migrated (sentinel present), so the step's own
    // status is AlreadyCorrect; the self-heal is a silent side effect of
    // that same call, not a status change.
    let memory_step = find_step(&outcome, "memory");
    assert_eq!(
        memory_step.status,
        StepStatus::AlreadyCorrect,
        "{}",
        memory_step.detail
    );
    assert!(
        !new_root.join("graph.json").exists(),
        "playbook setup should now repair a stray new-root graph.json, not just SessionStart"
    );
    assert_eq!(
        std::fs::read_to_string(new_root.join("memory.graph.json")).unwrap(),
        r#"{"nodes":[]}"#
    );
}

/// A full pre-ADR-0012 install migrates in one `run()` call.
#[test]
fn existing_install_migrates_settings_and_rcfile_with_doctor_reporting_no_drift() {
    // Arrange: old files under `~/.claude`, settings.json naming the old
    // statusline path, an rc file sourcing the old shell runtime path.
    let home = scratch_home("existing-install-migrates");
    let claude_home = claude_home_of(&home);
    write_text(&claude_home.join("statusline.sh"), "#!/bin/sh\necho old\n");
    write_text(&claude_home.join("shell/bash/cc.sh"), "# old cc.sh\n");
    let old_status_line = json!({
        "type": "command",
        "command": "bash $HOME/.claude/statusline.sh",
        "refreshInterval": 30
    });
    write_json(
        &claude_home.join("settings.json"),
        &json!({ "statusLine": old_status_line }),
    );
    // BASE agrees with settings.json, as a real pre-existing install would.
    write_json(
        &claude_home.join(".settings.base.json"),
        &json!({ "statusLine": old_status_line }),
    );
    write_text(
        &home.join(".bashrc"),
        "export EDITOR=vim\n\n# playbook launchers (cc/ccd)\nsource \"$HOME/.claude/shell/bash/cc.sh\"\n",
    );
    let paths = base_paths(&home);

    // Act
    let outcome = run(&paths);

    // Assert: migration succeeded end to end.
    assert!(
        outcome.ok(),
        "{:?}",
        outcome
            .steps
            .iter()
            .map(StepReport::render)
            .collect::<Vec<_>>()
    );

    // The `retired-launcher-rc-line` migration removes the old launcher block after a backup (ADR 0044).
    let rc = fs::read_to_string(home.join(".bashrc")).unwrap();
    assert_eq!(rc, "export EDITOR=vim\n");

    // Doctor Layer 5 shape: the Rust command.
    let settings_path = claude_home.join("settings.json");
    let settings = read_json(&settings_path);
    assert_eq!(settings["statusLine"]["command"], "playbook statusline");

    // Doctor Layer 7 shape: every hook command is bare, so nothing dangles.
    let commands = all_hook_commands(&settings);
    assert!(!commands.is_empty());
    for cmd in &commands {
        assert!(
            cmd.starts_with("playbook hook "),
            "no lingering path-shaped hook command: {cmd}"
        );
    }
}

// ── trust step ───────────────────────────────────────────────────────────────

fn trusted(claude_json: &Path, dir: &Path) -> bool {
    let doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(claude_json).unwrap()).unwrap();
    doc["projects"][dir.to_str().unwrap()]["hasTrustDialogAccepted"] == true
}

#[test]
fn init_trusts_the_config_dir_and_leaves_siblings_alone() {
    // Arrange: an existing ~/.claude.json with another project and a key.
    let home = scratch_home("trust-adds");
    let claude_json = home.join(".claude.json");
    fs::write(
        &claude_json,
        r#"{"numStartups":3,"projects":{"/other":{"hasTrustDialogAccepted":false,"x":1}}}"#,
    )
    .unwrap();
    let config_dir = home.join(".config").join("playbook");

    // Act
    let outcome = run(&base_paths(&home));

    // Assert
    let step = find_step(&outcome, "trust");
    assert_eq!(step.status, StepStatus::Wired, "{}", step.detail);
    assert!(trusted(&claude_json, &config_dir));
    let doc: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&claude_json).unwrap()).unwrap();
    assert_eq!(doc["numStartups"], 3);
    assert_eq!(doc["projects"]["/other"]["hasTrustDialogAccepted"], false);
    assert_eq!(doc["projects"]["/other"]["x"], 1);
}

#[test]
fn a_second_init_changes_nothing_in_claude_json() {
    // Arrange
    let home = scratch_home("trust-twice");
    let claude_json = home.join(".claude.json");
    fs::write(&claude_json, "{}").unwrap();
    run(&base_paths(&home));
    let first = fs::read(&claude_json).unwrap();

    // Act
    let outcome = run(&base_paths(&home));

    // Assert
    assert_eq!(
        find_step(&outcome, "trust").status,
        StepStatus::AlreadyCorrect
    );
    assert_eq!(fs::read(&claude_json).unwrap(), first);
}

#[test]
fn init_never_creates_claude_json() {
    // Arrange
    let home = scratch_home("trust-absent");

    // Act
    let outcome = run(&base_paths(&home));

    // Assert
    assert_eq!(find_step(&outcome, "trust").status, StepStatus::Skipped);
    assert!(!home.join(".claude.json").exists());
}

#[test]
fn a_malformed_claude_json_warns_and_init_still_succeeds() {
    // Arrange
    let home = scratch_home("trust-malformed");
    let claude_json = home.join(".claude.json");
    fs::write(&claude_json, "{ not json").unwrap();

    // Act
    let outcome = run(&base_paths(&home));

    // Assert: reported as skipped with the reason, the file untouched.
    let step = find_step(&outcome, "trust");
    assert_eq!(step.status, StepStatus::Skipped);
    assert!(step.detail.contains("could not"), "{}", step.detail);
    assert!(outcome.ok());
    assert_eq!(fs::read_to_string(&claude_json).unwrap(), "{ not json");
}

/// Init records the placed system prompt, then warns once the user edits it.
#[test]
fn init_warns_when_the_installed_system_prompt_was_edited() {
    // Arrange
    let home = scratch_home("prompt-edited");
    let mut paths = base_paths(&home);
    paths.system_prompt = playbook::init::system_prompt::Answer::Yes;
    let first = run(&paths);
    assert!(first.warnings.is_empty());
    let prompt = home.join(".config/playbook/prompts/SYSTEM_PROMPT.md");
    std::fs::write(&prompt, "my own edits").unwrap();

    // Act
    let second = run(&paths);

    // Assert
    assert_eq!(second.warnings.len(), 1, "{:?}", second.warnings);
    assert!(second.warnings[0].contains("edited"));
}

#[test]
fn the_path_step_puts_the_binary_dir_on_path_and_is_idempotent() {
    let home = scratch_home("path-step");
    let mut paths = base_paths(&home);
    paths.path_setup = Some(playbook::init::path::Setup {
        bin_dir: PathBuf::from("/opt/pb/bin"),
        shell: playbook::init::path::PathShell::Zsh,
    });

    let first = run(&paths);
    let step = find_step(&first, "path");
    assert_eq!(step.status, StepStatus::Wired, "{}", step.detail);
    let zshenv = fs::read_to_string(home.join(".zshenv")).unwrap();
    assert!(
        zshenv.contains("export PATH=\"/opt/pb/bin:$PATH\""),
        "{zshenv}"
    );

    let second = run(&paths);
    assert_eq!(
        find_step(&second, "path").status,
        StepStatus::AlreadyCorrect
    );
    assert_eq!(fs::read_to_string(home.join(".zshenv")).unwrap(), zshenv);
}

#[test]
fn without_a_binary_location_the_path_step_is_skipped() {
    let home = scratch_home("path-skip");
    let outcome = run(&base_paths(&home));
    assert_eq!(find_step(&outcome, "path").status, StepStatus::Skipped);
    assert!(!home.join(".zshenv").exists());
}

#[test]
fn a_fresh_init_without_the_statusline_leaves_statusline_out_of_settings() {
    // Arrange: no settings yet, status line answered no.
    let home = scratch_home("no-statusline-fresh");
    let mut paths = base_paths(&home);
    paths.statusline = false;

    // Act
    let outcome = run(&paths);

    // Assert
    assert!(outcome.ok());
    let settings = read_json(&claude_home_of(&home).join("settings.json"));
    assert!(settings.get("statusLine").is_none(), "{settings}");
}

#[test]
fn init_without_the_statusline_keeps_a_custom_command_and_does_not_rewrite_the_old_one() {
    // Arrange: the legacy script command is set, status line answered no.
    let home = scratch_home("no-statusline-legacy");
    let claude_home = claude_home_of(&home);
    let legacy = "bash $HOME/.config/playbook/statusline.sh";
    write_json(
        &claude_home.join("settings.json"),
        &serde_json::json!({"statusLine": {"type": "command", "command": legacy}}),
    );
    let mut paths = base_paths(&home);
    paths.statusline = false;

    // Act: twice, so the three-way merge cannot re-add or change it.
    run(&paths);
    let outcome = run(&paths);

    // Assert
    assert!(outcome.ok());
    assert_eq!(
        status_line_command(&claude_home),
        Value::String(legacy.into())
    );
}

#[test]
fn init_without_the_statusline_says_an_existing_playbook_status_line_stays() {
    // Arrange: a playbook status line from an earlier init.
    let home = scratch_home("no-statusline-existing");
    let claude_home = claude_home_of(&home);
    run(&base_paths(&home));
    let mut paths = base_paths(&home);
    paths.statusline = false;

    // Act
    let outcome = run(&paths);

    // Assert
    assert_eq!(
        status_line_command(&claude_home),
        Value::String("playbook statusline".into())
    );
    assert!(find_step(&outcome, "settings")
        .detail
        .contains("status line not changed"));
}

#[cfg(unix)]
#[test]
fn setup_links_pb_in_the_install_folder_and_writes_no_rc_file() {
    use std::os::unix::fs::PermissionsExt;
    // Arrange: a binary in the install folder and no `pb` anywhere.
    let home = scratch_home("short-name");
    let bin = home.join(".local/bin");
    fs::create_dir_all(&bin).unwrap();
    let exe = bin.join("playbook");
    write_text(&exe, "#!/bin/sh\n");
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
    let mut paths = base_paths(&home);
    paths.short_name = playbook::init::pb_link::Choice::Link;
    paths.pb_target = Some(playbook::init::pb_link::Target {
        exe,
        install_dir: bin.clone(),
        path_var: Default::default(),
    });

    // Act
    let first = run(&paths);
    let second = run(&paths);

    // Assert
    assert_eq!(find_step(&first, "short-name").status, StepStatus::Wired);
    assert_eq!(
        find_step(&second, "short-name").status,
        StepStatus::AlreadyCorrect
    );
    assert_eq!(
        fs::read_link(bin.join("pb")).unwrap(),
        Path::new("playbook")
    );
    assert!(!home.join(".zshrc").exists() && !home.join(".bashrc").exists());
}

#[test]
fn init_records_this_binary_in_the_version_record() {
    // Arrange
    let home = scratch_home("version-record");
    let paths = base_paths(&home);

    // Act
    let outcome = run(&paths);

    // Assert
    assert!(outcome.ok());
    let root = playbook::common::paths::playbook_root_from(&home);
    let record = playbook::heal::Record::read(&root).unwrap();
    assert_eq!(record.binary.as_deref(), Some(env!("CARGO_PKG_VERSION")));
    assert_eq!(record.schema, playbook::heal::LEGACY_SCHEMA);
}

#[test]
fn init_with_the_hooks_left_off_keeps_a_dead_hook_entry() {
    // Arrange
    let home = scratch_home("dead-hook-declined");
    let claude_home = claude_home_of(&home);
    let dead = "bash ~/.claude/hooks/gone.sh";
    write_json(
        &claude_home.join("settings.json"),
        &json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": dead}]}]}}),
    );
    let mut paths = base_paths(&home);
    paths.hooks = false;

    // Act
    let outcome = run(&paths);

    // Assert
    assert!(outcome.ok());
    let settings = read_json(&claude_home.join("settings.json"));
    assert!(all_hook_commands(&settings).contains(&dead.to_string()));
    let root = playbook::common::paths::playbook_root_from(&home);
    let record = playbook::heal::Record::read(&root).unwrap();
    assert!(record.pending.iter().any(|id| id == "dead-hook-scripts"));
}

#[test]
fn the_codex_hooks_part_writes_nothing_unless_chosen() {
    // Arrange
    let home = scratch_home("codex-hooks-off");
    fs::create_dir_all(home.join(".codex")).unwrap();
    // Act
    let outcome = run(&base_paths(&home));
    // Assert
    assert_eq!(
        find_step(&outcome, "codex-hooks").status,
        StepStatus::Skipped
    );
    assert!(!home.join(".codex/hooks.json").exists());
}

#[test]
fn the_codex_hooks_part_registers_the_guards_once_and_keeps_user_entries() {
    // Arrange
    let home = scratch_home("codex-hooks-on");
    fs::create_dir_all(home.join(".codex")).unwrap();
    let file = home.join(".codex/hooks.json");
    write_json(
        &file,
        &json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "mine"}]}]}}),
    );
    let chosen = || InitPaths {
        codex_hooks: true,
        ..base_paths(&home)
    };
    // Act
    let first = run(&chosen());
    let second = run(&chosen());
    // Assert
    assert_eq!(find_step(&first, "codex-hooks").status, StepStatus::Wired);
    assert_eq!(
        find_step(&second, "codex-hooks").status,
        StepStatus::AlreadyCorrect
    );
    let commands = all_hook_commands(&read_json(&file));
    assert!(commands.contains(&"mine".to_string()));
    assert!(commands.contains(&"playbook hook rm-workspace-guard".to_string()));
    assert!(commands.contains(&"playbook hook precommit-check".to_string()));
}
