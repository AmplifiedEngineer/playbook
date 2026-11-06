// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Metamorphic relations on the fixtures. Each test changes a copy of a
//! fixture in a way that has a known effect on the index and checks that
//! effect. Every relation first asserts that the baseline index is not empty,
//! so an extractor that returns nothing fails all of them.

mod common;
mod session;

use common::Row;
use session::Session;
use std::collections::BTreeSet;

/// Per language: the identifier to rename, its new name, a file to move and
/// the path it moves to.
const CASES: [(&str, &str, &str, &str, &str); 5] = [
    (
        "rust",
        "double_area",
        "twice_area",
        "src/shape.rs",
        "src/geometry/shape.rs",
    ),
    (
        "typescript",
        "makeSquare",
        "buildSquare",
        "src/shapes.ts",
        "src/lib/shapes.ts",
    ),
    (
        "javascript",
        "increment",
        "bump",
        "src/counter.js",
        "src/lib/counter.js",
    ),
    (
        "python",
        "make_square",
        "build_square",
        "pkg/shapes.py",
        "pkg/lib/shapes.py",
    ),
    ("go", "Total", "Combined", "shape.go", "geo/shape.go"),
];

const INSERTED_COMMENT_LINES: i64 = 2;
const TRAILING_PAD: &str = "   ";
const TAB: &str = "\t";
const TAB_AS_SPACES: &str = "    ";
const LAST_LINE_PADDING: &str = "\n\n";

fn baseline(lang: &str, tag: &str) -> (Session, Vec<Row>) {
    let session = Session::new(lang, tag);
    session.index(false);
    let rows = session.indexed().symbols();
    assert!(!rows.is_empty(), "{lang}: baseline index is empty");
    (session, rows)
}

fn paths(rows: &[Row]) -> BTreeSet<String> {
    rows.iter().map(|r| r.path.clone()).collect()
}

/// The identity of a symbol that survives a reformat: no line numbers.
fn shape(rows: &[Row]) -> BTreeSet<(String, String, String, bool)> {
    rows.iter()
        .map(|r| {
            (
                r.path.clone(),
                r.qualified.clone(),
                r.kind.clone(),
                r.exported,
            )
        })
        .collect()
}

fn sorted(mut rows: Vec<Row>) -> Vec<Row> {
    rows.sort_by(|a, b| {
        (&a.path, a.start_line, &a.qualified).cmp(&(&b.path, b.start_line, &b.qualified))
    });
    rows
}

fn reindexed(session: &Session) -> Vec<Row> {
    session.index(false);
    let rows = session.indexed().symbols();
    assert!(!rows.is_empty(), "index is empty after the change");
    rows
}

#[test]
fn rename_follows_the_symbol() {
    for (lang, old, new, _, _) in CASES {
        // Arrange
        let (session, before) = baseline(lang, "rename");
        assert!(
            before.iter().any(|r| r.name == old),
            "{lang}: {old} missing from the baseline"
        );
        for path in paths(&before) {
            session.write(&path, &session.read(&path).replace(old, new));
        }

        // Act
        let after = reindexed(&session);

        // Assert
        assert!(after.iter().any(|r| r.name == new), "{lang}: {new} absent");
        assert!(
            after.iter().all(|r| r.name != old),
            "{lang}: {old} still present"
        );
        let expected: Vec<Row> = before
            .into_iter()
            .map(|r| Row {
                name: r.name.replace(old, new),
                qualified: r.qualified.replace(old, new),
                ..r
            })
            .collect();
        assert_eq!(
            sorted(after),
            sorted(expected),
            "{lang}: rows changed beyond the name"
        );
    }
}

#[test]
fn move_follows_the_path() {
    for (lang, _, _, from, to) in CASES {
        // Arrange
        let (session, before) = baseline(lang, "move");
        assert!(paths(&before).contains(from), "{lang}: {from} not indexed");
        session.rename(from, to);

        // Act
        let after = reindexed(&session);

        // Assert
        assert!(paths(&after).contains(to), "{lang}: {to} absent");
        assert!(
            !paths(&after).contains(from),
            "{lang}: {from} still present"
        );
        let expected: Vec<Row> = before
            .into_iter()
            .map(|r| match r.path == from {
                true => Row {
                    path: to.to_string(),
                    ..r
                },
                false => r,
            })
            .collect();
        assert_eq!(
            sorted(after),
            sorted(expected),
            "{lang}: symbols changed by the move"
        );
    }
}

#[test]
fn reformat_keeps_the_symbol_set() {
    for (lang, ..) in CASES {
        // Arrange
        let (session, before) = baseline(lang, "reformat");
        for path in paths(&before) {
            let text = session.read(&path);
            let mut padded: String = text
                .lines()
                .map(|line| format!("{}{TRAILING_PAD}\n", line.replace(TAB, TAB_AS_SPACES)))
                .collect();
            padded.push_str(LAST_LINE_PADDING);
            assert_ne!(padded, text, "{lang}: {path} was not reformatted");
            session.write(&path, &padded);
        }

        // Act
        let after = reindexed(&session);

        // Assert
        assert_eq!(shape(&after), shape(&before), "{lang}: symbol set changed");
    }
}

#[test]
fn added_comments_only_shift_lines() {
    for (lang, ..) in CASES {
        // Arrange
        let (session, before) = baseline(lang, "comment");
        let marker = if lang == "python" { "#" } else { "//" };
        let header: String = (1..=INSERTED_COMMENT_LINES)
            .map(|n| format!("{marker} added comment {n}\n"))
            .collect();
        for path in paths(&before) {
            session.write(&path, &format!("{header}{}", session.read(&path)));
        }

        // Act
        let after = reindexed(&session);

        // Assert
        assert_eq!(shape(&after), shape(&before), "{lang}: symbol set changed");
        let shifted: Vec<Row> = before
            .into_iter()
            .map(|r| Row {
                start_line: r.start_line + INSERTED_COMMENT_LINES,
                end_line: r.end_line + INSERTED_COMMENT_LINES,
                ..r
            })
            .collect();
        assert_eq!(
            sorted(after),
            sorted(shifted),
            "{lang}: lines shifted wrongly"
        );
    }
}
