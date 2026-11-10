// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Checks for the `fix` command, and the rule that comments carry no
//! planning references. The shared parser is in `support/auto_parity.rs`.

#[path = "support/auto_parity.rs"]
mod ap;
#[path = "support/auto_parity_fix.rs"]
mod ap_fix;

use ap::*;
use ap_fix::*;
use regex::Regex;

#[test]
fn fix_command_file_exists() {
    // Arrange
    let path = commands_dir().join("fix.md");

    // Act
    let exists = path.is_file();

    // Assert
    assert!(exists, "{} does not exist", path.display());
}

#[test]
fn fix_frontmatter_has_description_tools_and_the_argument_hint() {
    // Arrange
    let file = load("fix");

    // Act
    let description = file.frontmatter_value("description");
    let hint = file.frontmatter_value("argument-hint");
    let tools = file.allowed_tools();

    // Assert
    assert!(
        description.is_some_and(|d| !d.is_empty()),
        "fix needs a non-empty description"
    );
    assert!(
        hint.as_deref()
            .is_some_and(|h| h.contains("[#issue | description] [--auto] [--ask]")),
        "fix argument-hint {hint:?} must contain `[#issue | description] [--auto] [--ask]`"
    );
    for needed in ["Bash", "Read", "Skill"] {
        assert!(
            tools.iter().any(|t| t == needed),
            "fix allowed-tools {tools:?} lacks {needed}"
        );
    }
}

#[test]
fn fix_escalation_names_all_five_rules() {
    // Arrange
    let file = load("fix");
    let section = escalation_section(&file);
    let text = file.text_of(&section);
    let rules = [
        (
            "more than fix.maxFiles files",
            r"(?is)more than[^.]*fix\.maxFiles[^.]*files",
        ),
        (
            "more than fix.maxLines lines, excluding the new test",
            r"(?is)more than[^.]*fix\.maxLines[^.]*excluding the new test",
        ),
        (
            "root cause unclear after two tested hypotheses",
            r"(?is)root cause[^.]*unclear[^.]*two tested hypotheses",
        ),
        (
            "new dependency, schema, config format or public interface",
            r"(?is)new dependency[^.]*schema[^.]*config[- ]format[^.]*public[- ]interface",
        ),
        ("no failing test possible", r"(?i)\bno failing test\b"),
    ];

    // Act
    let missing: Vec<&str> = rules
        .iter()
        .filter(|(_, pattern)| !Regex::new(pattern).expect("rule pattern").is_match(&text))
        .map(|(name, _)| *name)
        .collect();

    // Assert
    assert!(
        missing.is_empty(),
        "the `{}` section does not state these escalation rules: {missing:?}",
        section.title
    );
}

#[test]
fn fix_reads_both_thresholds_through_the_config_get_intent() {
    // Arrange
    let file = load("fix");
    let text = file.lines[file.body_start..].join("\n");

    // Act
    let missing: Vec<&str> = ["fix.maxFiles", "fix.maxLines"]
        .into_iter()
        .filter(|key| {
            !text
                .lines()
                .any(|l| l.contains("Intent: config-get") && l.contains(*key))
        })
        .collect();

    // Assert
    assert!(
        missing.is_empty(),
        "fix never reads the config-get intent for {missing:?}"
    );
}

#[test]
fn fix_escalation_hands_off_to_plan_and_states_which_rule_fired() {
    // Arrange
    let file = load("fix");
    let section = escalation_section(&file);
    let text = file.text_of(&section);
    let states_rule = Regex::new(r"(?is)\bwhich rule (fired|triggered)\b").expect("rule pattern");

    // Act
    let hands_off = text.contains("/playbook:plan");

    // Assert
    assert!(
        hands_off,
        "the `{}` section must hand off to `/playbook:plan`",
        section.title
    );
    assert!(
        states_rule.is_match(&text),
        "the `{}` section must say it states which rule fired",
        section.title
    );
}

