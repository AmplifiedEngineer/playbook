// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Source scans that keep the gc safety guarantee true: every removal goes
//! through `gc/guard.rs`, and the run lock is only taken by `gc/lock.rs`.

use std::fs;
use std::path::{Path, PathBuf};

fn gc_sources() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/gc");
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, fs::read_to_string(path).unwrap())
        })
        .collect()
}

/// Source lines that are not comments, with their line numbers.
fn code_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line))
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
}

fn hits(banned: &[&str], exempt: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    for (name, text) in gc_sources() {
        if exempt.contains(&name.as_str()) {
            continue;
        }
        for (number, line) in code_lines(&text) {
            for pattern in banned {
                if line.contains(pattern) {
                    found.push(format!("gc/{name}:{number}: {pattern}"));
                }
            }
        }
    }
    found
}

#[test]
fn gc_has_no_unguarded_removal() {
    let banned = [
        "fs::remove",
        "remove_file",
        "remove_dir",
        "use std::fs as",
        "worktree::remove",
        "\"worktree\", \"remove\"",
    ];

    let found = hits(&banned, &["guard.rs"]);

    assert!(found.is_empty(), "unguarded removal: {found:#?}");
}

#[test]
fn gc_does_not_call_the_old_worktree_sweep_outside_its_adapter() {
    // Every spelling that reaches the old sweep: a path call, a grouped or
    // glob import, a module import, or the injectable variant.
    let banned = [
        "worktree::sweep",
        "worktree::{",
        "worktree::*",
        "use crate::worktree;",
        "crate::worktree::{",
        "sweep_with",
    ];

    let found = hits(&banned, &["worktrees.rs"]);

    assert!(found.is_empty(), "old sweep call: {found:#?}");
}

/// Every `.rs` file under `dir`, with the text of its non-comment lines
/// joined and stripped of whitespace, so a call split across lines still
/// matches.
fn squashed_sources(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            squashed_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let text = fs::read_to_string(&path).unwrap();
            let squashed: String = code_lines(&text)
                .flat_map(|(_, line)| line.chars().filter(|c| !c.is_whitespace()))
                .collect();
            out.push((path, squashed));
        }
    }
}

#[test]
fn only_the_adapter_and_the_dry_run_doctor_reach_the_old_sweep() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    squashed_sources(&src, &mut files);
    let allowed = ["gc/worktrees.rs", "doctor/check.rs", "worktree/"];

    let found: Vec<String> = files
        .iter()
        .filter(|(path, _)| {
            let rel = path
                .strip_prefix(&src)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            !allowed.iter().any(|ok| rel.starts_with(ok))
        })
        .filter(|(_, text)| {
            // Any path prefix counts (`crate::`, `playbook::`, a bare `worktree::`);
            // only the report-only dry run may be named.
            let rest = text
                .replace("worktree::sweep_dry_run", "")
                .replace("cc::worktree::", "");
            rest.contains("worktree::sweep")
                || rest.contains("worktree::{")
                || rest.contains("worktree::*")
        })
        .map(|(path, _)| path.display().to_string())
        .collect();

    assert!(found.is_empty(), "unlocked worktree sweep: {found:#?}");
}

#[test]
fn the_locked_sweep_refreshes_the_lock_before_each_removal() {
    let (_, text) = gc_sources()
        .into_iter()
        .find(|(name, _)| name == "worktrees.rs")
        .unwrap();
    let body: String = code_lines(&text)
        .map(|(_, line)| line)
        .take_while(|line| !line.contains("mod tests"))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        body.matches("guard.touch()").count() >= 2,
        "the sweep must touch the lock at the start and inside the remover"
    );
}

#[test]
fn gc_lock_only_via_with_run_lock() {
    let banned = ["with_dir_lock", "acquire_dir_lock", "remove_stale_lock_dir"];

    let found = hits(&banned, &["lock.rs"]);

    assert!(found.is_empty(), "gc lock taken directly: {found:#?}");
}
