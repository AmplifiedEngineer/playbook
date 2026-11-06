// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The SCIP oracle adapter. It calls an installed indexer through the injected
//! process runner, never downloads one, and caches the printed index by repo,
//! commit and the pinned tool version. A missing tool or another version
//! returns a skip with a reason. `truth_for` reads the references of one
//! definition, and `partition` splits `rg` candidates into positives and
//! negatives.

use crate::corpus::Repo;
use crate::oracle::rg::Candidate;
use crate::process::{ProcessRunner, RunError};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

const PRINTER: &str = "scip";
const INDEX_FILE: &str = "index.scip";
/// SCIP `SymbolRole.Definition` bit.
const DEFINITION_ROLE: u32 = 1;

#[derive(Debug, PartialEq, Eq)]
pub enum Indexed {
    /// The index printed as JSON.
    Json(String),
    Skipped {
        reason: String,
    },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ScipError {
    #[error("language {0} has no scip indexer")]
    Language(String),
    #[error("tool {0} is not pinned in the manifest")]
    Unpinned(String),
    #[error("scip run failed: {0}")]
    Run(String),
    #[error("scip index does not parse: {0}")]
    Parse(String),
    #[error("cache io error: {0}")]
    Io(String),
}

/// Candidates split by the oracle. Both lists are sorted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub positives: Vec<Candidate>,
    pub negatives: Vec<Candidate>,
}

#[derive(Deserialize)]
struct Index {
    documents: Vec<Document>,
}

#[derive(Deserialize)]
struct Document {
    relative_path: String,
    occurrences: Vec<Occurrence>,
}

#[derive(Deserialize)]
struct Occurrence {
    range: Vec<u32>,
    symbol: String,
    #[serde(default)]
    symbol_roles: u32,
}

/// The tool name and its arguments for `repo`'s language.
fn indexer(language: &str, project: &str, out: &str) -> Option<(&'static str, Vec<String>)> {
    let args = |parts: &[&str]| parts.iter().map(|p| p.to_string()).collect();
    Some(match language {
        "rust" => ("rust-analyzer", args(&["scip", ".", "--output", out])),
        "go" => ("scip-go", args(&["--output", out])),
        "python" => (
            "scip-python",
            args(&["index", ".", "--project-name", project, "--output", out]),
        ),
        "typescript" | "javascript" => ("scip-typescript", args(&["index", "--output", out])),
        _ => return None,
    })
}

fn skip(reason: String) -> Result<Indexed, ScipError> {
    Ok(Indexed::Skipped { reason })
}

fn failed(e: RunError) -> ScipError {
    ScipError::Run(format!("{e:?}"))
}

/// Indexes `checkout` and returns the printed index. `pinned` maps tool names
/// to their manifest versions.
pub fn index(
    runner: &dyn ProcessRunner,
    pinned: &std::collections::BTreeMap<String, String>,
    repo: &Repo,
    checkout: &Path,
    cache: &Path,
) -> Result<Indexed, ScipError> {
    let out_path = cache.join(format!("{}-{}-{INDEX_FILE}", repo.name, repo.commit));
    let out = out_path.to_string_lossy().into_owned();
    let (tool, args) = indexer(&repo.language, &repo.name, &out)
        .ok_or_else(|| ScipError::Language(repo.language.clone()))?;
    let version = pinned
        .get(tool)
        .ok_or_else(|| ScipError::Unpinned(tool.to_string()))?;
    let cached = cache.join(format!(
        "{}-{}-{tool}-{version}.json",
        repo.name, repo.commit
    ));
    if let Ok(text) = fs::read_to_string(&cached) {
        return Ok(Indexed::Json(text));
    }
    match runner.run(tool, &["--version".to_string()], checkout) {
        Ok(banner) if banner.contains(version.as_str()) => {}
        Ok(_) => return skip(format!("{tool} is installed and {version} is pinned")),
        Err(RunError::NotFound) => return skip(format!("{tool} is not installed")),
        Err(e) => return Err(failed(e)),
    }
    fs::create_dir_all(cache).map_err(|e| ScipError::Io(e.to_string()))?;
    runner.run(tool, &args, checkout).map_err(failed)?;
    let print = ["print".to_string(), "--json".to_string(), out];
    let text = match runner.run(PRINTER, &print, checkout) {
        Ok(text) => text,
        Err(RunError::NotFound) => return skip(format!("{PRINTER} is not installed")),
        Err(e) => return Err(failed(e)),
    };
    fs::write(&cached, &text).map_err(|e| ScipError::Io(e.to_string()))?;
    Ok(Indexed::Json(text))
}

/// Every occurrence of the symbol defined at `path:line` (1-based), sorted.
pub fn truth_for(json: &str, path: &str, line: u32) -> Result<Vec<Candidate>, ScipError> {
    let index: Index = serde_json::from_str(json).map_err(|e| ScipError::Parse(e.to_string()))?;
    let start = |o: &Occurrence| o.range.first().map(|l| l + 1);
    let symbol = index
        .documents
        .iter()
        .filter(|d| d.relative_path == path)
        .flat_map(|d| &d.occurrences)
        .find(|o| o.symbol_roles & DEFINITION_ROLE != 0 && start(o) == Some(line))
        .map(|o| o.symbol.as_str());
    let mut truth = BTreeSet::new();
    if let Some(symbol) = symbol {
        for doc in &index.documents {
            for occ in doc.occurrences.iter().filter(|o| o.symbol == symbol) {
                if let Some(line) = start(occ) {
                    truth.insert(Candidate {
                        path: doc.relative_path.clone(),
                        line,
                    });
                }
            }
        }
    }
    Ok(truth.into_iter().collect())
}

