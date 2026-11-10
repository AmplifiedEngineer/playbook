// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Two review regressions. #593: the publish question appeared with no
//! findings printed above it. #595: a run called a tool named `bash`, and tool
//! names are case-sensitive.

use std::fs;
use std::path::Path;

fn read(rel: &str) -> String {
    fs::read_to_string(Path::new(env!("PLAYBOOK_PLUGIN_ROOT")).join(rel))
        .unwrap_or_else(|e| panic!("{rel}: {e}"))
}

const BASH_RUNNERS: [&str; 7] = [
    "commands/implement.md",
    "commands/commit.md",
    "commands/quick-review.md",
    "commands/deep-review.md",
    "commands/learn-project.md",
    "commands/address-pr-comments.md",
    "commands/adr.md",
];

#[test]
fn findings_are_printed_before_the_publish_question() {
    for (file, question) in [
        ("commands/quick-review.md", "**Q1**: \"Post which findings"),
        ("commands/deep-review.md", "- **Q1:** \"Post which findings"),
    ] {
        let text = read(file).to_ascii_lowercase();
        let question = question.to_ascii_lowercase();
        let rule = text
            .find("print every finding first (must)")
            .unwrap_or_else(|| panic!("{file} lacks the print-first rule"));
        let q = text
            .find(&question)
            .unwrap_or_else(|| panic!("{file} lacks the publish question"));
        assert!(rule < q, "{file}: the rule must come before the question");
    }
}

#[test]
fn the_print_rule_covers_codes_location_and_zero_findings() {
    for file in ["commands/quick-review.md", "commands/deep-review.md"] {
        let text = read(file);
        let at = text
            .to_ascii_lowercase()
            .find("print every finding first (must)")
            .unwrap();
        let rule = &text[at..at + 520];
        for needle in ["F1", "`file:line`", "zero findings"] {
            assert!(rule.contains(needle), "{file}: rule lacks {needle}");
        }
    }
}

#[test]
fn no_prose_names_a_lowercase_bash_tool() {
    for file in BASH_RUNNERS {
        for (i, line) in read(file).lines().enumerate() {
            let lower = line.to_ascii_lowercase();
            assert!(
                !lower.contains("`bash` tool") || line.contains("`Bash` tool"),
                "{file}:{}: tool name must be `Bash`",
                i + 1
            );
            assert!(!line.contains("\"bash\" tool") && !line.contains("bash tool call"));
        }
    }
}

#[test]
fn agent_tool_lists_use_the_exact_case() {
    let known = [
        "Read",
        "Grep",
        "Glob",
        "Edit",
        "Write",
        "Bash",
        "Skill",
        "WebFetch",
        "WebSearch",
        "NotebookEdit",
    ];
    let dir = Path::new(env!("PLAYBOOK_PLUGIN_ROOT")).join("agents");
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let text = fs::read_to_string(entry.path()).unwrap();
        let Some(line) = text.lines().find(|l| l.starts_with("tools:")) else {
            continue;
        };
        for tool in line["tools:".len()..].split(',').map(str::trim) {
            assert!(
                known.contains(&tool),
                "{}: unknown or wrong-case tool '{tool}'",
                entry.path().display()
            );
        }
    }
}

#[test]
fn agents_with_bash_name_the_capital_b_tool_in_the_body() {
    let dir = Path::new(env!("PLAYBOOK_PLUGIN_ROOT")).join("agents");
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let text = fs::read_to_string(entry.path()).unwrap();
        let has_bash = text
            .lines()
            .any(|l| l.starts_with("tools:") && l.contains("Bash"));
        if has_bash {
            let body = text.splitn(3, "---").nth(2).unwrap_or("");
            assert!(
                body.contains("`Bash`"),
                "{}: body must name the `Bash` tool",
                entry.path().display()
            );
        }
    }
}

#[test]
fn no_prose_says_a_lowercase_bash_call_or_step() {
    for dir in ["agents", "commands"] {
        let d = Path::new(env!("PLAYBOOK_PLUGIN_ROOT")).join(dir);
        for entry in fs::read_dir(d).unwrap().flatten() {
            let text = fs::read_to_string(entry.path()).unwrap();
            for (i, line) in text.lines().enumerate() {
                for bad in ["bash calls", "bash step", "bash tool"] {
                    assert!(
                        !line.contains(bad),
                        "{}:{}: write `Bash`, not '{bad}'",
                        entry.path().display(),
                        i + 1
                    );
                }
            }
        }
    }
}

/// Claude Code tool names that no longer exist. Text that names one sends an
/// agent to a tool it cannot call.
const RETIRED_TOOLS: [&str; 4] = [
    "TaskOutput",
    "KillShell",
    "MCPSearch",
    "ListMcpResourceTool",
];

fn markdown_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            markdown_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}

#[test]
fn no_plugin_or_docs_text_names_a_retired_tool() {
    let mut files = Vec::new();
    markdown_files(Path::new(env!("PLAYBOOK_PLUGIN_ROOT")), &mut files);
    markdown_files(
        &Path::new(env!("PLAYBOOK_REPO_ROOT")).join("docs"),
        &mut files,
    );
    assert!(!files.is_empty(), "no markdown files found");
    for file in files {
        let text = fs::read_to_string(&file).unwrap();
        for (i, line) in text.lines().enumerate() {
            for tool in RETIRED_TOOLS {
                let named = line.match_indices(tool).any(|(at, _)| {
                    !line[at + tool.len()..].starts_with(|c: char| c.is_ascii_alphanumeric())
                });
                assert!(
                    !named,
                    "{}:{}: names `{tool}`, which Claude Code no longer ships",
                    file.display(),
                    i + 1
                );
            }
        }
    }
}
