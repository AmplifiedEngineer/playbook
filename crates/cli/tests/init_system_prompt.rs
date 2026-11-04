// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for the `system-prompt` step `playbook::init::run`
//! composes in, backed by `playbook::init::system_prompt`.
//!
//! These drive the whole of `init::run::run` rather than
//! `system_prompt::place_system_prompt` directly, since the scenarios named
//! in the Work Unit brief are phrased in terms of `InitPaths.system_prompt`
//! and the `StepStatus` `run` reports, not the lower-level `Placement` enum.
//! Kept in their own file rather than folded into `tests/init_run.rs`: that
//! file's own scenarios all hold `system_prompt: false` fixed and assert
//! about the other five steps, so mixing in a second axis of variation on
//! `system_prompt` itself would blur what each test is pinning.
//!
//! Coverage map:
//! - opt-in false, no existing file, nothing installed, reports `Skipped`:
//!   `system_prompt_false_and_no_existing_file_leaves_nothing_installed_and_reports_skipped`
//! - opt-in true installs it: `system_prompt_true_installs_it`
//! - opt-in false but a stale copy already present is refreshed anyway,
//!   the opt-in-preserving behaviour `init::system_prompt`'s doc comment
//!   documents: `system_prompt_false_but_existing_stale_copy_is_refreshed`
//!
//! Every test uses a scratch directory standing in for `$HOME`; none read or
//! write the developer's real `~/.claude`.

use playbook::init::run::{run, InitOutcome, InitPaths, StepReport, StepStatus};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The checkout's `seed/` folder, where `prompts/SYSTEM_PROMPT.md` and the
/// rest of what `init::run::run` reads live, matching the helper of the same
/// name in `tests/init_run.rs`.
fn self_root() -> PathBuf {
    PathBuf::from(env!("PLAYBOOK_REPO_ROOT")).join("seed")
}

static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh scratch directory standing in for `$HOME`, unique per call so
/// parallel tests never collide and none of them ever touch a real
/// `~/.claude`.
fn scratch_home(tag: &str) -> PathBuf {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
    let home = env::temp_dir().join(format!(
        "playbook-init-system-prompt-{}-{tag}-{n}",
        playbook::testing::run_id()
    ));
    fs::create_dir_all(&home).expect("scratch home should be creatable");
    home
}

fn claude_home_of(home: &Path) -> PathBuf {
    home.join(".claude")
}

fn dest_path(home: &Path) -> PathBuf {
    home.join(".config/playbook/prompts/SYSTEM_PROMPT.md")
}

fn base_paths(home: &Path, system_prompt: bool) -> InitPaths {
    InitPaths {
        plugin_root: None,
        self_root: Some(self_root()),
        claude_home: claude_home_of(home),
        home: home.to_path_buf(),
        system_prompt: if system_prompt {
            playbook::init::system_prompt::Answer::Yes
        } else {
            playbook::init::system_prompt::Answer::Unasked
        },
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
    }
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

#[test]
fn system_prompt_false_and_no_existing_file_leaves_nothing_installed_and_reports_skipped() {
    // Arrange: a fresh machine, `--system-prompt` never passed.
    let home = scratch_home("no-opt-in-fresh");
    let paths = base_paths(&home, false);

    // Act
    let outcome = run(&paths);

    // Assert
    let step = find_step(&outcome, "system-prompt");
    assert_eq!(
        step.status,
        StepStatus::Skipped,
        "not opted in and no existing copy should be a clean skip: {}",
        step.detail
    );
    assert!(
        !dest_path(&home).exists(),
        "system_prompt: false with no prior file must not install one"
    );
}

#[test]
fn system_prompt_true_installs_it() {
    // Arrange: a fresh machine, `playbook setup --system-prompt`.
    let home = scratch_home("opt-in-fresh");
    let paths = base_paths(&home, true);

    // Act
    let outcome = run(&paths);

    // Assert
    let step = find_step(&outcome, "system-prompt");
    assert_eq!(
        step.status,
        StepStatus::Wired,
        "opting in on a fresh machine should install the prompt: {}",
        step.detail
    );
    let dest = dest_path(&home);
    assert_eq!(
        fs::read(&dest).expect("installed prompt should be readable"),
        fs::read(self_root().join("prompts/SYSTEM_PROMPT.md"))
            .expect("shipped prompts/SYSTEM_PROMPT.md should be readable"),
        "installed content should match the shipped prompt exactly"
    );
}

#[test]
fn system_prompt_false_but_existing_stale_copy_is_refreshed() {
    // Arrange: a machine that opted in on some earlier run (or was left with
    // a stale copy some other way), running `init` again WITHOUT the flag.
    // `init::system_prompt`'s doc comment calls this out explicitly: a user
    // who opted in once should not silently drift onto a stale copy just
    // because a later run omitted the flag.
    let home = scratch_home("no-flag-stale-copy");
    let dest = dest_path(&home);
    fs::create_dir_all(dest.parent().unwrap()).expect("prompts dir should be creatable");
    fs::write(
        &dest,
        "a stale, hand-edited copy that predates the shipped prompt\n",
    )
    .expect("seeding a stale copy should succeed");
    let paths = base_paths(&home, false);

    // Act
    let outcome = run(&paths);

    // Assert: refreshed, not skipped, and it is easy to get this backwards
    // (treating `system_prompt: false` as "leave it alone").
    let step = find_step(&outcome, "system-prompt");
    assert_eq!(
        step.status,
        StepStatus::Wired,
        "an existing stale copy must be refreshed even without --system-prompt: {}",
        step.detail
    );
    assert_eq!(
        fs::read(&dest).expect("refreshed prompt should be readable"),
        fs::read(self_root().join("prompts/SYSTEM_PROMPT.md"))
            .expect("shipped prompts/SYSTEM_PROMPT.md should be readable"),
        "the stale copy should now match the shipped prompt"
    );
}
