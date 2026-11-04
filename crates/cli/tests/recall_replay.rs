// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Replay of 11 prompts through the `memory-anchors` prompt hook against one
//! seeded scratch `HOME`. The replay is the acceptance layer for the memory
//! lookup work: every prompt is checked against five bars, each in its own
//! test so a failure names the bar it broke.
//!
//! Tests only touch a scratch `HOME`, never the real `~/.config/playbook`.

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

// --- Bars --------------------------------------------------------------------

/// Most facts one prompt may inject (`MAX_FACTS_PER_PROMPT`).
const MAX_INJECTED_FACTS: usize = 3;
/// Longest allowed hint-only message, in characters.
const MAX_HINT_CHARS: usize = 400;
/// Longest allowed message of any kind (`PROMPT_RECALL_CAP_CHARS`).
const MAX_MESSAGE_CHARS: usize = 5000;

/// Marker that starts every injected fact entry.
const FACT_HEADING: &str = "### ";
/// Marker that starts every hint line.
const HINT_PREFIX: &str = "Memory:";

// --- Seeded facts ------------------------------------------------------------

const CODE_FACT: &str = "playbook-code-crate-source-index";
const LAYOUT_FACT: &str = "rust-monorepo-crates-layout";

/// Body marker per fact, so a test can tell which fact was injected.
const CODE_MARKER: &str = "CODEINDEXBODYMARKER";
const LAYOUT_MARKER: &str = "LAYOUTBODYMARKER";

// --- Prompts -----------------------------------------------------------------

/// What a prompt is expected to produce.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Outcome {
    /// Fact injected and no `Memory:` line.
    Strong,
    /// Fact injected plus the weak-match hint.
    Weak,
    /// Hint alone, no fact.
    Hint,
    /// No output at all.
    Silent,
}

struct Case {
    prompt: &'static str,
    /// Plan expectation, kept to report any real divergence.
    expected: Outcome,
    /// The fact marker that must be present, if any.
    marker: Option<&'static str>,
    /// The prompt has at least one searchable term.
    searchable: bool,
}

const CASES: [Case; 11] = [
    Case {
        prompt: "PB code crate",
        expected: Outcome::Strong,
        marker: Some(CODE_MARKER),
        searchable: true,
    },
    Case {
        prompt: "what does pbcode do",
        expected: Outcome::Strong,
        marker: Some(CODE_MARKER),
        searchable: true,
    },
    Case {
        // One plain name token ("index"): injected, but weak under R2.
        prompt: "tell me about the code index",
        expected: Outcome::Weak,
        marker: Some(CODE_MARKER),
        searchable: true,
    },
    Case {
        prompt: "rust monorepo crates layout",
        expected: Outcome::Strong,
        marker: Some(LAYOUT_MARKER),
        searchable: true,
    },
    Case {
        prompt: "fix the bug in src",
        expected: Outcome::Hint,
        marker: None,
        searchable: true,
    },
    Case {
        prompt: "how does the zebra importer work",
        expected: Outcome::Hint,
        marker: None,
        searchable: true,
    },
    Case {
        prompt: "kubernetes helm chart",
        expected: Outcome::Hint,
        marker: None,
        searchable: true,
    },
    Case {
        prompt: "yes",
        expected: Outcome::Silent,
        marker: None,
        searchable: false,
    },
    Case {
        prompt: "fix the test",
        expected: Outcome::Silent,
        marker: None,
        searchable: false,
    },
    Case {
        prompt: "why is the crates folder split",
        expected: Outcome::Weak,
        marker: Some(LAYOUT_MARKER),
        searchable: true,
    },
    Case {
        prompt: "explain playbook-code-crate-source-index",
        expected: Outcome::Strong,
        marker: Some(CODE_MARKER),
        searchable: true,
    },
];

// --- Scratch HOME ------------------------------------------------------------

fn memory_dir(home: &Path) -> PathBuf {
    home.join(".config").join("playbook").join("memory")
}

fn write_fact(home: &Path, file: &str, content: &str) {
    fs::write(memory_dir(home).join(file), content).unwrap();
}

/// Builds the scratch HOME with a git repo and the four seeded facts.
fn seed_home() -> PathBuf {
    seed_home_named("playbook-recall-replay")
}

