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
    let found = hits(&["worktree::sweep"], &["worktrees.rs"]);

    assert!(found.is_empty(), "old sweep call: {found:#?}");
}

#[test]
fn gc_lock_only_via_with_run_lock() {
    let banned = ["with_dir_lock", "acquire_dir_lock", "remove_stale_lock_dir"];

    let found = hits(&banned, &["lock.rs"]);

    assert!(found.is_empty(), "gc lock taken directly: {found:#?}");
}
