// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `code-bench pilot` runs the pilot repos through the real binary and writes
//! the JSON report. It needs network for the first clone and never downloads
//! an oracle: a repo without an answers file reports no refs truth.

use code_bench::corpus::{fetch, GitTransport, Manifest, ThreadSleeper, Tier};
use code_bench::oracle::rg::Candidate;
use code_bench::runner::{self, PlaybookBinary, RepoJob};
use code_bench::tasks::{TaskSet, TASKS_VERSION};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const DEFAULT_PROGRAM: &str = "playbook";
const ANSWERS_DIR: &str = "crates/code-bench/data/oracle-answers";
const HOME_DIR: &str = "home";
const USAGE: &str = "usage: code-bench pilot CACHE_DIR REPORT_FILE [PLAYBOOK_BINARY]";

struct Args {
    cache: PathBuf,
    out: PathBuf,
    program: PathBuf,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let [cache, out, rest @ ..] = args else {
        return Err(USAGE.to_string());
    };
    Ok(Args {
        cache: PathBuf::from(cache),
        out: PathBuf::from(out),
        program: PathBuf::from(rest.first().map_or(DEFAULT_PROGRAM, String::as_str)),
    })
}

/// The oracle answers of one repo. A missing file means no refs truth; a file
/// that does not parse is an error.
fn load_truth(dir: &Path, repo: &str) -> Result<BTreeMap<String, Vec<Candidate>>, String> {
    match std::fs::read_to_string(dir.join(format!("{repo}.json"))) {
        Ok(text) => runner::parse_answers(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e.to_string()),
    }
}

fn pilot(args: &Args) -> Result<(), String> {
    std::fs::create_dir_all(&args.cache).map_err(|e| e.to_string())?;
    let manifest = Manifest::bundled().map_err(|e| e.to_string())?;
    let tasks = TaskSet::bundled().map_err(|e| e.to_string())?;
    let mut jobs = Vec::new();
    for repo in manifest.tier(Tier::Pilot) {
        jobs.push(RepoJob {
            repo: repo.name.clone(),
            checkout: fetch(repo, &args.cache, &GitTransport, &ThreadSleeper)
                .map_err(|e| e.to_string()),
            tasks: tasks
                .tasks
                .iter()
                .filter(|t| t.repo == repo.name)
                .cloned()
                .collect(),
            truth: load_truth(Path::new(ANSWERS_DIR), &repo.name)?,
        });
    }
    let binary = PlaybookBinary {
        program: args.program.clone(),
        home: args.cache.join(HOME_DIR),
    };
    let report = runner::run(&binary, &tasks, TASKS_VERSION, &jobs).map_err(|e| e.to_string())?;
    let text = report.to_json().map_err(|e| e.to_string())?;
    std::fs::write(&args.out, text).map_err(|e| e.to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.split_first() {
        Some((command, rest)) if command == "pilot" => parse_args(rest).and_then(|a| pilot(&a)),
        _ => Err(USAGE.to_string()),
    };
    if let Err(message) = result {
        eprintln!("{message}");
        std::process::exit(1);
    }
}
