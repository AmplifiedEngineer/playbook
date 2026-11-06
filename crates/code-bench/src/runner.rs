// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The pilot runner. It runs the frozen tasks through the real `playbook code`
//! binary, behind an injected command runner, and reports precision, recall,
//! bytes and tokens per repo and query kind. Precision and recall are micro
//! averages: every task's results and truth are pooled before the ratio.

use crate::metrics::{self, BasisPoints};
use crate::oracle::rg::Candidate;
use crate::process::RunError;
use crate::tasks::{Task, TaskSet};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Most results asked of each query, the CLI maximum, so recall is not capped
/// by the default limit.
pub const MAX_RESULTS: &str = "200";

const SUBCOMMAND: &str = "code";
const HOME_ENV: &str = "HOME";

/// What a query asks of the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueryKind {
    /// Where is the name declared. Truth is the task's own declaration.
    Symbol,
    /// Where is the name used. Truth is the oracle's positives.
    Refs,
}

impl QueryKind {
    pub const ALL: [QueryKind; 2] = [QueryKind::Symbol, QueryKind::Refs];

    fn subcommand(self) -> &'static str {
        match self {
            QueryKind::Symbol => "symbol",
            QueryKind::Refs => "refs",
        }
    }
}

/// Runs `playbook code <args>` in `cwd` and returns its standard output.
pub trait CodeRunner {
    fn run(&self, args: &[String], cwd: &Path) -> Result<String, RunError>;
}

/// The real binary. `home` holds its store, so a run never touches the
/// caller's own index.
pub struct PlaybookBinary {
    pub program: PathBuf,
    pub home: PathBuf,
}