/// [`seed_home`] in its own folder, so tests that run in parallel do not share one.
fn seed_home_named(name: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("{name}-{}", playbook::testing::run_id()));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(memory_dir(&home)).unwrap();
    let repo = home.join("repo");
    fs::create_dir_all(&repo).unwrap();
    let ok = Command::new("git")
        .args(["-C", repo.to_str().unwrap(), "init", "--quiet"])
        .status()
        .expect("git should be available")
        .success();
    assert!(ok, "scratch repo should initialise");

    let graph = json!({
        "nodes": [
            {"id": "global/code", "file": "code.md", "scope": "global",
             "type": "reference", "name": CODE_FACT,
             "description": "How the symbol store is laid out."},
            {"id": "global/layout", "file": "layout.md", "scope": "global",
             "type": "reference", "name": LAYOUT_FACT,
             "description": "Why the workspace splits its crates."},
            {"id": "global/signing", "file": "signing.md", "scope": "global",
             "type": "feedback", "name": "commit-signing-setup",
             "description": "Sign every commit with the configured key."},
            {"id": "global/release", "file": "release.md", "scope": "global",
             "type": "feedback", "name": "release-checklist-notes",
             "description": "Steps to run before a version tag."},
            {"id": "code:crates/code/", "file": "crates/code/", "scope": "code", "type": "code"}
        ],
        "edges": [
            {"from": "global/code", "to": "code:crates/code/", "relation": "anchors"}
        ]
    })
    .to_string();
    playbook::testing::write_graph_fixture(&memory_dir(&home), &graph);

    write_fact(
        &home,
        "code.md",
        &format!("---\nname: {CODE_FACT}\naliases: [pbcode]\n---\n\n{CODE_MARKER}\n"),
    );
    write_fact(
        &home,
        "layout.md",
        &format!("---\nname: {LAYOUT_FACT}\n---\n\n{LAYOUT_MARKER}\n"),
    );
    write_fact(
        &home,
        "signing.md",
        "---\nname: commit-signing-setup\n---\n\nSIGNINGBODYMARKER\n",
    );
    write_fact(
        &home,
        "release.md",
        "---\nname: release-checklist-notes\n---\n\nRELEASEBODYMARKER\n",
    );
    home
}

// --- Replay ------------------------------------------------------------------

struct Replay {
    prompt: &'static str,
    /// The hook's `additionalContext`, empty when it printed nothing.
    message: String,
}

impl Replay {
    fn hint_lines(&self) -> Vec<&str> {
        self.message
            .lines()
            .filter(|l| l.starts_with(HINT_PREFIX))
            .collect()
    }

    fn fact_count(&self) -> usize {
        self.message.matches(FACT_HEADING).count()
    }
}

fn run_prompt(home: &Path, prompt: &str, session_id: &str) -> String {
    let input = json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": session_id,
        "prompt": prompt
    })
    .to_string();
    let output = Command::new(env!("CARGO_BIN_EXE_playbook"))
        .env_remove("CI")
        .env_remove("PLAYBOOK_HEADLESS")
        .args(["hook", "memory-anchors"])
        .current_dir(fs::canonicalize(home.join("repo")).unwrap())
        .env("HOME", home)
        .env("HOOK_INPUT", &input)
        .stdin(Stdio::null())
        .output()
        .expect("playbook binary should run");
    assert!(output.status.success(), "hook should exit 0 for {prompt:?}");
    let out = String::from_utf8_lossy(&output.stdout);
    let out = out.trim_end_matches('\n');
    if out.is_empty() {
        return String::new();
    }
    let parsed: Value = serde_json::from_str(out).expect("hook output should be JSON");
    parsed["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext should be a string")
        .to_string()
}

/// Removes the scratch HOME on drop, so a panic still cleans up.
struct HomeGuard(PathBuf);

impl Drop for HomeGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Runs every prompt once, each in its own session, and shares the result.
fn replays() -> &'static Vec<Replay> {
    static REPLAYS: OnceLock<Vec<Replay>> = OnceLock::new();
    REPLAYS.get_or_init(|| {
        let guard = HomeGuard(seed_home());
        let home = &guard.0;
        let replays = CASES
            .iter()
            .enumerate()
            .map(|(i, case)| Replay {
                prompt: case.prompt,
                message: run_prompt(home, case.prompt, &format!("replay-{i}")),
            })
            .collect();
        replays
    })
}