#[test]
fn fix_every_ask_sits_behind_an_if_mode_is_ask_branch() {
    // Arrange
    let mut failures = Vec::new();

    for spec in rows(Auto::AsksOnlyInAskMode) {
        let file = load(spec.name);
        let fenced = fence_flags(&file.lines);

        // Act
        for (i, line) in file.lines.iter().enumerate().skip(file.body_start) {
            if fenced[i] || !is_ask(line) {
                continue;
            }

            // Assert
            if !is_behind_ask_branch(&file.lines, file.body_start, i) {
                failures.push(format!(
                    "{}: line {} asks outside an `{ASK_BRANCH}` branch: {}",
                    spec.name,
                    i + 1,
                    line.trim()
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "unconditional asks:\n{}",
        failures.join("\n")
    );
}

#[test]
fn ask_branch_parser_accepts_headings_and_bullets_and_rejects_the_rest() {
    // Arrange
    let lines = |text: &str| -> Vec<String> { text.lines().map(str::to_owned).collect() };
    let behind = [
        "### If mode is ask\nAsk the user which test to keep.",
        "## Step 3\n- If mode is ask, ask the user which test to keep.",
        "## Step 3\n- If mode is ask:\n  - Ask the user which test to keep.",
        "## Step 3\n- If mode is ask:\n  Ask the user which test to keep.",
    ];
    let not_behind = [
        "## Step 3\nAsk the user which test to keep.",
        "## Step 3\n- Ask the user which test to keep.",
        "### If mode is ask\n## Step 4\nAsk the user which test to keep.",
        "- If mode is ask:\n  - Pick one.\n\n- Ask the user which test to keep.",
        "## Step 3\n```\n# if mode is ask\n```\nAsk the user which test to keep.",
    ];
    let ask_index = |l: &[String]| {
        l.iter()
            .position(|x| is_ask(x))
            .expect("fixture has an ask")
    };

    // Act
    let missed: Vec<&&str> = behind
        .iter()
        .filter(|t| {
            let l = lines(t);
            !is_behind_ask_branch(&l, 0, ask_index(&l))
        })
        .collect();
    let false_hits: Vec<&&str> = not_behind
        .iter()
        .filter(|t| {
            let l = lines(t);
            is_behind_ask_branch(&l, 0, ask_index(&l))
        })
        .collect();

    // Assert
    assert!(missed.is_empty(), "parser rejected gated asks: {missed:?}");
    assert!(
        false_hits.is_empty(),
        "parser accepted ungated asks: {false_hits:?}"
    );
}

#[test]
fn fix_opens_its_pull_request_through_the_create_pull_request_skill() {
    // Arrange
    let file = load("fix");
    let fenced = fence_flags(&file.lines);
    let tools = file.allowed_tools();

    // Act
    let names_skill = file.lines[file.body_start..]
        .iter()
        .any(|l| l.contains("/playbook:pr-create"));
    let raw_calls: Vec<usize> = (file.body_start..file.lines.len())
        .filter(|&i| fenced[i] && file.lines[i].contains("gh pr create"))
        .map(|i| i + 1)
        .collect();

    // Assert
    assert!(
        names_skill,
        "fix must open its PR through `/playbook:pr-create`"
    );
    assert!(
        tools.iter().any(|t| t == "Skill"),
        "fix allowed-tools {tools:?} lacks Skill, so it cannot invoke the skill"
    );
    assert!(
        raw_calls.is_empty(),
        "fix runs `gh pr create` directly on lines {raw_calls:?}"
    );
}

#[test]
fn planning_reference_scan_flags_references_and_leaves_plain_comments() {
    // Arrange
    let flagged = [
        "// ported in WU-16",
        "# see gh-123 for the design",
        "<!-- Work Unit 3 only -->",
        "# fixes #123",
        "// the brief says so",
    ];
    let clean = [
        "// a section ends at the next heading of the same level",
        "# Set each flag from $ARGUMENTS: true when the flag was passed",
        "# 1) read the mode first",
        "<!-- keep in sync with the help block -->",
    ];

    // Act
    let missed: Vec<&&str> = flagged.iter().filter(|t| !references_planning(t)).collect();
    let false_hits: Vec<&&str> = clean.iter().filter(|t| references_planning(t)).collect();

    // Assert
    assert!(missed.is_empty(), "scan missed references: {missed:?}");
    assert!(
        false_hits.is_empty(),
        "scan flagged plain comments: {false_hits:?}"
    );
}

#[test]
fn comments_in_this_test_and_the_commands_carry_no_planning_references() {
    // Arrange
    let mut offenders = Vec::new();
    let own_source = [
        include_str!("commands_auto_parity.rs"),
        include_str!("commands_fix.rs"),
        include_str!("support/auto_parity.rs"),
        include_str!("support/auto_parity_auto.rs"),
        include_str!("support/auto_parity_fix.rs"),
    ]
    .join("\n");
    assert!(
        COMMANDS
            .iter()
            .any(|spec| !load(spec.name).comments().is_empty()),
        "the comment extractor found no comment in any command"
    );

    // Act
    for (i, line) in own_source.lines().enumerate() {
        if line.trim_start().starts_with("//") && references_planning(line) {
            offenders.push(format!(
                "the plugin check sources, line {}: {}",
                i + 1,
                line.trim()
            ));
        }
    }
    for spec in COMMANDS {
        let file = load(spec.name);
        for (i, text) in file.comments() {
            if references_planning(&text) {
                offenders.push(format!(
                    "commands/{}.md:{}: {}",
                    spec.name,
                    i + 1,
                    text.trim()
                ));
            }
        }
    }

    // Assert
    assert!(
        offenders.is_empty(),
        "comments reference planning artefacts:\n{}",
        offenders.join("\n")
    );
}
