// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A command that can spawn agents reads the user's effort ceiling in Step 0,
//! so the agents it picks stay within `maxEffortLevel` and the per-component
//! keys (ADR-0017). The check parses the Step 0 section of each command file.

use std::fs;
use std::path::PathBuf;

fn commands_dir() -> PathBuf {
    PathBuf::from(env!("PLAYBOOK_PLUGIN_ROOT")).join("commands")
}

/// The `allowed-tools:` frontmatter line, or an empty string.
fn allowed_tools(text: &str) -> &str {
    text.lines()
        .take_while(|l| !l.is_empty() || true)
        .skip(1)
        .take_while(|l| *l != "---")
        .find_map(|l| l.strip_prefix("allowed-tools:"))
        .unwrap_or("")
}

/// A deprecated stub hides itself from the model and runs the new command, so
/// the new command's own Step 0 covers the run.
fn is_stub(text: &str) -> bool {
    text.lines()
        .skip(1)
        .take_while(|l| *l != "---")
        .any(|l| l == "disable-model-invocation: true")
}

/// The body of the `Step 0` section: from its heading to the next heading of
/// the same or a higher level.
fn step_zero(text: &str) -> Option<String> {
    let mut lines = text.lines();
    let level = loop {
        let line = lines.next()?;
        if let Some(rest) = line.strip_prefix('#') {
            let hashes = 1 + rest.chars().take_while(|c| *c == '#').count();
            if line
                .trim_start_matches('#')
                .trim_start()
                .starts_with("Step 0")
            {
                break hashes;
            }
        }
    };
    let mut body = String::new();
    for line in lines {
        let hashes = line.chars().take_while(|c| *c == '#').count();
        if hashes > 0 && hashes <= level && line.chars().nth(hashes) == Some(' ') {
            break;
        }
        body.push_str(line);
        body.push('\n');
    }
    Some(body)
}

#[test]
fn every_command_that_spawns_agents_reads_the_effort_ceiling_in_step_zero() {
    let mut checked = 0;
    for entry in fs::read_dir(commands_dir()).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let text = fs::read_to_string(&path).unwrap();
        if is_stub(&text) || !allowed_tools(&text).split(',').any(|t| t.trim() == "Agent") {
            continue;
        }
        let step = step_zero(&text).unwrap_or_else(|| panic!("{name}: no Step 0 section"));
        let old = format!("playbook run-context --command {name}");
        assert!(
            step.contains(&old)
                || step.contains("Intent: run-mode")
                || step.contains("references/run-context.md"),
            "{name}: Step 0 must read the run mode (Intent: run-mode)"
        );
        assert!(
            step.contains("delegating-subagents"),
            "{name}: Step 0 must point at the delegating-subagents skill"
        );
        checked += 1;
    }
    assert!(
        checked >= 5,
        "found only {checked} commands that spawn agents"
    );
}

#[test]
fn the_resolved_names_are_real_components() {
    let root = PathBuf::from(env!("PLAYBOOK_PLUGIN_ROOT"));
    for name in ["adr", "implement", "learn", "plan", "review"] {
        assert!(
            root.join("commands").join(format!("{name}.md")).is_file(),
            "{name}"
        );
    }
}

#[test]
fn the_quick_pass_holds_its_reviewer_to_medium() {
    let text = fs::read_to_string(
        PathBuf::from(env!("PLAYBOOK_PLUGIN_ROOT"))
            .join("skills/grounding-review/references/quick-pass.md"),
    )
    .unwrap();
    assert!(
        (text.contains("Intent: effort-resolve") && text.contains("--cap medium"))
            || text.contains("playbook effort resolve agents reviewer --cap medium --json"),
        "the quick pass must resolve its reviewer with a medium cap"
    );
    assert!(
        text.contains("`playbook:reviewer`"),
        "the quick pass must name the base reviewer, which ships at medium"
    );
}

#[test]
fn review_runs_at_medium_and_holds_the_union_of_the_pass_tools() {
    let text = fs::read_to_string(commands_dir().join("review.md")).unwrap();
    let tools: Vec<&str> = allowed_tools(&text).split(',').map(str::trim).collect();
    let want = [
        "Bash",
        "Read",
        "Grep",
        "Glob",
        "Write",
        "Agent",
        "SendMessage",
        "TaskStop",
        "Skill",
        "AskUserQuestion",
    ];
    assert_eq!(tools, want, "review.md allowed-tools");
    assert!(text.contains("\nmodel: sonnet\n"), "review.md model");
    assert!(text.contains("\neffort: medium\n"), "review.md effort");
}

const ROUTE_KINDS: [&str; 6] = [
    "mechanical",
    "classify",
    "check",
    "review",
    "implement",
    "design",
];

#[test]
fn every_command_that_spawns_agents_calls_route_and_names_the_gate() {
    let mut checked = 0;
    for entry in fs::read_dir(commands_dir()).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let text = fs::read_to_string(&path).unwrap();
        if is_stub(&text) || !allowed_tools(&text).split(',').any(|t| t.trim() == "Agent") {
            continue;
        }
        let step = step_zero(&text).unwrap_or_else(|| panic!("{name}: no Step 0 section"));
        assert!(
            step.contains("Intent: route-model")
                || ROUTE_KINDS
                    .iter()
                    .any(|k| step.contains(&format!("playbook route {k} --json"))),
            "{name}: Step 0 must name the route-model intent"
        );
        for word in [
            "routing.escalate",
            "ask",
            "downgraded",
            "auto",
            "log the assumption",
        ] {
            assert!(
                step.contains(word),
                "{name}: the Step 0 routing gate sentence must mention `{word}`"
            );
        }
        checked += 1;
    }
    assert!(
        checked >= 5,
        "found only {checked} commands that spawn agents"
    );
}

#[test]
fn the_delegating_skill_documents_the_escalation_gate() {
    let skill =
        PathBuf::from(env!("PLAYBOOK_PLUGIN_ROOT")).join("skills/delegating-subagents/SKILL.md");
    let text = fs::read_to_string(skill).unwrap();
    assert!(
        text.contains("routing.escalate"),
        "delegating-subagents: must name `routing.escalate`"
    );
    for value in ["`ask`", "`auto`", "`deny`"] {
        assert!(
            text.contains(value),
            "delegating-subagents: must document {value}"
        );
    }
}
