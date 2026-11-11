// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Shipped text must name the real home of the memory graph,
//! `~/.config/playbook/data/memory.db`. A command that sends the model to
//! `memory.graph.json` or to `~/.config/playbook/memory.db` makes it look in
//! the wrong place and reach for `sqlite3`. ADRs are history and are not
//! scanned. The few docs that describe the old file as history are listed
//! by name below, and even there a line naming it must also name the
//! current home or the `playbook memory` command.

#[path = "support/shipped.rs"]
mod shipped;

use std::fs;
use std::path::{Path, PathBuf};

/// What the plugin, setup and the docs ship. Each must exist.
const SCANNED: [&str; 5] = ["plugins", "seed", "docs", "README.md", "CONTRIBUTING.md"];

/// Not scanned: decision records keep the paths of their day.
const SKIPPED: &str = "docs/adr";

/// Docs that name `memory.graph.json` only as the file older versions kept.
const OLD_FILE_HISTORY: [&str; 4] = [
    "docs/internals/13-memory-system.md",
    "docs/guides/05-what-setup-installs.md",
    "docs/internals/11-data-folder.md",
    "docs/reference/cli-and-config.md",
];

fn root() -> PathBuf {
    PathBuf::from(env!("PLAYBOOK_REPO_ROOT"))
}

/// Every scanned file, as its path relative to the repo root.
fn scanned_files() -> Vec<PathBuf> {
    let root = root();
    let mut files = Vec::new();
    for rel in SCANNED {
        shipped::files_under(&root.join(rel), &["md", "json", "sh", "txt"], &mut files);
    }
    files
        .into_iter()
        .filter_map(|f| f.strip_prefix(&root).ok().map(Path::to_path_buf))
        .filter(|rel| !rel.starts_with(SKIPPED))
        .collect()
}

/// Whether `line` of the shipped file `rel` names a stale memory graph path.
fn is_stale(rel: &Path, line: &str) -> bool {
    if line.contains("config/playbook/memory.db") {
        return true;
    }
    if !line.contains("memory.graph.json") {
        return false;
    }
    let history = OLD_FILE_HISTORY.iter().any(|h| rel == Path::new(h))
        && (line.contains("memory.db") || line.contains("playbook memory"));
    !history
}

#[test]
fn shipped_text_names_the_real_memory_graph_path() {
    let mut bad = Vec::new();
    for rel in scanned_files() {
        let text = fs::read_to_string(root().join(&rel)).unwrap_or_default();
        for (n, line) in text.lines().enumerate() {
            if is_stale(&rel, line) {
                bad.push(format!("{}:{}: {}", rel.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "the memory graph lives in ~/.config/playbook/data/memory.db; inspect it with \
         `playbook memory stats`. Fix these lines (or add a history-only doc to \
         OLD_FILE_HISTORY):\n{}",
        bad.join("\n")
    );
}

#[test]
fn the_scan_reaches_every_shipped_root_and_skips_the_adrs() {
    for rel in SCANNED {
        assert!(root().join(rel).exists(), "scanned path {rel} is missing");
    }
    for rel in OLD_FILE_HISTORY {
        assert!(root().join(rel).is_file(), "history doc {rel} is missing");
    }
    let files = scanned_files();
    assert!(files.contains(&PathBuf::from("plugins/playbook/commands/learn.md")));
    assert!(files.iter().any(|f| f.ends_with("SYSTEM_PROMPT.md")));
    assert!(!files.iter().any(|f| f.starts_with(SKIPPED)));
}

#[test]
fn the_line_check_flags_stale_paths_and_keeps_history() {
    let cmd = Path::new("plugins/playbook/commands/learn.md");
    let history = Path::new("docs/internals/11-data-folder.md");

    assert!(is_stale(cmd, "read ~/.config/playbook/memory.db"));
    assert!(is_stale(cmd, "the graph is memory.graph.json"));
    assert!(is_stale(history, "the graph is memory.graph.json"));
    assert!(is_stale(history, "see ~/.config/playbook/memory.db"));
    assert!(!is_stale(
        history,
        "older memory.graph.json moved into data/memory.db"
    ));
    assert!(!is_stale(
        cmd,
        "the graph is ~/.config/playbook/data/memory.db"
    ));
}
