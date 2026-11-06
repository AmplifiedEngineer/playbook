// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The frozen task set. Tasks come from ctags symbols (never from the index
//! under test), one tier per symbol kind, picked by a seeded shuffle so the
//! same input always gives the same file. The set carries a version that the
//! runner checks against the baseline's.

use crate::ctags::{Symbol, SymbolKind};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The bundled task set.
pub const BUNDLED: &str = include_str!("../data/tasks-v1.json");

/// Version written by the generator. A new symbol kind bumps it.
pub const TASKS_VERSION: u32 = 1;

/// Tasks picked per repo and symbol kind.
pub const TASKS_PER_KIND: usize = 25;

/// Seed of the committed task set.
pub const DEFAULT_SEED: u64 = 20_261_106;

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
const SPLITMIX_GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;
const SPLITMIX_MUL_A: u64 = 0xbf58_476d_1ce4_e5b9;
const SPLITMIX_MUL_B: u64 = 0x94d0_49bb_1331_11eb;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TaskError {
    #[error("task set does not parse: {0}")]
    Parse(String),
    #[error("task set does not serialize: {0}")]
    Serialize(String),
    #[error("task id {0} is listed twice")]
    DuplicateId(String),
    #[error("baseline is version {baseline} and the task set is version {tasks}")]
    VersionMismatch { baseline: u32, tasks: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub id: String,
    pub repo: String,
    pub kind: SymbolKind,
    pub name: String,
    pub path: String,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSet {
    pub version: u32,
    pub seed: u64,
    /// Where the symbols came from, such as the exact ctags version.
    pub source: String,
    pub tasks: Vec<Task>,
}

/// A handle to the index store. The generator accepts one so a test can prove
/// it never opens it.
pub trait IndexStore {
    fn open(&self) -> Result<(), String>;
}

/// The symbols of one repo.
pub struct RepoSymbols {
    pub repo: String,
    pub symbols: Vec<Symbol>,
}

fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(SPLITMIX_GAMMA);
    x = (x ^ (x >> 30)).wrapping_mul(SPLITMIX_MUL_A);
    x = (x ^ (x >> 27)).wrapping_mul(SPLITMIX_MUL_B);
    x ^ (x >> 31)
}

fn stream_seed(seed: u64, label: &str) -> u64 {
    let hash = label.bytes().fold(FNV_OFFSET, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(FNV_PRIME)
    });
    seed ^ hash
}

fn shuffle<T>(items: &mut [T], seed: u64) {
    let mut state = seed;
    for i in (1..items.len()).rev() {
        state = mix(state);
        let j = (state % (i as u64 + 1)) as usize;
        items.swap(i, j);
    }
}

fn task_id(repo: &str, kind: SymbolKind, name: &str, path: &str) -> String {
    format!("{repo}/{}/{name}@{path}", kind.as_str())
}

/// Builds the task set. `_store` is never opened.
pub fn generate(
    repos: &[RepoSymbols],
    seed: u64,
    source: &str,
    _store: &dyn IndexStore,
) -> TaskSet {
    let mut tasks = Vec::new();
    for entry in repos {
        let mut tiers: BTreeMap<SymbolKind, BTreeMap<String, Task>> = BTreeMap::new();
        for s in &entry.symbols {
            let id = task_id(&entry.repo, s.kind, &s.name, &s.path);
            tiers
                .entry(s.kind)
                .or_default()
                .entry(id.clone())
                .or_insert(Task {
                    id,
                    repo: entry.repo.clone(),
                    kind: s.kind,
                    name: s.name.clone(),
                    path: s.path.clone(),
                    line: s.line,
                });
        }
        for (kind, candidates) in tiers {
            let mut pool: Vec<Task> = candidates.into_values().collect();
            shuffle(
                &mut pool,
                stream_seed(seed, &format!("{}/{}", entry.repo, kind.as_str())),
            );
            pool.truncate(TASKS_PER_KIND);
            tasks.extend(pool);
        }
    }
    tasks.sort_by(|a, b| a.id.cmp(&b.id));
    TaskSet {
        version: TASKS_VERSION,
        seed,
        source: source.to_string(),
        tasks,
    }
}

impl TaskSet {
    pub fn parse(text: &str) -> Result<Self, TaskError> {
        let set: TaskSet =
            serde_json::from_str(text).map_err(|e| TaskError::Parse(e.to_string()))?;
        let mut seen = BTreeSet::new();
        for task in &set.tasks {
            if !seen.insert(task.id.as_str()) {
                return Err(TaskError::DuplicateId(task.id.clone()));
            }
        }
        Ok(set)
    }

    pub fn bundled() -> Result<Self, TaskError> {
        Self::parse(BUNDLED)
    }

