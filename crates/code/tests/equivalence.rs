// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! An index kept up to date through edits equals an index built fresh from
//! the same files, and two runs over the same files read back equal rows.

mod common;
mod session;

use common::Row;
use session::Session;

/// Per language: a file to edit, the text appended to it, a file to rename,
/// its new path, and the path of an extra file that is deleted.
struct Case {
    lang: &'static str,
    edit: &'static str,
    appended: &'static str,
    rename_from: &'static str,
    rename_to: &'static str,
    extra: &'static str,
    extra_text: &'static str,
}

const CASES: [Case; 5] = [
    Case {
        lang: "rust",
        edit: "src/lib.rs",
        appended: "\npub fn added_symbol() -> i32 {\n    1\n}\n",
        rename_from: "src/shape.rs",
        rename_to: "src/shape_renamed.rs",
        extra: "src/extra.rs",
        extra_text: "pub fn extra_symbol() {}\n",
    },
    Case {
        lang: "typescript",
        edit: "src/main.ts",
        appended: "\nexport function addedSymbol(): number {\n  return 1;\n}\n",
        rename_from: "src/shapes.ts",
        rename_to: "src/shapes_renamed.ts",
        extra: "src/extra.ts",
        extra_text: "export function extraSymbol() {}\n",
    },
    Case {
        lang: "javascript",
        edit: "src/app.js",
        appended: "\nexport function addedSymbol() {\n  return 1;\n}\n",
        rename_from: "src/counter.js",
        rename_to: "src/counter_renamed.js",
        extra: "src/extra.js",
        extra_text: "export function extraSymbol() {}\n",
    },
    Case {
        lang: "python",
        edit: "pkg/main.py",
        appended: "\n\ndef added_symbol():\n    return 1\n",
        rename_from: "pkg/shapes.py",
        rename_to: "pkg/shapes_renamed.py",
        extra: "pkg/extra.py",
        extra_text: "def extra_symbol():\n    return 0\n",
    },
    Case {
        lang: "go",
        edit: "main.go",
        appended: "\nfunc AddedSymbol() int {\n\treturn 1\n}\n",
        rename_from: "shape.go",
        rename_to: "shape_renamed.go",
        extra: "extra.go",
        extra_text: "package shape\n\nfunc ExtraSymbol() int { return 0 }\n",
    },
];

/// Everything the index stores about a repo that does not depend on the
/// scratch location, the clock or row ids.
#[derive(Debug, PartialEq, Eq)]
struct Rows {
    symbols: Vec<Row>,
    manifest: Vec<(String, String, String, Option<String>)>,
    imports: Vec<(String, String, Option<String>, i64)>,
}

fn read_back(session: &Session) -> Rows {
    let indexed = session.indexed();
    let manifest = indexed
        .conn
        .prepare("SELECT path, sha256, status, lang FROM files ORDER BY path")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    let imports = indexed
        .conn
        .prepare(
            "SELECT f.path, i.module, i.names, i.line
             FROM imports i JOIN files f ON f.id = i.file_id
             ORDER BY f.path, i.line, i.module",
        )
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();
    let rows = Rows {
        symbols: indexed.symbols(),
        manifest,
        imports,
    };
    assert!(!rows.symbols.is_empty(), "symbols are empty");
    assert!(!rows.manifest.is_empty(), "manifest is empty");
    rows
}

fn edit_and_rename(session: &Session, case: &Case) {
    session.write(
        case.edit,
        &format!("{}{}", session.read(case.edit), case.appended),
    );
    session.rename(case.rename_from, case.rename_to);
}

#[test]
fn incremental_index_equals_a_fresh_index() {
    for case in &CASES {
        // Arrange
        let incremental = Session::new(case.lang, "incremental");
        incremental.write(case.extra, case.extra_text);
        incremental.index(false);
        let before = read_back(&incremental);
        assert!(
            before.manifest.iter().any(|m| m.0 == case.extra),
            "{}: extra file not indexed",
            case.lang
        );

        // Act
        edit_and_rename(&incremental, case);
        incremental.remove(case.extra);
        incremental.index(false);
        let fresh = Session::new(case.lang, "fresh");
        edit_and_rename(&fresh, case);
        fresh.index(false);

        // Assert
        let after = read_back(&incremental);
        assert_ne!(after, before, "{}: the edits changed nothing", case.lang);
        assert_eq!(
            after,
            read_back(&fresh),
            "{}: incremental differs from fresh",
            case.lang
        );
    }
}

#[test]
fn two_runs_read_back_equal_rows() {
    for case in &CASES {
        // Arrange
        let first = Session::new(case.lang, "first");
        let second = Session::new(case.lang, "second");

        // Act
        first.index(false);
        second.index(false);
        let one = read_back(&first);
        first.index(false);
        let again = read_back(&first);
        first.index(true);
        let rebuilt = read_back(&first);

        // Assert
        assert_eq!(one, read_back(&second), "{}: two copies differ", case.lang);
        assert_eq!(one, again, "{}: an unchanged rerun differs", case.lang);
        assert_eq!(one, rebuilt, "{}: a rebuild differs", case.lang);
    }
}
