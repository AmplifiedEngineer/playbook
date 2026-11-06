// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Shared helpers for the fixture tests: each test copies a fixture repo into
//! a scratch git repo and indexes it through the crate's public API.

use playbook_code::{db, index};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const WORKTREE: &str = "fixture-worktree";
const REPO_DIR: &str = "repo";

/// One symbol row as the index stores it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub path: String,
    pub qualified: String,
    pub name: String,
    pub kind: String,
    pub start_line: i64,
    pub end_line: i64,
    pub exported: bool,
}

/// A fixture copied to a scratch repo and indexed.
pub(crate) struct Indexed {
    pub root: PathBuf,
    pub conn: db::Store,
}

pub(crate) fn fixture_dir(lang: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(lang)
}

/// Copies the fixture's repo into a fresh git repo and indexes it.
pub(crate) fn index_fixture(lang: &str) -> Indexed {
    let scratch = playbook_common::test_support::scratch_dir(&format!("fixture-{lang}"));
    let root = scratch.join(REPO_DIR);
    copy_tree(&fixture_dir(lang).join(REPO_DIR), &root);
    git(&root, &["init", "--quiet"]);
    git(&root, &["add", "."]);
    let store = scratch.join("store").join("index.sqlite3");
    index::run(&root, &store, WORKTREE, false, &index::Options::default()).expect("index run");
    let conn = db::open_query(&store, WORKTREE).expect("query open");
    Indexed { root, conn }
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create dir");
    for entry in fs::read_dir(from)
        .expect("read dir")
        .map(|e| e.expect("entry"))
    {
        let target = to.join(entry.file_name());
        match entry.file_type().expect("file type").is_dir() {
            true => copy_tree(&entry.path(), &target),
            false => drop(fs::copy(entry.path(), target).expect("copy")),
        }
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?}");
}

impl Indexed {
    /// Every indexed symbol, ordered by path, line and qualified name.
    pub(crate) fn symbols(&self) -> Vec<Row> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT f.path, s.qualified_name, s.name, s.kind, s.start_line, s.end_line,
                        s.exported
                 FROM symbols s JOIN files f ON f.id = s.file_id
                 ORDER BY f.path, s.start_line, s.qualified_name",
            )
            .expect("prepare");
        stmt.query_map([], |r| {
            Ok(Row {
                path: r.get(0)?,
                qualified: r.get(1)?,
                name: r.get(2)?,
                kind: r.get(3)?,
                start_line: r.get(4)?,
                end_line: r.get(5)?,
                exported: r.get(6)?,
            })
        })
        .expect("query")
        .map(|r| r.expect("row"))
        .collect()
    }
}
