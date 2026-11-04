// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The gc engine: a registry of stores, a safety guard that every removal goes
//! through, and one run lock. Stores arrive in later units; this module holds
//! the types they share.

pub mod build_output;
pub mod guard;
pub mod index;
pub mod lock;
pub mod report;
pub mod space;
pub mod stores;
pub mod tombstone;
pub mod worktrees;

use std::path::{Path, PathBuf};
use std::time::Duration;

/// The folder under the playbook root that holds gc state.
pub const GC_DIR: &str = "gc";
/// The folder under [`GC_DIR`] that holds tombstoned items.
pub const TOMBSTONES_DIR: &str = "tombstones";
/// A lock folder, or a pending record, older than this belongs to a dead run.
pub const RUN_LOCK_STALE: Duration = Duration::from_secs(3600);
/// Name of the environment variable that sets the clock in debug builds.
const TEST_NOW_VAR: &str = "PLAYBOOK_TEST_NOW";

/// A source of the current time as epoch seconds, so tests control it.
pub trait Clock {
    fn now(&self) -> i64;
}

/// The wall clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        crate::common::time::now_secs()
    }
}

/// A fixed time, used by tests and by the debug-only environment override.
pub struct FixedClock(pub i64);

impl Clock for FixedClock {
    fn now(&self) -> i64 {
        self.0
    }
}

/// The clock the binary uses. A debug build reads `PLAYBOOK_TEST_NOW` (epoch
/// seconds) so integration tests that run the real binary can set the time.
/// A release build always uses the wall clock.
pub fn default_clock() -> Box<dyn Clock> {
    #[cfg(debug_assertions)]
    if let Some(fixed) = std::env::var(TEST_NOW_VAR)
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
    {
        return Box::new(FixedClock(fixed));
    }
    Box::new(SystemClock)
}

/// What a store does with an item that has outlived its rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Delete,
    Tombstone,
    Rotate,
    ReportOnly,
}

impl Action {
    /// The word the report prints.
    pub fn label(self) -> &'static str {
        match self {
            Self::Delete => "delete",
            Self::Tombstone => "tombstone",
            Self::Rotate => "rotate",
            Self::ReportOnly => "report only",
        }
    }
}

/// One thing a store found: a file or folder, its size and its age.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub path: PathBuf,
    pub bytes: u64,
    /// Newest modification time found, as epoch seconds.
    pub newest_mtime: i64,
    /// Set when the store's rule says this item should go now.
    pub due: Option<Due>,
}

/// Why an item is due and how long its tombstone is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Due {
    pub reason: String,
    pub retain_days: u32,
}

/// Lists the items under a store's roots. Takes `now` so a rule can be tested
/// without the wall clock.
pub type ScanFn = fn(roots: &[PathBuf], now: i64) -> Vec<Item>;

/// One kind of on-disk state with a retention rule.
#[derive(Debug, Clone)]
pub struct Store {
    pub name: &'static str,
    pub roots: Vec<PathBuf>,
    pub scan: ScanFn,
    pub action: Action,
    /// The config key that sets this store's retention, if it has one.
    pub retention_key: Option<&'static str>,
}

impl Store {
    pub fn items(&self, now: i64) -> Vec<Item> {
        (self.scan)(&self.roots, now)
    }
}

/// Every store, plus the root that holds tombstones.
#[derive(Debug, Clone)]
pub struct Registry {
    gc_dir: PathBuf,
    stores: Vec<Store>,
}

impl Registry {
    /// An empty registry for the gc folder `gc_dir`.
    pub fn new(gc_dir: PathBuf) -> Self {
        Self {
            gc_dir,
            stores: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_store(mut self, store: Store) -> Self {
        self.stores.push(store);
        self
    }

    pub fn stores(&self) -> &[Store] {
        &self.stores
    }

    pub fn gc_dir(&self) -> &Path {
        &self.gc_dir
    }

    pub fn tombstones_dir(&self) -> PathBuf {
        self.gc_dir.join(TOMBSTONES_DIR)
    }

    /// The paths gc may remove inside: each store's roots and the tombstones
    /// folder.
    pub fn roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = self
            .stores
            .iter()
            .flat_map(|store| store.roots.iter().cloned())
            .collect();
        roots.push(self.tombstones_dir());
        roots
    }
}

/// The production registry for the playbook root `root`. Each unit that adds a
/// store adds one line here.
pub fn registry(root: &Path) -> Registry {
    Registry::new(root.join(GC_DIR))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tombstone_root_is_registered() {
        let registry = registry(Path::new("/pb"));

        let roots = registry.roots();

        assert!(roots.contains(&PathBuf::from("/pb/gc/tombstones")));
    }

    #[test]
    fn registry_roots_include_every_store_root() {
        let store = Store {
            name: "t",
            roots: vec![PathBuf::from("/a"), PathBuf::from("/b")],
            scan: |_, _| Vec::new(),
            action: Action::Delete,
            retention_key: None,
        };
        let registry = Registry::new(PathBuf::from("/pb/gc")).with_store(store);

        let roots = registry.roots();

        assert!(roots.contains(&PathBuf::from("/a")) && roots.contains(&PathBuf::from("/b")));
    }

    #[test]
    fn fixed_clock_returns_its_time() {
        assert_eq!(FixedClock(42).now(), 42);
    }
}
