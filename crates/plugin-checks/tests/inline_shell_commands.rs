// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Lint for inline shell blocks (` ```! ` fences) in plugin commands and
//! skills. Claude Code runs them before the model sees the text, substitutes
//! `$ARGUMENTS` into the shell text, and aborts the whole invocation when a
//! command is not allowed or exits non zero. So every block must pass its
//! arguments through a quoted heredoc with the one shared delimiter, and every
//! command must be covered by the file's `allowed-tools`.

use std::fs;
use std::path::{Path, PathBuf};

const DELIMITER: &str = "PLAYBOOK_ARGS_END_7Q2XK9";

fn plugin_root() -> PathBuf {
    PathBuf::from(env!("PLAYBOOK_PLUGIN_ROOT"))
}

fn markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            markdown_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}

/// The text of each inline block of `text`.
fn blocks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in text.lines() {
        match (&mut current, line.trim_end()) {
            (None, "```!") => current = Some(Vec::new()),
            (Some(lines), "```") => {
                out.push(lines.join("\n"));
                current = None;
            }
            (Some(lines), _) => lines.push(line),
            (None, _) => {}
        }
    }
    out
}

fn allowed_tools(text: &str) -> String {
    text.lines()
        .skip(1)
        .take_while(|l| *l != "---")
        .find_map(|l| l.strip_prefix("allowed-tools:"))
        .unwrap_or("")
        .trim()
        .to_string()
}

/// The problems of one file's blocks.
fn problems(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let allowed = allowed_tools(text);
    for block in blocks(text) {
        let first = block.lines().next().unwrap_or("");
        if !block.contains(&format!("<<'{DELIMITER}'")) {
            out.push(format!("block does not use a quoted heredoc: {first}"));
        }
        if block.matches(DELIMITER).count() != 2 {
            out.push(format!(
                "block must hold the delimiter exactly twice: {first}"
            ));
        }
        if !block.contains("$ARGUMENTS") {
            out.push(format!("block never passes $ARGUMENTS: {first}"));
        }
        let command: Vec<&str> = first
            .split(" <<")
            .next()
            .unwrap_or("")
            .split_whitespace()
            .collect();
        let covered = allowed.split(", ").any(|rule| {
            let Some(inner) = rule.strip_prefix("Bash(").and_then(|r| r.strip_suffix(')')) else {
                return rule == "Bash";
            };
            let prefix = inner.trim_end_matches(":*");
            let words: Vec<&str> = prefix.split_whitespace().collect();
            command.len() >= words.len() && command[..words.len()] == words[..]
        });
        if !covered {
            out.push(format!("allowed-tools does not cover: {first}"));
        }
    }
    out
}

#[test]
fn a_block_with_a_quoted_heredoc_and_a_covering_rule_passes() {
    let text = format!(
        "---\nallowed-tools: Bash(playbook commit all:*)\n---\n```!\nplaybook commit all --json <<'{DELIMITER}'\n$ARGUMENTS\n{DELIMITER}\n```\n"
    );
    assert!(problems(&text).is_empty(), "{:?}", problems(&text));
}

#[test]
fn an_unquoted_heredoc_is_flagged() {
    let text = format!(
        "---\nallowed-tools: Bash(playbook:*)\n---\n```!\nplaybook x <<{DELIMITER}\n$ARGUMENTS\n{DELIMITER}\n```\n"
    );
    assert!(problems(&text).iter().any(|p| p.contains("quoted heredoc")));
}

#[test]
fn a_command_outside_allowed_tools_is_flagged() {
    let text = format!(
        "---\nallowed-tools: Bash(git:*)\n---\n```!\nplaybook x <<'{DELIMITER}'\n$ARGUMENTS\n{DELIMITER}\n```\n"
    );
    assert!(problems(&text).iter().any(|p| p.contains("does not cover")));
}

#[test]
fn a_block_that_ignores_arguments_is_flagged() {
    let text = format!(
        "---\nallowed-tools: Bash(playbook:*)\n---\n```!\nplaybook x <<'{DELIMITER}'\nfixed\n{DELIMITER}\n```\n"
    );
    assert!(problems(&text).iter().any(|p| p.contains("$ARGUMENTS")));
}

#[test]
fn every_inline_block_in_the_plugin_is_clean() {
    let mut files = Vec::new();
    markdown_files(&plugin_root(), &mut files);
    let mut all = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file).expect("markdown file");
        for p in problems(&text) {
            all.push(format!("{}: {p}", file.display()));
        }
    }
    assert!(all.is_empty(), "{}", all.join("\n"));
}

#[test]
fn the_delimiter_appears_only_inside_inline_blocks() {
    let mut files = Vec::new();
    markdown_files(&plugin_root(), &mut files);
    let mut stray = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file).expect("markdown file");
        let inside: usize = blocks(&text)
            .iter()
            .map(|b| b.matches(DELIMITER).count())
            .sum();
        if text.matches(DELIMITER).count() != inside {
            stray.push(file.display().to_string());
        }
    }
    assert!(stray.is_empty(), "delimiter outside a block: {stray:?}");
}
