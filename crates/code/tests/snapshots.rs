// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Snapshots of what the index extracts from each fixture. A changed extractor
//! kind or range changes a snapshot. Paths are repo relative, and each test
//! checks that no scratch path leaks into the text.

mod common;

use common::index_fixture;
use std::fmt::Write;

/// The languages that have a fixture repo, in snapshot order.
const LANGS: [&str; 5] = ["rust", "typescript", "javascript", "python", "go"];

fn render(lang: &str) -> String {
    let indexed = index_fixture(lang);
    let mut out = String::new();
    for row in indexed.symbols() {
        let export = if row.exported { " exported" } else { "" };
        writeln!(
            out,
            "{} {} {} {}-{}{export}",
            row.path, row.kind, row.qualified, row.start_line, row.end_line
        )
        .expect("write to string");
    }
    let scratch = indexed.root.to_string_lossy();
    assert!(!out.contains(scratch.as_ref()), "scratch path in snapshot");
    out
}

#[test]
fn rust_symbols_match_the_snapshot() {
    insta::assert_snapshot!(render(LANGS[0]));
}

#[test]
fn typescript_symbols_match_the_snapshot() {
    insta::assert_snapshot!(render(LANGS[1]));
}

#[test]
fn javascript_symbols_match_the_snapshot() {
    insta::assert_snapshot!(render(LANGS[2]));
}

#[test]
fn python_symbols_match_the_snapshot() {
    insta::assert_snapshot!(render(LANGS[3]));
}

#[test]
fn go_symbols_match_the_snapshot() {
    insta::assert_snapshot!(render(LANGS[4]));
}
