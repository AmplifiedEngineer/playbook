// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The skill call graph guard. A recipe never invokes another recipe, and a
//! rule skill never loads a skill, except for the rows of a list that only
//! shrinks. The pure checks are in `support/call_graph.rs`.

#[path = "support/call_graph.rs"]
mod call_graph;
#[path = "support/markdown.rs"]
mod markdown;
#[path = "support/skill_kinds.rs"]
mod skill_kinds;

use call_graph::*;
use skill_kinds::{skills_of, SkillKind};
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

const LIST: &str = include_str!("call_graph_allowlist.txt");

/// The least files the real scan reads, so an empty scan fails.
const MIN_COMMANDS: usize = 14;
const MIN_SKILLS: usize = 11;
const MIN_AGENTS: usize = 11;
const MIN_RULE_LOAD_LINES: usize = 30;

fn plugin_root() -> PathBuf {
    PathBuf::from(env!("PLAYBOOK_PLUGIN_ROOT"))
}

fn recipes(files: &[File]) -> Vec<String> {
    let mut out: Vec<String> = files
        .iter()
        .filter_map(|f| f.path.strip_prefix("commands/")?.strip_suffix(".md"))
        .map(str::to_owned)
        .collect();
    out.extend(skills_of(SkillKind::Recipe).into_iter().map(str::to_owned));
    out
}

fn file<'a>(path: &'a str, text: &'a str) -> File<'a> {
    File { path, text }
}

