// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A rule skill never loads another skill, so the recipe or agent that needs
//! two rule skills names both loads itself. This pins that each caller's text
//! holds its loads. Whether the model then loads them is runtime behavior.

use std::fs;
use std::path::PathBuf;

/// One caller: its file, the heading its loads must sit under (`None` for the
/// whole file) and the skills it must load.
struct Caller {
    file: &'static str,
    section: Option<&'static str>,
    loads: &'static [&'static str],
}

const CALLERS: &[Caller] = &[
    Caller {
        file: "agents/critic.md",
        section: None,
        loads: &["writing-style", "grounding-research"],
    },
    Caller {
        file: "agents/fact-checker.md",
        section: None,
        loads: &["writing-style", "grounding-research"],
    },
    Caller {
        file: "commands/plan.md",
        section: None,
        loads: &["writing-style", "grounding-research", "atlassian"],
    },
    Caller {
        file: "commands/adr.md",
        section: None,
        loads: &["writing-style", "grounding-research"],
    },
    Caller {
        file: "skills/grounding-review/references/quick-pass.md",
        section: Some("Step 4"),
        loads: &["writing-style"],
    },
    Caller {
        file: "skills/grounding-review/references/deep-pass.md",
        section: Some("Step 5"),
        loads: &["writing-style"],
    },
    Caller {
        file: "commands/pr-comments.md",
        section: Some("Discipline: writing the reply"),
        loads: &["writing-style"],
    },
    Caller {
        file: "commands/learn.md",
        section: None,
        loads: &["atlassian", "writing-style"],
    },
    Caller {
        file: "commands/fix.md",
        section: None,
        loads: &["systematic-debugging", "engineering-standards"],
    },
    Caller {
        file: "commands/implement.md",
        section: None,
        loads: &[
            "systematic-debugging",
            "engineering-standards",
            "grounding-research",
        ],
    },
    Caller {
        file: "agents/implementer.md",
        section: None,
        loads: &[
            "systematic-debugging",
            "engineering-standards",
            "grounding-research",
        ],
    },
];

/// The text under the `##` heading that starts with `title`, cut at the next
/// `##` heading.
fn section_of(text: &str, title: &str) -> Option<String> {
    let mut out = String::new();
    let mut inside = false;
    let mut found = false;
    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            inside = heading.starts_with(title);
            found |= inside;
        }
        if inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    found.then_some(out)
}

/// The loads a caller's text lacks, by name.
fn missing(text: &str, caller: &Caller) -> Vec<String> {
    let scope = match caller.section {
        None => text.to_owned(),
        Some(title) => match section_of(text, title) {
            Some(scope) => scope,
            None => return vec![format!("no section `{title}`")],
        },
    };
    caller
        .loads
        .iter()
        .filter(|skill| !scope.contains(&format!("playbook:{skill}")))
        .map(|skill| format!("{} does not name playbook:{skill}", caller.file))
        .collect()
}

fn read(rel: &str) -> String {
    let path = PathBuf::from(env!("PLAYBOOK_PLUGIN_ROOT")).join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn a_caller_missing_one_load_fails_naming_it() {
    // Arrange
    let both = Caller {
        file: "agents/x.md",
        section: None,
        loads: &["writing-style", "grounding-research"],
    };
    let step = Caller {
        file: "commands/y.md",
        section: Some("Step 4"),
        loads: &["writing-style"],
    };

    // Act
    let ok = missing(
        "Load `playbook:writing-style` and `playbook:grounding-research`.",
        &both,
    );
    let lacks = missing("Load `playbook:grounding-research`.", &both);
    let elsewhere = missing(
        "## Step 3\nLoad `playbook:writing-style`.\n## Step 4\nPost.\n",
        &step,
    );
    let inside = missing("## Step 4\nLoad `playbook:writing-style`.\n", &step);
    let no_section = missing("## Step 3\nText.\n", &step);

    // Assert
    assert!(ok.is_empty(), "{ok:?}");
    assert_eq!(
        lacks,
        vec!["agents/x.md does not name playbook:writing-style"]
    );
    assert_eq!(elsewhere.len(), 1, "{elsewhere:?}");
    assert!(inside.is_empty(), "{inside:?}");
    assert_eq!(no_section, vec!["no section `Step 4`"]);
}

#[test]
fn rule_skill_callers_name_both_loads() {
    // Arrange
    let mut problems = Vec::new();

    // Act
    for caller in CALLERS {
        problems.extend(missing(&read(caller.file), caller));
    }
    let delegating = read("skills/delegating-subagents/SKILL.md");

    // Assert
    assert!(problems.is_empty(), "{problems:#?}");
    assert!(
        delegating.contains("`playbook code context`"),
        "delegating-subagents lost its own copy of the code search rule"
    );
}
