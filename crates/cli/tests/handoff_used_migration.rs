// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `handoff/used/` folder is gone: loaded handoffs are gc tombstones. Only
//! the migration, its tests and the changelog and gc docs may still name it.

use std::fs;
use std::path::{Path, PathBuf};

const ALLOWED: [&str; 6] = [
    "crates/cli/src/init/migrate.rs",
    "crates/cli/src/init/handoff_used.rs",
    "crates/cli/tests/handoff_used_migration.rs",
    "CHANGELOG.md",
    "docs/internals/17-gc-and-tombstones.md",
    "docs/internals/03-worktree.md",
];
const SCANNED_DIRS: [&str; 5] = ["crates", "docs", "plugins", "seed", "scripts"];
const EXTENSIONS: [&str; 6] = ["rs", "md", "json", "toml", "sh", "yml"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repo root should exist")
}

fn source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "target" && name != "node_modules" && !name.starts_with('.') {
                source_files(&path, out);
            }
        } else if path
            .extension()
            .is_some_and(|ext| EXTENSIONS.iter().any(|known| ext == *known))
        {
            out.push(path);
        }
    }
}

/// Whether `text` holds `used/` at a word boundary, or the words "used directory".
fn names_the_old_folder(text: &str) -> bool {
    if text.contains("used directory") {
        return true;
    }
    text.match_indices("used/").any(|(at, _)| {
        text[..at]
            .chars()
            .next_back()
            .is_none_or(|before| !(before.is_alphanumeric() || before == '_'))
    })
}

#[test]
fn no_used_dir_reference_remains() {
    // Arrange
    let root = repo_root();
    let mut files = Vec::new();
    for dir in SCANNED_DIRS {
        source_files(&root.join(dir), &mut files);
    }

    // Act
    let offenders: Vec<String> = files
        .iter()
        .filter_map(|path| path.strip_prefix(&root).ok())
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .filter(|rel| !ALLOWED.contains(&rel.as_str()))
        .filter(|rel| {
            fs::read_to_string(root.join(rel)).is_ok_and(|text| names_the_old_folder(&text))
        })
        .collect();

    // Assert
    assert!(
        offenders.is_empty(),
        "these still name used/: {offenders:?}"
    );
}
