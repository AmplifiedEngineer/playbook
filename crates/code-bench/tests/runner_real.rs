// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Runs the real `playbook` binary on a copy of the committed Go fixture, in a
//! temp store of its own.

use code_bench::ctags::SymbolKind;
use code_bench::oracle::rg::Candidate;
use code_bench::runner::{run, PlaybookBinary, RepoJob, RepoStatus};
use code_bench::tasks::{Task, TaskSet, TASKS_VERSION};
use std::path::{Path, PathBuf};
use std::process::Command;

const FIXTURE: &str = "../code/tests/fixtures/go/repo";
const ORIGIN: &str = "https://github.com/test-owner/test-repo.git";
const TRUE_REFS: [(&str, u32); 5] = [
    ("shape.go", 11),
    ("shape.go", 16),
    ("shape.go", 20),
    ("main.go", 5),
    ("main.go", 15),
];

fn binary() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let found = exe.parent().unwrap().parent().unwrap().join("playbook");
    if !found.exists() {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let built = Command::new(cargo)
            .args(["build", "-p", "playbook"])
            .status();
        assert!(built.unwrap().success());
    }
    found
}

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git").arg("-C").arg(repo).args(args).status();
    assert!(status.unwrap().success());
}

#[test]
fn the_real_binary_reports_measured_values_on_the_go_fixture() {
    let scratch = std::env::temp_dir().join(format!("code-bench-runner-{}", std::process::id()));
    let repo = scratch.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    for entry in std::fs::read_dir(FIXTURE).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), repo.join(entry.file_name())).unwrap();
    }
    git(&repo, &["init", "--quiet"]);
    git(&repo, &["remote", "add", "origin", ORIGIN]);
    let task = Task {
        id: "fixture/struct/Square@shape.go".into(),
        repo: "fixture".into(),
        kind: SymbolKind::Struct,
        name: "Square".into(),
        path: "shape.go".into(),
        line: 11,
    };
    let truth = TRUE_REFS.map(|(path, line)| Candidate {
        path: path.into(),
        line,
    });
    let set = TaskSet {
        version: TASKS_VERSION,
        seed: 0,
        source: "fixture".into(),
        tasks: vec![task.clone()],
    };
    let job = RepoJob {
        repo: "fixture".into(),
        checkout: Ok(repo),
        truth: [(task.id.clone(), truth.to_vec())].into(),
        tasks: vec![task],
    };
    let runner = PlaybookBinary {
        program: binary(),
        home: scratch.join("home"),
    };
    let report = run(&runner, &set, TASKS_VERSION, &[job]).unwrap();
    let repo = &report.repos[0];
    assert_eq!(repo.status, RepoStatus::Measured, "{repo:?}");
    let [symbol, refs] = &repo.cells[..] else {
        panic!("two cells expected");
    };
    assert_eq!(
        (symbol.precision_bps, symbol.recall_bps),
        (Some(10_000), Some(10_000))
    );
    assert_eq!(
        (refs.precision_bps, refs.recall_bps),
        (Some(8333), Some(10_000))
    );
    assert!(refs.bytes > 0 && refs.tokens > 0);
    std::fs::remove_dir_all(scratch).unwrap();
}