fn found(files: &[File<'_>]) -> BTreeSet<Row> {
    edges(files, &recipes(files))
}

fn listed(rows: &[(RowKind, &str, &str)]) -> BTreeSet<Row> {
    rows.iter()
        .map(|(kind, f, t)| Row::new(*kind, f, t))
        .collect()
}

#[test]
fn a_line_is_an_invocation_a_pointer_a_load_or_a_mention() {
    // Arrange
    use Class::*;
    use Source::*;
    let cases: [(&str, &str, bool, Source, Class); 12] = [
        ("invoke the `playbook:pr-create` skill with the Skill tool", "pr-create", false, Recipe, Invoke),
        ("Invoke playbook:pr-create", "pr-create", false, Recipe, Invoke),
        ("run `/playbook:review <n> --self --deep`.", "review", false, Recipe, Invoke),
        ("Next: invoke playbook:finish-pull-request for this PR.", "finish-pull-request", false, Recipe, Invoke),
        ("load the `playbook:pr-create` skill", "pr-create", false, Recipe, Invoke),
        ("Tell the user to run `/playbook:plan`.", "plan", false, Recipe, Pointer),
        ("Tell the user to run `/playbook:plan`, then invoke playbook:pr-create with the Skill tool", "pr-create", false, Recipe, Invoke),
        ("commit each WU with `/playbook:commit`", "commit", false, Recipe, Invoke),
        ("load the `playbook:writing-style` skill", "writing-style", true, Recipe, Load),
        ("follows `playbook:writing-style`", "writing-style", true, RuleSkill, Load),
        ("dispatched by `/playbook:review`", "review", false, Agent, Mention),
        ("invoke the `playbook:commit` skill", "commit", false, Agent, Invoke),
    ];
    let mut problems = Vec::new();

    // Act
    for (text, target, rule, source, want) in cases {
        let got = classify_line(text, target, rule, source);
        if got != Some(want) {
            problems.push(format!("{text:?}: want {want:?}, got {got:?}"));
        }
    }
    let longer = classify_line(
        "playbook:review-triage runs",
        "review",
        false,
        Source::Recipe,
    );
    let other = classify_line("nothing here", "review", false, Source::Recipe);
    let tell_list = classify_line(
        "Tell the user to run `/playbook:plan`, or `/playbook:adr` for a decision.",
        "adr",
        false,
        Source::Recipe,
    );

    // Assert
    assert!(problems.is_empty(), "{problems:#?}");
    assert_eq!(longer, None, "a longer name is another name");
    assert_eq!(other, None);
    assert_eq!(tell_list, Some(Class::Pointer));
}

#[test]
fn a_recipe_invoking_a_recipe_fails_unless_listed() {
    // Arrange
    let fix = file(
        "commands/fix.md",
        "Invoke the `playbook:pr-create` skill with the Skill tool.\n",
    );
    let pr_create = file("commands/pr-create.md", "Open the PR.\n");
    let doctor = file(
        "commands/doctor.md",
        "Re-running /playbook:setup is safe.\n",
    );
    let setup = file("commands/setup.md", "Wire things up.\n");
    let pointer = file(
        "commands/plan.md",
        "Tell the user to run `/playbook:implement` when ready.\n",
    );
    let implement = file("commands/implement.md", "Run the plan.\n");
    let agent_verb = file(
        "agents/critic.md",
        "Invoke the `playbook:pr-create` skill with the Skill tool.\n",
    );
    let agent_plain = file(
        "agents/writer.md",
        "It commits through `/playbook:commit`.\n",
    );
    let commit = file("commands/commit.md", "Commit.\n");

    // Act
    let fix_set = [fix, pr_create];
    let fix_empty = check(&found(&fix_set), &BTreeSet::new());
    let fix_listed = check(
        &found(&fix_set),
        &listed(&[(RowKind::Invoke, "commands/fix.md", "pr-create")]),
    );
    let doctor_set = [doctor, setup];
    let doctor_empty = check(&found(&doctor_set), &BTreeSet::new());
    let doctor_listed = check(
        &found(&doctor_set),
        &listed(&[(RowKind::Mention, "commands/doctor.md", "setup")]),
    );
    let pointer_set = [pointer, implement];
    let pointer_empty = check(&found(&pointer_set), &BTreeSet::new());
    let agent_set = [agent_verb, file("commands/pr-create.md", "x\n")];
    let agent_with_row = check(
        &found(&agent_set),
        &listed(&[(RowKind::Mention, "agents/critic.md", "pr-create")]),
    );
    let agent_invoke_row = check(
        &found(&agent_set),
        &listed(&[(RowKind::Invoke, "agents/critic.md", "pr-create")]),
    );
    let plain_set = [agent_plain, commit];
    let agent_plain_found = check(&found(&plain_set), &BTreeSet::new());

    // Assert
    assert_eq!(fix_empty.len(), 1, "{fix_empty:?}");
    assert!(fix_listed.is_empty(), "{fix_listed:?}");
    assert_eq!(doctor_empty.len(), 1, "{doctor_empty:?}");
    assert!(doctor_listed.is_empty(), "{doctor_listed:?}");
    assert!(
        pointer_empty.is_empty(),
        "a pointer needs no row: {pointer_empty:?}"
    );
    assert!(
        !agent_with_row.is_empty(),
        "an agent verb fails with a mention row"
    );
    assert!(
        !agent_invoke_row.is_empty(),
        "an agent verb fails with an invoke row"
    );
    assert!(agent_plain_found.is_empty(), "{agent_plain_found:?}");
}

#[test]
fn a_rule_skill_loading_a_skill_fails_unless_listed() {
    // Arrange
    let loads = file(
        "skills/grounding-research/SKILL.md",
        "You MUST load the `playbook:writing-style` skill first.\n",
    );
    let reader = file(
        "skills/grounding-research/SKILL.md",
        "Read by `/playbook:review` before it writes.\n",
    );
    let writing = file("skills/writing-style/SKILL.md", "Style rules.\n");
    let review = file("commands/review.md", "Review.\n");

    // Act
    let loads_set = [loads, writing];
    let empty = check(&found(&loads_set), &BTreeSet::new());
    let with_row = check(
        &found(&loads_set),
        &listed(&[(
            RowKind::RuleLoad,
            "skills/grounding-research/SKILL.md",
            "writing-style",
        )]),
    );
    let reader_set = [reader, review];
    let mention = check(&found(&reader_set), &BTreeSet::new());

    // Assert
    assert_eq!(empty.len(), 1, "{empty:?}");
    assert!(with_row.is_empty(), "{with_row:?}");
    assert!(
        mention.is_empty(),
        "a mention in a rule skill needs no row: {mention:?}"
    );
}

#[test]
fn a_reference_file_follows_the_recipe_rule() {
    // Arrange
    let deep = file(
        "skills/grounding-review/references/deep-pass.md",
        "Then run `/playbook:pr-comments` on the PR.\n",
    );
    let loads = file(
        "skills/grounding-review/references/deep-pass.md",
        "Load `playbook:writing-style` for the comment text.\n",
    );
    let comments = file("commands/pr-comments.md", "Walk the threads.\n");

    // Act
    let invokes = [deep, file("commands/pr-comments.md", "x\n")];
    let invoke_empty = check(&found(&invokes), &BTreeSet::new());
    let load_set = [loads, comments];
    let load_empty = check(&found(&load_set), &BTreeSet::new());

    // Assert
    assert_eq!(invoke_empty.len(), 1, "{invoke_empty:?}");
    assert!(load_empty.is_empty(), "{load_empty:?}");
}

#[test]
fn a_list_row_the_scan_no_longer_finds_fails() {
    // Arrange
    let fix = file("commands/fix.md", "Do the work.\n");
    let research = file("skills/grounding-research/SKILL.md", "Facts.\n");
    let set = [fix, research];
    let rows = [
        (RowKind::Invoke, "commands/fix.md", "pr-finish"),
        (RowKind::Mention, "commands/fix.md", "pr-create"),
        (
            RowKind::RuleLoad,
            "skills/grounding-research/SKILL.md",
            "writing-style",
        ),
    ];

    // Act
    let problems = check(&found(&set), &listed(&rows));

    // Assert
    assert_eq!(problems.len(), 3, "{problems:?}");
    assert!(problems
        .iter()
        .all(|p| p.starts_with("the scan no longer finds")));
}

#[test]
fn the_list_parses_and_rejects_a_bad_row() {
    // Arrange
    let good = "# comment\n\ninvoke commands/fix.md pr-create\nmention commands/x.md plan\n";
    let bad = [
        "keep commands/x.md plan",
        "invoke commands/x.md",
        "invoke a b c",
    ];

    // Act
    let rows = parse_list(good);
    let errors: Vec<_> = bad.iter().map(|b| parse_list(b)).collect();

    // Assert
    assert_eq!(rows.map(|r| r.len()), Ok(2));
    assert!(errors.iter().all(Result::is_err), "{errors:?}");
}

#[test]
fn the_real_tree_matches_the_list() {
    // Arrange
    let mut paths = Vec::new();
    for dir in ["commands", "skills", "agents"] {
        markdown::markdown_files(&plugin_root().join(dir), &mut paths);
    }
    paths.sort();
    let texts: Vec<(String, String)> = paths
        .iter()
        .map(|p| {
            let rel = p
                .strip_prefix(plugin_root())
                .expect("under the plugin root")
                .to_string_lossy()
                .replace('\\', "/");
            (rel, fs::read_to_string(p).expect("a readable file"))
        })
        .filter(|(rel, text)| !(rel.starts_with("commands/") && is_stub(text)))
        .collect();
    let files: Vec<File> = texts.iter().map(|(p, t)| file(p, t)).collect();
    let count = |dir: &str| files.iter().filter(|f| f.path.starts_with(dir)).count();
    let skills: BTreeSet<&str> = files
        .iter()
        .filter_map(|f| f.path.strip_prefix("skills/")?.split('/').next())
        .collect();
    let want = parse_list(LIST).expect("the list parses");

    // Act
    let problems = check(&edges(&files, &recipes(&files)), &want);

    // Assert
    assert!(problems.is_empty(), "{problems:#?}");
    assert!(
        count("commands/") >= MIN_COMMANDS,
        "{} commands",
        count("commands/")
    );
    assert!(skills.len() >= MIN_SKILLS, "{} skills", skills.len());
    assert!(
        count("agents/") >= MIN_AGENTS,
        "{} agents",
        count("agents/")
    );
    assert!(
        rule_load_lines(&files) >= MIN_RULE_LOAD_LINES,
        "{} rule skill load lines",
        rule_load_lines(&files)
    );
}

/// A deprecated stub hides itself from the model.
fn is_stub(text: &str) -> bool {
    text.strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .is_some_and(|(front, _)| front.lines().any(|l| l == "disable-model-invocation: true"))
}