impl CodeRunner for PlaybookBinary {
    fn run(&self, args: &[String], cwd: &Path) -> Result<String, RunError> {
        let out = Command::new(&self.program)
            .arg(SUBCOMMAND)
            .args(args)
            .current_dir(cwd)
            .env(HOME_ENV, &self.home)
            .output()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => RunError::NotFound,
                _ => RunError::Failed(e.to_string()),
            })?;
        if !out.status.success() {
            return Err(RunError::Failed(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// One repo to run. A failed clone arrives as `Err` and fails the repo.
pub struct RepoJob {
    pub repo: String,
    pub checkout: Result<PathBuf, String>,
    pub tasks: Vec<Task>,
    /// Oracle positives by task id. A task with no entry has no refs truth.
    pub truth: BTreeMap<String, Vec<Candidate>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum RepoStatus {
    Measured,
    Failed { reason: String },
}

/// One repo and query kind pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    pub kind: QueryKind,
    /// Tasks that were measured.
    pub tasks: u32,
    /// Tasks left out because they have no truth for this kind.
    pub excluded: u32,
    pub precision_bps: Option<BasisPoints>,
    pub recall_bps: Option<BasisPoints>,
    pub correct: u64,
    pub bytes: u64,
    pub tokens: u64,
    pub bytes_per_correct: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoReport {
    pub repo: String,
    #[serde(flatten)]
    pub status: RepoStatus,
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub tasks_version: u32,
    pub repos: Vec<RepoReport>,
}

impl Report {
    /// The file text: pretty JSON with a trailing newline.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        Ok(text)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RunnerError {
    #[error("a task set of version {tasks} cannot run against a baseline of version {baseline}")]
    Version { baseline: u32, tasks: u32 },
}

#[derive(Deserialize)]
struct Location {
    path: String,
    line: u32,
}

/// Reads an oracle answers file: task id to the positive locations.
pub fn parse_answers(text: &str) -> Result<BTreeMap<String, Vec<Candidate>>, String> {
    let raw: BTreeMap<String, Vec<Location>> =
        serde_json::from_str(text).map_err(|e| format!("answers do not parse: {e}"))?;
    Ok(raw
        .into_iter()
        .map(|(id, found)| {
            let found = found
                .into_iter()
                .map(|l| Candidate {
                    path: l.path,
                    line: l.line,
                })
                .collect();
            (id, found)
        })
        .collect())
}

#[derive(Deserialize)]
struct Output {
    results: Vec<Hit>,
}

#[derive(Deserialize)]
struct Hit {
    path: String,
    line: u32,
}

type Pooled = (usize, String, u32);

#[derive(Default)]
struct Tally {
    measured: u32,
    excluded: u32,
    retrieved: Vec<Pooled>,
    truth: Vec<Pooled>,
    correct: u64,
    bytes: u64,
    tokens: u64,
}

impl Tally {
    fn add(&mut self, index: usize, stdout: &str, truth: &[(String, u32)]) -> Result<(), String> {
        let parsed: Output =
            serde_json::from_str(stdout).map_err(|e| format!("output does not parse: {e}"))?;
        let wanted: BTreeSet<(&str, u32)> = truth.iter().map(|(p, l)| (p.as_str(), *l)).collect();
        let found: BTreeSet<(&str, u32)> = parsed
            .results
            .iter()
            .map(|h| (h.path.as_str(), h.line))
            .filter(|hit| wanted.contains(hit))
            .collect();
        self.correct += found.len() as u64;
        self.measured += 1;
        self.bytes += metrics::bytes(stdout);
        self.tokens += metrics::tokens(stdout);
        self.retrieved
            .extend(parsed.results.into_iter().map(|h| (index, h.path, h.line)));
        self.truth
            .extend(truth.iter().map(|(p, l)| (index, p.clone(), *l)));
        Ok(())
    }

    fn cell(self, kind: QueryKind) -> Cell {
        Cell {
            kind,
            tasks: self.measured,
            excluded: self.excluded,
            precision_bps: metrics::precision(&self.retrieved, &self.truth),
            recall_bps: metrics::recall(&self.retrieved, &self.truth),
            correct: self.correct,
            bytes: self.bytes,
            tokens: self.tokens,
            bytes_per_correct: metrics::cost_per_correct(self.bytes, self.correct),
        }
    }
}

fn truth_for(kind: QueryKind, task: &Task, job: &RepoJob) -> Option<Vec<(String, u32)>> {
    match kind {
        QueryKind::Symbol => Some(vec![(task.path.clone(), task.line)]),
        QueryKind::Refs => job.truth.get(&task.id).map(|found| {
            found
                .iter()
                .map(|c| (c.path.clone(), c.line))
                .collect::<Vec<_>>()
        }),
    }
}

fn query(kind: QueryKind, name: &str) -> Vec<String> {
    [kind.subcommand(), name, "--limit", MAX_RESULTS, "--json"]
        .map(String::from)
        .into()
}

fn run_repo(runner: &dyn CodeRunner, job: &RepoJob) -> Result<Vec<Cell>, String> {
    let checkout = job.checkout.as_ref().map_err(|e| format!("clone: {e}"))?;
    let index_args = ["index", "--json"].map(String::from);
    runner
        .run(&index_args, checkout)
        .map_err(|e| format!("index: {e:?}"))?;
    let mut cells = Vec::new();
    for kind in QueryKind::ALL {
        let mut tally = Tally::default();
        for (i, task) in job.tasks.iter().enumerate() {
            let Some(truth) = truth_for(kind, task, job) else {
                tally.excluded += 1;
                continue;
            };
            let stdout = runner
                .run(&query(kind, &task.name), checkout)
                .map_err(|e| format!("{} {}: {e:?}", kind.subcommand(), task.id))?;
            tally
                .add(i, &stdout, &truth)
                .map_err(|e| format!("{} {}: {e}", kind.subcommand(), task.id))?;
        }
        cells.push(tally.cell(kind));
    }
    Ok(cells)
}

/// Runs every job. `baseline_version` is the version the report will be
/// compared with; a task set of another version is refused.
pub fn run(
    runner: &dyn CodeRunner,
    tasks: &TaskSet,
    baseline_version: u32,
    jobs: &[RepoJob],
) -> Result<Report, RunnerError> {
    tasks
        .require_version(baseline_version)
        .map_err(|_| RunnerError::Version {
            baseline: baseline_version,
            tasks: tasks.version,
        })?;
    let repos = jobs
        .iter()
        .map(|job| match run_repo(runner, job) {
            Ok(cells) => RepoReport {
                repo: job.repo.clone(),
                status: RepoStatus::Measured,
                cells,
            },
            Err(reason) => RepoReport {
                repo: job.repo.clone(),
                status: RepoStatus::Failed { reason },
                cells: Vec::new(),
            },
        })
        .collect();
    Ok(Report {
        tasks_version: tasks.version,
        repos,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctags::SymbolKind;
    use std::cell::RefCell;

    struct Stub {
        calls: RefCell<Vec<String>>,
    }

    impl CodeRunner for Stub {
        fn run(&self, args: &[String], _cwd: &Path) -> Result<String, RunError> {
            self.calls.borrow_mut().push(args.join(" "));
            Ok(match args[0].as_str() {
                "index" => "{}".to_string(),
                "symbol" => r#"{"results":[{"path":"a.rs","line":3}]}"#.to_string(),
                _ => r#"{"results":[{"path":"a.rs","line":3},{"path":"b.rs","line":9},{"path":"c.rs","line":1}]}"#
                    .to_string(),
            })
        }
    }

    fn task(id: &str) -> Task {
        Task {
            id: id.into(),
            repo: "r".into(),
            kind: SymbolKind::Function,
            name: "f".into(),
            path: "a.rs".into(),
            line: 3,
        }
    }

    fn set(version: u32) -> TaskSet {
        TaskSet {
            version,
            seed: 0,
            source: String::new(),
            tasks: vec![task("t1")],
        }
    }

    fn job(truth: BTreeMap<String, Vec<Candidate>>) -> RepoJob {
        RepoJob {
            repo: "r".into(),
            checkout: Ok(PathBuf::from(".")),
            tasks: vec![task("t1")],
            truth,
        }
    }

    fn cand(path: &str, line: u32) -> Candidate {
        Candidate {
            path: path.into(),
            line,
        }
    }

    fn stub() -> Stub {
        Stub {
            calls: RefCell::new(Vec::new()),
        }
    }

    #[test]
    fn report_holds_the_values_the_stub_produces() {
        let truth = BTreeMap::from([("t1".to_string(), vec![cand("a.rs", 3), cand("b.rs", 9)])]);
        let report = run(&stub(), &set(1), 1, &[job(truth)]).unwrap();
        let repo = &report.repos[0];
        assert_eq!(repo.status, RepoStatus::Measured);
        let symbol = &repo.cells[0];
        assert_eq!(symbol.kind, QueryKind::Symbol);
        assert_eq!(symbol.precision_bps, Some(10_000));
        assert_eq!(symbol.recall_bps, Some(10_000));
        let refs = &repo.cells[1];
        assert_eq!(refs.precision_bps, Some(6667));
        assert_eq!(refs.recall_bps, Some(10_000));
        assert_eq!(refs.correct, 2);
        let out = r#"{"results":[{"path":"a.rs","line":3},{"path":"b.rs","line":9},{"path":"c.rs","line":1}]}"#;
        assert_eq!(refs.bytes, out.len() as u64);
        assert_eq!(refs.bytes_per_correct, Some(out.len() as u64 / 2));
    }

    #[test]
    fn a_task_without_truth_is_excluded_from_refs() {
        let report = run(&stub(), &set(1), 1, &[job(BTreeMap::new())]).unwrap();
        let refs = &report.repos[0].cells[1];
        assert_eq!((refs.tasks, refs.excluded), (0, 1));
        assert_eq!(refs.precision_bps, None);
        assert_eq!(refs.recall_bps, None);
    }

    #[test]
    fn a_failing_clone_marks_the_repo_failed() {
        let mut failing = job(BTreeMap::new());
        failing.checkout = Err("network down".into());
        let runner = stub();
        let report = run(&runner, &set(1), 1, &[failing]).unwrap();
        assert_eq!(
            report.repos[0].status,
            RepoStatus::Failed {
                reason: "clone: network down".into()
            }
        );
        assert!(report.repos[0].cells.is_empty());
        assert!(runner.calls.borrow().is_empty());
    }

    #[test]
    fn mismatched_versions_are_refused_and_named() {
        let err = run(&stub(), &set(2), 1, &[]).unwrap_err();
        assert_eq!(
            err,
            RunnerError::Version {
                baseline: 1,
                tasks: 2
            }
        );
        assert!(err.to_string().contains('2') && err.to_string().contains('1'));
    }

    #[test]
    fn answers_parse_and_malformed_answers_fail() {
        let parsed = parse_answers(r#"{"t1":[{"path":"a.rs","line":3}]}"#).unwrap();
        assert_eq!(parsed["t1"], vec![cand("a.rs", 3)]);
        assert!(parse_answers("[]").is_err());
    }
}