    /// The file text: pretty JSON with a trailing newline.
    pub fn to_json(&self) -> Result<String, TaskError> {
        let mut text =
            serde_json::to_string_pretty(self).map_err(|e| TaskError::Serialize(e.to_string()))?;
        text.push('\n');
        Ok(text)
    }

    /// Refuses a task set whose version differs from the baseline's.
    pub fn require_version(&self, baseline: u32) -> Result<(), TaskError> {
        if self.version == baseline {
            return Ok(());
        }
        Err(TaskError::VersionMismatch {
            baseline,
            tasks: self.version,
        })
    }

    /// The expected kinds that have no task for `repo`.
    pub fn unmeasured_kinds(&self, repo: &str, expected: &[SymbolKind]) -> Vec<SymbolKind> {
        expected
            .iter()
            .copied()
            .filter(|k| !self.tasks.iter().any(|t| t.repo == repo && t.kind == *k))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::Manifest;
    use std::cell::Cell;

    struct FailingStore(Cell<u32>);

    impl IndexStore for FailingStore {
        fn open(&self) -> Result<(), String> {
            self.0.set(self.0.get() + 1);
            Err("store must not be opened".into())
        }
    }

    fn symbols(count: u32) -> Vec<RepoSymbols> {
        let symbols = (0..count)
            .map(|i| Symbol {
                path: format!("f{}.go", i % 3),
                line: i + 1,
                name: format!("name{i}"),
                kind: if i % 2 == 0 {
                    SymbolKind::Function
                } else {
                    SymbolKind::Struct
                },
            })
            .collect();
        vec![RepoSymbols {
            repo: "cobra".into(),
            symbols,
        }]
    }

    fn build(count: u32, seed: u64, store: &FailingStore) -> TaskSet {
        generate(&symbols(count), seed, "test", store)
    }

    #[test]
    fn same_seed_gives_identical_output_and_never_opens_the_store() {
        let store = FailingStore(Cell::new(0));
        let a = build(200, 7, &store);
        let b = build(200, 7, &store);
        assert_eq!(a.to_json().unwrap(), b.to_json().unwrap());
        assert_eq!(store.0.get(), 0);
        assert_ne!(a.tasks, build(200, 8, &store).tasks);
    }

    #[test]
    fn each_tier_is_capped_and_output_is_sorted() {
        let set = build(200, 7, &FailingStore(Cell::new(0)));
        for kind in [SymbolKind::Function, SymbolKind::Struct] {
            let n = set.tasks.iter().filter(|t| t.kind == kind).count();
            assert_eq!(n, TASKS_PER_KIND);
        }
        assert!(set.tasks.windows(2).all(|w| w[0].id < w[1].id));
        let small = build(4, 7, &FailingStore(Cell::new(0)));
        assert_eq!(small.tasks.len(), 4);
    }

    #[test]
    fn version_mismatch_names_both_versions() {
        let set = build(4, 7, &FailingStore(Cell::new(0)));
        assert_eq!(set.require_version(TASKS_VERSION), Ok(()));
        let err = set.require_version(2).unwrap_err();
        assert_eq!(
            err.to_string(),
            "baseline is version 2 and the task set is version 1"
        );
    }

    #[test]
    fn missing_tier_is_reported_as_unmeasured() {
        let set = build(4, 7, &FailingStore(Cell::new(0)));
        let expected = [SymbolKind::Function, SymbolKind::Trait];
        assert_eq!(
            set.unmeasured_kinds("cobra", &expected),
            [SymbolKind::Trait]
        );
        assert_eq!(set.unmeasured_kinds("other", &expected), expected);
    }

    #[test]
    fn duplicate_ids_and_unknown_fields_fail_to_parse() {
        let set = build(4, 7, &FailingStore(Cell::new(0)));
        let mut twice = set.clone();
        twice.tasks.push(set.tasks[0].clone());
        assert!(matches!(
            TaskSet::parse(&twice.to_json().unwrap()),
            Err(TaskError::DuplicateId(_))
        ));
        assert!(matches!(
            TaskSet::parse("{\"extra\":1}"),
            Err(TaskError::Parse(_))
        ));
        assert_eq!(TaskSet::parse(&set.to_json().unwrap()).unwrap(), set);
    }

    #[test]
    fn committed_set_is_valid_sorted_and_matches_the_manifest() {
        let set = TaskSet::bundled().unwrap();
        assert_eq!(set.version, TASKS_VERSION);
        assert!(!set.tasks.is_empty());
        assert!(set.tasks.windows(2).all(|w| w[0].id < w[1].id));
        let manifest = Manifest::bundled().unwrap();
        for task in &set.tasks {
            assert!(
                manifest.repos.iter().any(|r| r.name == task.repo),
                "{}",
                task.id
            );
            assert_eq!(
                task.id,
                task_id(&task.repo, task.kind, &task.name, &task.path)
            );
        }
        assert_eq!(set.to_json().unwrap(), BUNDLED);
    }
}
