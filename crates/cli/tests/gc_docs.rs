// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The gc docs name every store. The changelog is built from commit subjects at
//! release (ADR 0037), so it has no hand written entry to check.

use std::fs;
use std::path::PathBuf;

fn repo_file(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn internals_doc_names_every_store() {
    // Arrange
    let home = std::env::temp_dir().join("playbook-gc-changelog-home");
    let registry = playbook::gc::registry(&home);
    let doc = repo_file("docs/internals/16-gc-and-tombstones.md").to_lowercase();

    // Act
    let missing: Vec<&str> = registry
        .stores()
        .iter()
        .map(|store| store.name)
        .filter(|name| !doc.contains(&name.to_lowercase()))
        .collect();

    // Assert
    assert!(missing.is_empty(), "doc lacks stores: {missing:?}");
}