// --- Tests -------------------------------------------------------------------

/// The data-driven test: each prompt lands on its planned outcome.
#[test]
fn every_prompt_produces_its_planned_outcome() {
    for (case, replay) in CASES.iter().zip(replays()) {
        let m = &replay.message;
        let hinted = !replay.hint_lines().is_empty();
        let facts = replay.fact_count();
        let ok = match case.expected {
            Outcome::Strong => facts > 0 && !hinted,
            Outcome::Weak => facts > 0 && hinted,
            Outcome::Hint => facts == 0 && hinted,
            Outcome::Silent => m.is_empty(),
        };
        assert!(
            ok,
            "{:?} expected {:?}, got: {m}",
            case.prompt, case.expected
        );
        if let Some(marker) = case.marker {
            assert!(m.contains(marker), "{:?} lacks {marker}: {m}", case.prompt);
        }
    }
}

/// The first prompt must rank the crate-anchored fact first.
#[test]
fn the_crate_prompt_ranks_the_code_fact_first() {
    let replay = &replays()[0];
    let first = replay.message.find(CODE_FACT).expect("code fact injected");
    if let Some(other) = replay.message.find(LAYOUT_FACT) {
        assert!(
            first < other,
            "code fact should rank first: {}",
            replay.message
        );
    }
}

/// Bar 1: a searchable term never meets silence.
#[test]
fn a_prompt_with_a_searchable_term_gets_output() {
    for (case, replay) in CASES.iter().zip(replays()) {
        if case.searchable {
            assert!(
                !replay.message.is_empty(),
                "{:?} got silence",
                replay.prompt
            );
        }
    }
}

/// Bar 2: at most three facts are injected per prompt.
#[test]
fn no_prompt_injects_more_than_three_facts() {
    for replay in replays() {
        assert!(
            replay.fact_count() <= MAX_INJECTED_FACTS,
            "{:?} injected {} facts",
            replay.prompt,
            replay.fact_count()
        );
    }
}

/// Bar 3: every hint line stays within 400 characters.
#[test]
fn every_hint_line_stays_within_the_hint_budget() {
    for replay in replays() {
        for line in replay.hint_lines() {
            let len = line.chars().count();
            assert!(
                len <= MAX_HINT_CHARS,
                "{:?} hint is {len} chars",
                replay.prompt
            );
        }
    }
}

/// Bar 4: a strong prompt carries no `Memory:` line.
#[test]
fn strong_prompts_carry_no_hint_line() {
    for (case, replay) in CASES.iter().zip(replays()) {
        if case.expected == Outcome::Strong {
            assert!(
                replay.hint_lines().is_empty(),
                "{:?} got a hint: {}",
                replay.prompt,
                replay.message
            );
        }
    }
}

/// Bar 5: the whole message stays within the recall cap.
#[test]
fn no_message_exceeds_the_recall_cap() {
    for replay in replays() {
        let len = replay.message.chars().count();
        assert!(
            len <= MAX_MESSAGE_CHARS,
            "{:?} is {len} chars",
            replay.prompt
        );
    }
}

/// A word found only in a fact's body reaches the model through the real hook:
/// the rebuild indexes the body and the hook injects the fact with the weak
/// hint, since a body-only match is never strong.
#[test]
fn a_word_only_a_fact_body_holds_recalls_that_fact_as_a_weak_match() {
    // Arrange
    let guard = HomeGuard(seed_home_named("playbook-recall-body-only"));
    let home = &guard.0;
    let graph = json!({
        "nodes": [{"id": "global/habitat", "file": "habitat.md", "scope": "global",
                   "type": "reference", "name": "habitat-notes",
                   "description": "Where the animals are kept."}],
        "edges": []
    })
    .to_string();
    write_fact(
        home,
        "habitat.md",
        "---\nname: habitat-notes\n---\n\nThe quokka burrows sit by the north fence. HABITATBODYMARKER\n",
    );
    playbook::testing::write_graph_fixture(&memory_dir(home), &graph);

    // Act
    let message = run_prompt(home, "where do the quokka burrows sit", "body-only");

    // Assert
    assert!(
        message.contains("HABITATBODYMARKER"),
        "fact not injected: {message:?}"
    );
    assert!(message.contains(HINT_PREFIX), "no weak hint: {message:?}");
}
