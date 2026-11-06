// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The committed data files parse, list every corpus repo, and never fall
//! below their merge-base copies without a recorded override. CI checks out
//! the full history so the merge base exists; a missing one fails the test.

use code_bench::corpus::Manifest;
use code_bench::gate::{Baseline, Floor, Overrides};
use code_bench::merge_base::{check, DataFile, GitReader, MergeBaseReader, Shape};
use code_bench::mutants::floor_lowered;
use code_bench::process::{ProcessRunner, SystemRunner};
use code_bench::runner::QueryKind;
use code_bench::tasks::TASKS_VERSION;
use std::path::{Path, PathBuf};

const BASELINE: &str = "crates/code-bench/data/baseline.json";
const FLOOR: &str = "crates/code-bench/data/floor.json";
const OVERRIDES: &str = "crates/code-bench/data/overrides.json";
const GATE_BASELINE: &str = "crates/code/tests/fixtures/gate-baseline.json";
const NOT_IN_CORPUS: &str = "not yet in the corpus";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &str) -> Option<String> {
    std::fs::read_to_string(root().join(path)).ok()
}

fn required(path: &str) -> String {
    read(path).unwrap_or_else(|| panic!("{path} is missing"))
}

#[test]
fn baseline_lists_every_corpus_repo_as_measured_or_waived() {
    let baseline = Baseline::parse(&required(BASELINE)).unwrap();
    assert_eq!(baseline.tasks_version, TASKS_VERSION);
    let manifest = Manifest::bundled().unwrap();
    for repo in &manifest.repos {
        let measured = baseline.repos.iter().any(|r| r.repo == repo.name);
        let whole = baseline
            .waivers
            .iter()
            .find(|w| w.repo == repo.name && w.kind.is_none());
        let cells = QueryKind::ALL.iter().all(|k| {
            baseline
                .waivers
                .iter()
                .any(|w| w.repo == repo.name && w.kind == Some(*k))
        });
        assert!(
            measured || whole.is_some() || cells,
            "{} is not listed",
            repo.name
        );
        if let Some(w) = whole {
            assert_eq!(w.reason, NOT_IN_CORPUS, "{}", repo.name);
        }
    }
}

#[test]
fn floor_and_overrides_parse() {
    Floor::parse(&required(FLOOR)).unwrap();
    Overrides::parse(&required(OVERRIDES)).unwrap();
}

#[test]
fn data_files_are_not_lower_than_the_merge_base() {
    let runner = SystemRunner;
    let date_args = ["log", "-1", "--format=%cs"].map(String::from);
    let run_date = runner.run("git", &date_args, &root()).unwrap();
    let files = [
        ("baseline", BASELINE, Shape::Baseline),
        ("floor", FLOOR, Shape::Floor),
        ("gate-baseline", GATE_BASELINE, Shape::Baseline),
    ]
    .map(|(name, path, shape)| DataFile {
        name,
        path,
        shape,
        current: read(path),
    });
    let overrides = Overrides::parse(&required(OVERRIDES)).unwrap();
    let reader = GitReader {
        runner: &runner,
        root: &root(),
    };
    let found = check(&files, &reader, &overrides, run_date.trim());
    assert!(found.is_empty(), "{found:#?}");
}

const MUTATION_OVERRIDE: &str = "floor:mutation";

#[test]
fn the_mutation_floor_is_not_lower_than_the_merge_base() {
    let runner = SystemRunner;
    let date_args = ["log", "-1", "--format=%cs"].map(String::from);
    let run_date = runner.run("git", &date_args, &root()).unwrap();
    let reader = GitReader {
        runner: &runner,
        root: &root(),
    };
    let before = reader
        .read(FLOOR)
        .unwrap()
        .map(|text| Floor::parse(&text).unwrap().mutation_floor_bps);
    let now = Floor::parse(&required(FLOOR)).unwrap().mutation_floor_bps;
    let overrides = Overrides::parse(&required(OVERRIDES)).unwrap();
    let lowered = floor_lowered(before.flatten(), now);
    assert!(
        !lowered
            || overrides
                .active(MUTATION_OVERRIDE, run_date.trim())
                .is_some()
    );
}