/// Splits candidates into those the oracle accepts and those it rejects.
pub fn partition(candidates: &[Candidate], truth: &[Candidate]) -> Answer {
    let accepted: BTreeSet<&Candidate> = truth.iter().collect();
    let (mut positives, mut negatives) = (Vec::new(), Vec::new());
    for c in candidates {
        if accepted.contains(c) {
            positives.push(c.clone());
        } else {
            negatives.push(c.clone());
        }
    }
    positives.sort();
    negatives.sort();
    Answer {
        positives,
        negatives,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::Manifest;
    use playbook_common::test_support::scratch_dir;
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    const VERSION: &str = "0.1.26";
    const INDEX: &str = r#"{"documents":[
      {"relative_path":"a.go","occurrences":[
        {"range":[2,5,8],"symbol":"pkg/Foo#","symbol_roles":1},
        {"range":[9,1,4],"symbol":"pkg/Foo#"},
        {"range":[11,1,4],"symbol":"pkg/Other#"}]},
      {"relative_path":"b.go","occurrences":[{"range":[0,0,3],"symbol":"pkg/Foo#"}]}]}"#;

    struct Stub {
        banner: Result<String, RunError>,
        calls: RefCell<Vec<String>>,
    }

    impl ProcessRunner for Stub {
        fn run(&self, program: &str, args: &[String], _: &Path) -> Result<String, RunError> {
            self.calls.borrow_mut().push(program.to_string());
            match args.first().map(String::as_str) {
                Some("--version") => self.banner.clone(),
                Some("print") => Ok(INDEX.to_string()),
                _ => Ok(String::new()),
            }
        }
    }

    fn stub(banner: Result<String, RunError>) -> Stub {
        Stub {
            banner,
            calls: RefCell::default(),
        }
    }

    fn pins(version: &str) -> BTreeMap<String, String> {
        BTreeMap::from([("scip-go".to_string(), version.to_string())])
    }

    fn go_repo() -> Repo {
        let manifest = Manifest::bundled().unwrap();
        manifest
            .repos
            .into_iter()
            .find(|r| r.language == "go")
            .unwrap()
    }

    fn run(stub: &Stub, version: &str, cache: &Path) -> Result<Indexed, ScipError> {
        index(stub, &pins(version), &go_repo(), &PathBuf::from("."), cache)
    }

    fn cand(path: &str, line: u32) -> Candidate {
        Candidate {
            path: path.into(),
            line,
        }
    }

    #[test]
    fn oracle_rejecting_a_candidate_makes_it_a_negative() {
        let truth = truth_for(INDEX, "a.go", 3).unwrap();
        assert_eq!(truth, [cand("a.go", 3), cand("a.go", 10), cand("b.go", 1)]);
        let found = [cand("b.go", 1), cand("a.go", 3), cand("c.go", 4)];
        let answer = partition(&found, &truth);
        assert_eq!(answer.positives, [cand("a.go", 3), cand("b.go", 1)]);
        assert_eq!(answer.negatives, [cand("c.go", 4)]);
        assert!(truth_for(INDEX, "a.go", 99).unwrap().is_empty());
        assert!(matches!(
            truth_for("x", "a.go", 1),
            Err(ScipError::Parse(_))
        ));
    }

    #[test]
    fn missing_tool_and_other_version_skip_with_a_reason() {
        let cache = scratch_dir("scip-skip");
        let missing = stub(Err(RunError::NotFound));
        let got = run(&missing, VERSION, &cache).unwrap();
        assert_eq!(
            got,
            Indexed::Skipped {
                reason: "scip-go is not installed".into()
            }
        );
        let other = stub(Ok("scip-go 0.1.20".into()));
        assert!(matches!(
            run(&other, VERSION, &cache).unwrap(),
            Indexed::Skipped { .. }
        ));
    }

    #[test]
    fn cache_hit_runs_no_tool_and_other_version_misses() {
        let cache = scratch_dir("scip-cache");
        let first = stub(Ok(format!("scip-go {VERSION}")));
        assert_eq!(
            run(&first, VERSION, &cache).unwrap(),
            Indexed::Json(INDEX.into())
        );
        let second = stub(Err(RunError::NotFound));
        assert!(matches!(
            run(&second, VERSION, &cache).unwrap(),
            Indexed::Json(_)
        ));
        assert!(second.calls.borrow().is_empty());
        assert!(matches!(
            run(&second, "0.2.0", &cache).unwrap(),
            Indexed::Skipped { .. }
        ));
    }

    #[test]
    fn unknown_language_and_unpinned_tool_fail() {
        let cache = scratch_dir("scip-bad");
        let s = stub(Ok(String::new()));
        let mut repo = go_repo();
        let none = BTreeMap::new();
        let err = index(&s, &none, &repo, &PathBuf::from("."), &cache).unwrap_err();
        assert_eq!(err, ScipError::Unpinned("scip-go".into()));
        repo.language = "cobol".into();
        let err = index(&s, &pins(VERSION), &repo, &PathBuf::from("."), &cache).unwrap_err();
        assert_eq!(err, ScipError::Language("cobol".into()));
    }
}
