// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Every worktree removal by a sweep goes through the gc guard: the only code
//! that passes its own remover to the sweep is the gc worktree store.

use std::fs;
use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// The files under `src` that mention `needle`, as paths relative to `src`.
fn mentions(needle: &str) -> Vec<String> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let mut found: Vec<String> = files
        .iter()
        .filter(|path| fs::read_to_string(path).is_ok_and(|text| text.contains(needle)))
        .filter_map(|path| path.strip_prefix(&src).ok())
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .collect();
    found.sort();
    found
}

#[test]
fn only_the_gc_store_passes_a_remover_to_the_sweep() {
    assert_eq!(
        mentions("sweep_removing("),
        ["gc/worktrees.rs", "worktree/mod.rs"]
    );
}

#[test]
fn the_unguarded_sweep_entry_point_is_gone() {
    assert!(mentions("worktree::sweep(").is_empty());
}
