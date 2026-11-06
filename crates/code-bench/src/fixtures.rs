// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The fixture gate: the committed fixture repos turned into tasks and run
//! through the real binary. The `labels` file of each fixture is the truth:
//! every `symbol` line is a task, and the `ref` lines of a name are the truth
//! of the refs query for that name. The same code writes the committed
//! baseline and judges a run against it.

use crate::ctags::SymbolKind;
use crate::gate::{evaluate, Baseline, Failure, Floor, Inputs, Overrides};
use crate::oracle::rg::Candidate;
use crate::runner::{self, CodeRunner, PlaybookBinary, RepoJob, Report};
use crate::tasks::{Task, TaskSet, TASKS_VERSION};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Fewest tasks a fixture cell needs. Production uses `MIN_TASKS_PER_CELL`.
pub const FIXTURE_MIN_TASKS_PER_CELL: u32 = 3;
/// The fixture languages, one repo each.
pub const LANGUAGES: [&str; 5] = ["rust", "typescript", "javascript", "python", "go"];
/// The date handed to the gate; no override applies to the fixtures.
pub const RUN_DATE: &str = "2026-01-01";

const FIXTURES_DIR: &str = "../code/tests/fixtures";
const BASELINE_FILE: &str = "gate-baseline.json";
const LABELS_FILE: &str = "labels";
const REPO_DIR: &str = "repo";
const HOME_DIR: &str = "home";
const SOURCE: &str = "fixture labels";
const ORIGIN: &str = "https://github.com/test-owner/test-repo.git";
const PROGRAM: &str = "playbook";
const PACKAGE: &str = "playbook";

pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURES_DIR)
}

pub fn baseline_path() -> PathBuf {
    fixtures_dir().join(BASELINE_FILE)
}

fn kind_of(label: &str) -> Option<SymbolKind> {
    Some(match label {
        "function" => SymbolKind::Function,
        "method" => SymbolKind::Method,
        "class" => SymbolKind::Class,
        "struct" => SymbolKind::Struct,
        "interface" => SymbolKind::Interface,
        "enum" => SymbolKind::Enum,
        "trait" => SymbolKind::Trait,
        "type" => SymbolKind::Type,
        "const" | "variable" => SymbolKind::Constant,
        _ => return None,
    })
}

/// Refs truth by task id.
pub type Truth = BTreeMap<String, Vec<Candidate>>;

/// The tasks of one fixture and the refs truth by task id.
pub fn parse_labels(lang: &str, text: &str) -> Result<(Vec<Task>, Truth), String> {
    let mut tasks = Vec::new();
    let mut refs: BTreeMap<&str, Vec<Candidate>> = BTreeMap::new();
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        match line.split_whitespace().collect::<Vec<_>>().as_slice() {
            ["symbol", path, qualified, kind, row] => {
                let name = qualified.rsplit(['.', ':']).next().unwrap_or(qualified);
                let line_no = row.parse().map_err(|e| format!("{line}: {e}"))?;
                if let Some(kind) = kind_of(kind) {
                    tasks.push(Task {
                        id: format!("{lang}/{qualified}@{path}"),
                        repo: lang.to_string(),
                        kind,
                        name: name.to_string(),
                        path: path.to_string(),
                        line: line_no,
                    });
                }
            }
            ["ref", name, path, row] => refs.entry(name).or_default().push(Candidate {
                path: path.to_string(),
                line: row.parse().map_err(|e| format!("{line}: {e}"))?,
            }),
            ["negative", ..] => {}
            _ => return Err(format!("bad label line in {lang}: {line}")),
        }
    }
    let truth = tasks
        .iter()
        .filter_map(|t| refs.get(t.name.as_str()).map(|r| (t.id.clone(), r.clone())))
        .collect();
    Ok((tasks, truth))
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(from).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let target = to.join(entry.file_name());
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn git(repo: &Path, args: &[&str]) -> Result<(), String> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .map_err(|e| e.to_string())?;
    status
        .success()
        .then_some(())
        .ok_or(format!("git {args:?}"))
}

/// Copies every fixture repo under `scratch` and builds the task set and one
/// job per language. The fixtures themselves are never touched.
pub fn jobs(scratch: &Path) -> Result<(TaskSet, Vec<RepoJob>), String> {
    let mut all = Vec::new();
    let mut jobs = Vec::new();
    for lang in LANGUAGES {
        let dir = fixtures_dir().join(lang);
        let text = std::fs::read_to_string(dir.join(LABELS_FILE)).map_err(|e| e.to_string())?;
        let (tasks, truth) = parse_labels(lang, &text)?;
        let repo = scratch.join(lang).join(REPO_DIR);
        copy_tree(&dir.join(REPO_DIR), &repo)?;
        git(&repo, &["init", "--quiet"])?;
        git(&repo, &["remote", "add", "origin", ORIGIN])?;
        all.extend(tasks.iter().cloned());
        jobs.push(RepoJob {
            repo: lang.to_string(),
            checkout: Ok(repo),
            tasks,
            truth,
        });
    }
    let set = TaskSet {
        version: TASKS_VERSION,
        seed: 0,
        source: SOURCE.to_string(),
        tasks: all,
    };
    Ok((set, jobs))
}

/// Runs the fixtures through `runner`.
pub fn measure(runner: &dyn CodeRunner, scratch: &Path) -> Result<Report, String> {
    let (set, jobs) = jobs(scratch)?;
    runner::run(runner, &set, TASKS_VERSION, &jobs).map_err(|e| e.to_string())
}

/// The failures of `report` against `baseline` with the production
/// tolerances and the fixture task minimum.
pub fn judge(baseline: &Baseline, report: &Report) -> Vec<Failure> {
    let ids = BTreeSet::new();
    evaluate(&Inputs {
        baseline,
        floor: &Floor { cells: Vec::new() },
        overrides: &Overrides {
            entries: Vec::new(),
        },
        report,
        run_date: RUN_DATE,
        prior_task_ids: &ids,
        task_ids: &ids,
        min_tasks_per_cell: FIXTURE_MIN_TASKS_PER_CELL,
    })
}

/// The `playbook` binary next to the running executable, built first when it
/// is missing.
pub fn playbook_program() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("executable has no directory")?;
    let find = || {
        [dir.join(PROGRAM), dir.join("..").join(PROGRAM)]
            .into_iter()
            .find(|p| p.is_file())
    };
    if let Some(found) = find() {
        return Ok(found);
    }
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let built = Command::new(cargo)
        .args(["build", "-p", PACKAGE])
        .status()
        .map_err(|e| e.to_string())?;
    built
        .success()
        .then(find)
        .flatten()
        .ok_or("no playbook binary".to_string())
}

/// The real binary with a store under `scratch`.
pub fn binary(scratch: &Path) -> Result<PlaybookBinary, String> {
    Ok(PlaybookBinary {
        program: playbook_program()?,
        home: scratch.join(HOME_DIR),
    })
}

/// The text of `gate-baseline.json` for the current index behavior.
pub fn baseline_text(report: &Report) -> Result<String, String> {
    let baseline = Baseline {
        tasks_version: report.tasks_version,
        waivers: Vec::new(),
        repos: report.repos.clone(),
    };
    let mut text = serde_json::to_string_pretty(&baseline).map_err(|e| e.to_string())?;
    text.push('\n');
    Ok(text)
}
