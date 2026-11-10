// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The shipped system prompt follows its own rules, stays small, and does not
//! restate facts that live elsewhere (a model name, an effort variant), which
//! is how it used to contradict `agents/*.md`.

use std::fs;
use std::path::Path;

/// Characters. Loaded on every turn, so growth should be a deliberate edit.
const PROMPT_BUDGET_CHARS: usize = 10_100;

const CODE_SEARCH_RULE: &str = "**Code search**: for any code question, run `playbook code status` first, and `playbook code index` when it says stale or missing, then use `playbook code context|search|symbol|refs|outline|read`. Use Grep and Glob only when the binary or the index is unavailable. Put this rule in every `Explore` prompt, or search yourself.";

const EXPLORE_CLAUSE: &str = "Put this rule in every `Explore` prompt, or search yourself.";

fn prompt() -> String {
    let path = Path::new(env!("PLAYBOOK_REPO_ROOT")).join("seed/prompts/SYSTEM_PROMPT.md");
    fs::read_to_string(path).expect("shipped system prompt")
}

#[test]
fn the_prompt_carries_the_worktree_recovery_rule() {
    // Arrange
    let text = prompt();

    // Act and assert
    for needle in ["EnterWorktree", "ExitWorktree", "isolation: worktree"] {
        assert!(text.contains(needle), "missing {needle}");
    }
}

#[test]
fn the_prompt_stays_under_its_budget() {
    let len = prompt().chars().count();

    assert!(
        len <= PROMPT_BUDGET_CHARS,
        "{len} chars, budget {PROMPT_BUDGET_CHARS}"
    );
}

#[test]
fn the_prompt_does_not_require_task_tools_that_many_builds_lack() {
    // `TaskStop` is the one task tool the prompt names; the other task tools stay
    // conditional because many builds lack them.
    let text = prompt();

    for tool in [
        "TaskCreate",
        "TaskList",
        "TaskGet",
        "TaskOutput",
        "TaskUpdate",
    ] {
        assert!(!text.contains(tool), "names `{tool}`, which may be missing");
    }
    assert!(!text.contains("`Task*`"), "tells the model to use `Task*`");
    assert!(
        text.contains("use task tools only if listed"),
        "task tools must be conditional"
    );
    assert!(text.contains("`TaskStop`"), "the TaskStop rule must stay");
}

#[test]
fn the_prompt_uses_no_em_or_en_dash() {
    let text = prompt();

    assert!(!text.contains('\u{2014}') && !text.contains('\u{2013}'));
}

#[test]
fn the_prompt_never_uses_a_so_consequence_clause() {
    let text = prompt();

    let hits: Vec<&str> = text.lines().filter(|l| l.contains(", so ")).collect();

    assert!(hits.is_empty(), "breaks its own rule: {hits:?}");
}

#[test]
fn the_prompt_does_not_restate_models_or_effort_variants() {
    // The agent files and `playbook route` own these. A copy here drifts.
    let text = prompt().to_lowercase();

    for word in ["opus", "sonnet", "haiku", "-xhigh", "-low", "high effort"] {
        assert!(
            !text.contains(word),
            "restates `{word}`: point at agents/ instead"
        );
    }
}

#[test]
fn the_prompt_no_longer_names_the_retired_guard() {
    assert!(!prompt().contains("no-slop-guard"));
}

#[test]
fn the_prompt_points_at_the_files_that_own_routing_and_the_hooks_that_enforce_rules() {
    let text = prompt();

    for needle in [
        "playbook route",
        "playbook:delegating-subagents",
        "policy-guard",
        "commit-message-sanitizer",
    ] {
        assert!(text.contains(needle), "missing `{needle}`");
    }
}

#[test]
fn the_prompt_requires_a_memory_search_before_claiming_no_knowledge() {
    // Arrange
    let text = prompt();

    // Act
    let missing: Vec<&str> = ["search memory and the repo", "name the sources you checked"]
        .into_iter()
        .filter(|needle| !text.contains(needle))
        .collect();

    // Assert
    assert!(
        missing.is_empty(),
        "missing the search-before-no-knowledge rule: {missing:?}"
    );
}

#[test]
fn the_prompt_states_the_code_search_rule() {
    // Arrange
    let text = prompt();

    // Act
    let has_rule = text.contains(CODE_SEARCH_RULE);
    let stale: Vec<&str> = ["LSP", "ToolSearch", "Grep and Glob for search"]
        .into_iter()
        .filter(|needle| text.contains(needle))
        .collect();

    // Assert
    assert!(has_rule, "missing the code search rule, verbatim");
    assert!(
        stale.is_empty(),
        "still holds the old search advice: {stale:?}"
    );
}

#[test]
fn the_prompt_tells_every_explore_prompt_to_carry_the_rule() {
    // Arrange
    let text = prompt();

    // Act
    let has_clause = text.contains(EXPLORE_CLAUSE);

    // Assert
    assert!(has_clause, "missing the Explore clause");
}
