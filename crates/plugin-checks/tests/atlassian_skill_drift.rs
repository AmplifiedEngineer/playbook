// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Drift guard: any skill, command or agent file that mentions Jira,
//! Confluence, Atlassian, acli or twg must point at `playbook:atlassian`, so
//! the access order and the tool quirks live in one place.

#[path = "support/markdown_refs.rs"]
mod markdown_refs;

use std::fs;
use std::path::{Path, PathBuf};

/// Files that only name these products as plain nouns (a data source, a
/// destination for prose) and never drive an Atlassian tool.
const ALLOWLIST: &[&str] = &[
    "agents/analyst.md",
    "skills/playbook-usage/SKILL.md",
    "skills/writing-style/SKILL.md",
];

const TERMS: &[&str] = &["jira", "confluence", "atlassian", "acli", "twg"];

fn root() -> PathBuf {
    PathBuf::from(env!("PLAYBOOK_PLUGIN_ROOT"))
}

fn files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in ["commands", "agents"] {
        for e in fs::read_dir(root().join(dir)).expect("dir").flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "md") {
                out.push(p);
            }
        }
    }
    for e in fs::read_dir(root().join("skills"))
        .expect("skills")
        .flatten()
    {
        let p = e.path();
        if p.file_name().is_some_and(|n| n == "atlassian") {
            continue;
        }
        let skill = p.join("SKILL.md");
        if skill.is_file() {
            out.push(skill);
        }
    }
    out.sort();
    out
}

fn rel(p: &Path) -> String {
    p.strip_prefix(root())
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

fn mentions_term(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|w| TERMS.contains(&w))
}

#[test]
fn atlassian_mentions_point_at_the_atlassian_skill() {
    let mut missing = Vec::new();
    for p in files() {
        let r = rel(&p);
        if ALLOWLIST.contains(&r.as_str()) {
            continue;
        }
        let text = markdown_refs::command_text_with_refs(&r);
        if mentions_term(&text) && !text.contains("playbook:atlassian") {
            missing.push(r);
        }
    }
    assert!(
        missing.is_empty(),
        "mention Jira, Confluence, Atlassian, acli or twg without `playbook:atlassian`:\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn allowlisted_files_exist() {
    for r in ALLOWLIST {
        assert!(root().join(r).is_file(), "stale allowlist entry {r}");
    }
}

#[test]
fn old_skill_name_is_gone() {
    for p in files() {
        let text = fs::read_to_string(&p).expect("read");
        assert!(
            !text.contains("atlassian-cli"),
            "{} uses the old name",
            rel(&p)
        );
    }
}
