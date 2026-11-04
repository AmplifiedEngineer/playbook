// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Shared helpers for unit tests that need a scratch repo and an index.

use super::db;
use super::index::{self, Options, Report};
use super::walker::test_repo::{git_in, init, write};
use std::fs::File;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, UNIX_EPOCH};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Pins git config so a developer's global excludes cannot change a listing,
/// and serialises tests that depend on it.
pub(crate) fn pinned() -> MutexGuard<'static, ()> {
    let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("GIT_CONFIG_GLOBAL", "/dev/null");
    std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
    guard
}

/// A scratch repo with its own index path.
pub(crate) struct Fx {
    pub root: PathBuf,
    pub index: PathBuf,
    /// The worktree key every run and query of this fixture uses.
    pub worktree: String,
}

pub(crate) fn fx(tag: &str) -> Fx {
    let root = init(tag);
    let index = playbook_common::test_support::scratch_dir(&format!("{tag}-idx"))
        .join("code")
        .join("store-v4.sqlite3");
    Fx {
        root,
        index,
        worktree: "fixture-worktree".to_string(),
    }
}

impl Fx {
    pub(crate) fn write(&self, rel: &str, text: &str) {
        write(&self.root, rel, text.as_bytes());
    }

    pub(crate) fn git_add_all(&self) {
        git_in(&self.root, &["add", "."]);
    }

    pub(crate) fn run(&self) -> Report {
        self.run_with(&Options::default())
    }

    pub(crate) fn run_with(&self, opts: &Options) -> Report {
        index::run(&self.root, &self.index, &self.worktree, false, opts).expect("index run")
    }

    pub(crate) fn conn(&self) -> db::Store {
        db::open_query(&self.index, &self.worktree).expect("query open")
    }

    pub(crate) fn set_mtime(&self, rel: &str, secs: u64) {
        let file = File::options()
            .write(true)
            .open(self.root.join(rel))
            .expect("open");
        file.set_modified(UNIX_EPOCH + Duration::from_secs(secs))
            .expect("set mtime");
    }
}
