// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A scratch copy of one fixture repo that a test edits and indexes again.
//! The copy and its store live in the test's own temp directory.

// Each test crate compiles this module and uses only part of it.
#![allow(dead_code)]

use crate::common::{self, Indexed};
use playbook_code::{db, index};
use std::fs;
use std::path::PathBuf;

const WORKTREE: &str = "session-worktree";
const REPO_DIR: &str = "repo";

pub(crate) struct Session {
    pub root: PathBuf,
    store: PathBuf,
}

impl Session {
    /// Copies the fixture of `lang` into a fresh git repo with an empty store.
    pub(crate) fn new(lang: &str, tag: &str) -> Self {
        let scratch = playbook_common::test_support::scratch_dir(&format!("session-{tag}-{lang}"));
        let root = scratch.join(REPO_DIR);
        common::copy_tree(&common::fixture_dir(lang).join(REPO_DIR), &root);
        common::git(&root, &["init", "--quiet"]);
        let store = scratch.join("store").join("index.sqlite3");
        Self { root, store }
    }

    /// Runs the indexer on the current files, incrementally unless `rebuild`.
    pub(crate) fn index(&self, rebuild: bool) {
        index::run(
            &self.root,
            &self.store,
            WORKTREE,
            rebuild,
            &index::Options::default(),
        )
        .expect("index run");
    }

    /// The stored rows, read back through a query connection.
    pub(crate) fn indexed(&self) -> Indexed {
        let conn = db::open_query(&self.store, WORKTREE).expect("query open");
        Indexed {
            root: self.root.clone(),
            conn,
        }
    }

    pub(crate) fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.root.join(rel)).expect("read file")
    }

    pub(crate) fn write(&self, rel: &str, text: &str) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        fs::write(path, text).expect("write file");
    }

    pub(crate) fn remove(&self, rel: &str) {
        fs::remove_file(self.root.join(rel)).expect("remove file");
    }

    pub(crate) fn rename(&self, from: &str, to: &str) {
        let target = self.root.join(to);
        fs::create_dir_all(target.parent().expect("parent")).expect("dirs");
        fs::rename(self.root.join(from), target).expect("rename file");
    }
}
