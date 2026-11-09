// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The ADR numbers in `docs/adr` run 1, 2, 3 and so on with no gap, and
//! every link to an ADR file in shipped text names a file that exists. A
//! number may repeat only for the blueprint, quality and test mapping
//! companions of one ADR, which share its number and start with its slug.

#[path = "support/shipped.rs"]
mod shipped;

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("PLAYBOOK_REPO_ROOT"))
}

/// Whether a Markdown file name in `docs/adr` is a decision record or one of
/// its companions. The index, `README.md`, is neither.
fn is_adr_file(name: &str) -> bool {
    name.ends_with(".md") && name != "README.md"
}

/// The ADR file names under `docs/adr`, without the index.
fn adr_files() -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(root().join("docs/adr"))
        .expect("docs/adr exists")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| is_adr_file(n))
        .collect();
    names.sort();
    names
}

#[test]
fn the_adr_index_is_not_an_adr_file() {
    assert!(!is_adr_file("README.md"));
    assert!(is_adr_file("0001-package-toolkit-as-plugin.md"));
    assert!(is_adr_file("0003-purpose-built-subagents-quality.md"));
    assert!(!is_adr_file("notes.txt"));
}

#[test]
fn every_decision_record_has_a_row_in_the_adr_index() {
    let index = fs::read_to_string(root().join("docs/adr/README.md")).expect("docs/adr/README.md");
    let missing: Vec<String> = adr_files()
        .into_iter()
        .filter(|n| !is_companion(n))
        .filter(|n| !index.contains(&format!("]({n})")))
        .collect();
    assert!(
        missing.is_empty(),
        "docs/adr/README.md has no row for: {missing:?}"
    );
}

/// Whether `name` starts with four digits and a dash.
fn has_number(name: &str) -> bool {
    name.len() > 5 && name.as_bytes()[4] == b'-' && name[..4].bytes().all(|b| b.is_ascii_digit())
}

/// The four digit number a file name starts with.
fn number(name: &str) -> u32 {
    assert!(has_number(name), "{name} does not start with NNNN-");
    name[..4].parse().expect("digits")
}

/// Whether `name` is a blueprint, quality or test mapping companion.
fn is_companion(name: &str) -> bool {
    let stem = name.trim_end_matches(".md");
    stem.ends_with("-blueprint") || stem.ends_with("-quality") || stem == "0007-test-mapping"
}

#[test]
fn adr_numbers_are_contiguous_from_one() {
    let numbers: BTreeSet<u32> = adr_files().iter().map(|n| number(n)).collect();
    let last = *numbers.last().expect("at least one ADR");
    let missing: Vec<u32> = (1..=last).filter(|n| !numbers.contains(n)).collect();
    assert!(
        missing.is_empty(),
        "ADR numbers missing from 1..={last}: {missing:?}"
    );
}

#[test]
fn a_number_is_shared_only_with_companions_of_one_adr() {
    let files = adr_files();
    let mut seen = BTreeSet::new();
    for name in files.iter().filter(|n| !is_companion(n)) {
        assert!(
            seen.insert(number(name)),
            "two ADRs share the number in {name}"
        );
    }
    for name in files.iter().filter(|n| is_companion(n)) {
        let stem = name.trim_end_matches(".md");
        let main = stem
            .strip_suffix("-blueprint")
            .or_else(|| stem.strip_suffix("-quality"))
            .unwrap_or(stem);
        let has_adr = files.iter().any(|f| f.trim_end_matches(".md") == main);
        // The test mapping of ADR 0007 is named for the number only.
        let mapping = stem == "0007-test-mapping" && seen.contains(&number(name));
        assert!(
            has_adr || mapping,
            "{name} is a companion of an ADR that does not exist"
        );
    }
}

#[test]
fn each_adr_title_carries_its_file_number() {
    let root = root();
    for name in adr_files().iter().filter(|n| !is_companion(n)) {
        let text = fs::read_to_string(root.join("docs/adr").join(name)).expect("readable");
        let title = text.lines().next().unwrap_or_default();
        let n = number(name);
        let ok = title.starts_with(&format!("# ADR-{n:04}:"))
            || title.starts_with(&format!("# ADR {n:04}:"));
        assert!(ok, "{name} title is {title:?}, want the number {n:04}");
    }
}

#[test]
fn every_plain_adr_reference_names_an_existing_number() {
    let root = root();
    let numbers: BTreeSet<u32> = adr_files().iter().map(|n| number(n)).collect();
    let mut files = Vec::new();
    for rel in [
        "docs",
        "crates",
        "scripts",
        ".github",
        "README.md",
        "CONTRIBUTING.md",
    ] {
        shipped::files_under(
            &root.join(rel),
            &["md", "rs", "sh", "yml", "toml"],
            &mut files,
        );
    }
    let mut checked = 0;
    let mut stale = Vec::new();
    for path in files {
        let text = fs::read_to_string(&path).unwrap_or_default();
        for (at, _) in text.match_indices("ADR") {
            let rest = &text[at + 3..];
            let rest = rest.strip_prefix([' ', '-']).unwrap_or(rest);
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if digits.len() != 4 {
                continue;
            }
            checked += 1;
            if !numbers.contains(&digits.parse().expect("digits")) {
                let rel = path.strip_prefix(&root).unwrap_or(&path);
                stale.push(format!("{}: ADR {digits}", rel.display()));
            }
        }
    }
    assert!(
        checked > 50,
        "only {checked} ADR references were found; the scan is broken"
    );
    assert!(
        stale.is_empty(),
        "ADR references to a number with no file: {stale:#?}"
    );
}

#[test]
fn every_adr_link_resolves_to_a_file() {
    let root = root();
    let files = adr_files();
    let mut docs = Vec::new();
    for rel in ["docs", "crates", "README.md", "CONTRIBUTING.md"] {
        shipped::files_under(&root.join(rel), &["md"], &mut docs);
    }
    let mut broken = Vec::new();
    let mut checked = 0;
    for path in docs {
        let in_adr = path.starts_with(root.join("docs/adr"));
        let text = fs::read_to_string(&path).unwrap_or_default();
        for (at, _) in text.match_indices(".md") {
            let head = &text[..at];
            let start = head
                .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                .map_or(0, |i| i + 1);
            let name = format!("{}.md", &head[start..]);
            let before = &head[..start];
            let linked = before.ends_with("adr/") || (in_adr && before.ends_with("]("));
            if linked && has_number(&name) {
                checked += 1;
            }
            if linked && has_number(&name) && !files.contains(&name) {
                let rel = path.strip_prefix(&root).unwrap_or(&path);
                broken.push(format!("{}: {name}", rel.display()));
            }
        }
    }
    assert!(
        checked > 20,
        "only {checked} ADR links were found; the scan is broken"
    );
    assert!(broken.is_empty(), "ADR links with no file: {broken:#?}");
}
