// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The gate on the committed fixtures, against the committed fixture
//! baseline, with the production tolerances. Only the task minimum differs:
//! the fixtures are small, so a cell needs `FIXTURE_MIN_TASKS_PER_CELL`.

use code_bench::fixtures::{
    baseline_path, binary, judge, measure, FIXTURE_MIN_TASKS_PER_CELL, LANGUAGES,
};
use code_bench::gate::{Baseline, Failure, Metric};
use code_bench::process::RunError;
use code_bench::runner::{CodeRunner, QueryKind, Report};
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const TIME_FILE_ENV: &str = "FIXTURE_GATE_TIME_FILE";
const TIME_BUDGET: Duration = Duration::from_secs(120);
const DROPPED_REPO: &str = "rust";
const DROPPED_PATH: &str = "src/lib.rs";
const DROPPED_LINE: u64 = 8;

fn scratch(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("fixture-gate-{tag}-{}", std::process::id()))
}

fn committed() -> Baseline {
    let text = std::fs::read_to_string(baseline_path()).unwrap();
    Baseline::parse(&text).unwrap()
}

fn run_fixtures(tag: &str, wrap: impl Fn(Box<dyn CodeRunner>) -> Box<dyn CodeRunner>) -> Report {
    let dir = scratch(tag);
    let inner: Box<dyn CodeRunner> = Box::new(binary(&dir).unwrap());
    let report = measure(wrap(inner).as_ref(), &dir).unwrap();
    std::fs::remove_dir_all(dir).unwrap();
    report
}

fn real_report() -> &'static Report {
    static REPORT: OnceLock<Report> = OnceLock::new();
    REPORT.get_or_init(|| run_fixtures("real", |r| r))
}

/// A runner that hides one labeled symbol from every `symbol` answer, as an
/// extractor that lost it would.
struct DropsOneSymbol(Box<dyn CodeRunner>);

impl CodeRunner for DropsOneSymbol {
    fn run(&self, args: &[String], cwd: &Path) -> Result<String, RunError> {
        let out = self.0.run(args, cwd)?;
        let in_repo = cwd.starts_with(scratch("dropped").join(DROPPED_REPO));
        if args.first().map(String::as_str) != Some("symbol") || !in_repo {
            return Ok(out);
        }
        let mut json: serde_json::Value = serde_json::from_str(&out).unwrap();
        if let Some(results) = json["results"].as_array_mut() {
            results.retain(|h| {
                (h["path"].as_str(), h["line"].as_u64()) != (Some(DROPPED_PATH), Some(DROPPED_LINE))
            });
        }
        Ok(json.to_string())
    }
}

#[test]
fn the_committed_baseline_is_measured_in_every_cell() {
    let baseline = committed();
    assert_eq!(baseline.repos.len(), LANGUAGES.len());
    for repo in &baseline.repos {
        assert_eq!(repo.cells.len(), QueryKind::ALL.len(), "{}", repo.repo);
        for cell in &repo.cells {
            assert!(
                cell.tasks >= FIXTURE_MIN_TASKS_PER_CELL && cell.correct > 0,
                "{} {:?} is not measured",
                repo.repo,
                cell.kind
            );
        }
    }
}

#[test]
fn the_committed_index_passes_the_committed_baseline() {
    let failures = judge(&committed(), real_report());
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn an_extractor_that_drops_a_labeled_symbol_fails_the_gate() {
    let report = run_fixtures("dropped", |inner| Box::new(DropsOneSymbol(inner)));
    let failures = judge(&committed(), &report);
    assert!(
        failures.iter().any(|f| matches!(
            f,
            Failure::Regression { repo, kind: QueryKind::Symbol, metric: Metric::Recall, .. }
                if repo == DROPPED_REPO
        )),
        "{failures:#?}"
    );
}

#[test]
fn a_cell_of_two_tasks_is_unmeasured_and_a_cell_of_three_passes() {
    let mut report = real_report().clone();
    let refs = QueryKind::ALL
        .iter()
        .position(|k| *k == QueryKind::Refs)
        .unwrap();
    assert_eq!(
        report.repos[0].cells[refs].tasks,
        FIXTURE_MIN_TASKS_PER_CELL
    );
    assert!(judge(&committed(), &report).is_empty());
    report.repos[0].cells[refs].tasks = FIXTURE_MIN_TASKS_PER_CELL - 1;
    let failures = judge(&committed(), &report);
    assert!(
        matches!(failures.as_slice(), [Failure::Unmeasured { tasks, .. }] if *tasks == FIXTURE_MIN_TASKS_PER_CELL - 1),
        "{failures:#?}"
    );
}

#[test]
fn the_run_time_is_recorded_for_the_ci_artifact() {
    let start = Instant::now();
    run_fixtures("timed", |r| r);
    if let Ok(file) = std::env::var(TIME_FILE_ENV) {
        std::fs::write(file, format!("{}\n", start.elapsed().as_millis())).unwrap();
    }
}

#[test]
#[ignore = "wall-clock budget, run by the scheduled workflow"]
fn the_fixture_gate_stays_inside_its_time_budget() {
    let start = Instant::now();
    let report = run_fixtures("budget", |r| r);
    assert!(judge(&committed(), &report).is_empty());
    assert!(start.elapsed() < TIME_BUDGET, "took {:?}", start.elapsed());
}
