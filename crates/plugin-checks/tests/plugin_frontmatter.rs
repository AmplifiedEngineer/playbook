// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Pins one frontmatter fault that shipped in 0.9.0 and 0.9.1: an unquoted
//! value holding a colon-space. YAML reads that as a nested mapping and
//! rejects the document, dropping every field, including the `tools`
//! allowlist that keeps Edit and Bash away from the read-only agents.
//!
//! The check lives here rather than delegating to `claude plugin validate
//! --strict` because that validator is version-dependent for this class:
//! measured 2026-08-18 on the same broken file, it exits 1 on CLI 2.1.220 and
//! 0 on 2.1.234, while python's yaml rejects it under both.

use std::fs;
use std::path::{Path, PathBuf};

const ROOTS: [&str; 4] = ["agents", "commands", "skills", "output-styles"];

/// Both the key separator and, when it recurs inside an unquoted value, the
/// fault itself.
const KEY_SEP: &str = ": ";

/// Floor for the directory walk, so a broken traversal fails loudly instead of
/// passing vacuously. There were 35 such files when this was written.
const MIN_SCANNED_FILES: usize = 20;

const VALUE_PREVIEW_CHARS: usize = 80;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("PLAYBOOK_PLUGIN_ROOT"))
}

fn collect_markdown(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_markdown(&path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}

/// `None` for a file with no frontmatter, which is legitimate for a plain
/// README rather than an error.
fn frontmatter(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    Some(&rest[..end])
}

/// Quoting makes a colon-space inert, so quoted values are never at fault.
fn is_quoted(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(
        (chars.next(), chars.next_back()),
        (Some('"'), Some('"')) | (Some('\''), Some('\''))
    )
}

fn colon_space_offenders(block: &str) -> Vec<(&str, &str)> {
    block
        .lines()
        // Indented lines belong to nested structures this check does not model,
        // so flagging them would be a false positive.
        .filter(|line| !line.starts_with(char::is_whitespace))
        .filter_map(|line| line.split_once(KEY_SEP))
        .filter(|(key, _)| {
            !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
        })
        .map(|(key, value)| (key, value.trim()))
        .filter(|(_, value)| !value.is_empty() && !is_quoted(value) && value.contains(KEY_SEP))
        .collect()
}

#[test]
fn plugin_markdown_frontmatter_has_no_unquoted_colon_space() {
    let root = repo_root();
    let mut files = Vec::new();
    for r in ROOTS {
        collect_markdown(&root.join(r), &mut files);
    }
    files.sort();

    assert!(
        files.len() >= MIN_SCANNED_FILES,
        "scanned {} files, expected at least {MIN_SCANNED_FILES}; the walk is broken",
        files.len()
    );

    let mut with_frontmatter = 0usize;
    let mut failures = Vec::new();
    for path in &files {
        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        let Some(block) = frontmatter(&text) else {
            continue;
        };
        with_frontmatter += 1;
        for (key, value) in colon_space_offenders(block) {
            let preview: String = value.chars().take(VALUE_PREVIEW_CHARS).collect();
            failures.push(format!(
                "{}: '{key}' is unquoted and contains a colon-space, so YAML \
                 rejects the document and drops every field. Quote it. Value: {preview}",
                path.strip_prefix(&root).unwrap_or(path).display()
            ));
        }
    }

    assert!(
        with_frontmatter >= MIN_SCANNED_FILES,
        "only {with_frontmatter} files carried frontmatter, expected at least {MIN_SCANNED_FILES}"
    );
    assert!(
        failures.is_empty(),
        "{} file(s) carry unparseable frontmatter:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Without this the cheapest way to satisfy the scan is to ban colons outright,
/// which would fire on every `/playbook:review` reference.
#[test]
fn bare_and_quoted_colons_are_accepted() {
    let block = "name: reviewer\n\
                 description: Runs the /playbook:review swarm.\n\
                 quoted: \"takes a focus from the prompt: a single named lens\"\n\
                 single: 'also fine: quoted'\n";
    assert!(colon_space_offenders(block).is_empty());

    let bad = "description: takes a focus from the prompt: a single named lens\n";
    assert_eq!(
        colon_space_offenders(bad),
        vec![(
            "description",
            "takes a focus from the prompt: a single named lens"
        )]
    );
}

#[test]
fn indented_lines_are_not_top_level_keys() {
    let block = "links:\n  relates_to: some fact: with a colon\nname: x\n";
    assert!(colon_space_offenders(block).is_empty());
}
