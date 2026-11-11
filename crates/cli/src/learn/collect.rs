// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook learn collect git|structure`: the two Phase 1 collectors of
//! `/playbook:learn` that were Haiku agents running fixed git and
//! listing commands and summarising the output. The numbers here are counted,
//! not summarised, and the repo is read only through `git`, so nothing
//! outside version control and nothing ignored is looked at.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

/// How many commits the churn count scans.
const CHURN_COMMITS: usize = 2000;
/// How many recent subjects the commit style is read from.
const SUBJECTS: usize = 300;

/// Longest one history scan may run. A huge repo that needs longer yields an
/// empty scan instead of a command that never returns.
const SCAN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// The scan limit; `LEARN_SCAN_TIMEOUT_SECONDS` overrides it.
fn scan_timeout() -> std::time::Duration {
    std::env::var("LEARN_SCAN_TIMEOUT_SECONDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map_or(SCAN_TIMEOUT, std::time::Duration::from_secs)
}

/// Stdout of a scan, or `None` when git could not start or ran out of time.
fn git_opt(dir: &Path, args: &[&str]) -> Option<String> {
    let mut command = crate::common::git::command(dir);
    command.args(args);
    crate::common::proc::run_with_timeout(&mut command, scan_timeout())
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
}

fn git(dir: &Path, args: &[&str]) -> String {
    git_opt(dir, args).unwrap_or_default()
}

/// The `limit` highest counts, ties broken by name so the output is stable.
fn top(counts: BTreeMap<String, usize>, limit: usize) -> Vec<(String, usize)> {
    let mut rows: Vec<(String, usize)> = counts.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    rows.truncate(limit);
    rows
}

fn tally<'a>(items: impl Iterator<Item = &'a str>) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for item in items.filter(|i| !i.is_empty()) {
        *counts.entry(item.to_string()).or_default() += 1;
    }
    counts
}

fn rows(list: Vec<(String, usize)>, key: &str) -> Value {
    Value::Array(
        list.into_iter()
            .map(|(name, n)| json!({ key: name, "count": n }))
            .collect(),
    )
}

/// The conventional commit type of a subject (`feat(x)!: y` is `feat`).
pub fn commit_type(subject: &str) -> Option<&str> {
    let (head, _) = subject.split_once(':')?;
    let head = head.trim_end_matches('!');
    let kind = head.split('(').next()?;
    let ok = !kind.is_empty() && kind.chars().all(|c| c.is_ascii_lowercase());
    ok.then_some(kind)
}

/// History facts: who commits, where the churn is, how commits are named,
/// the recent tags and the monthly cadence.
pub fn git_history(dir: &Path) -> Value {
    // A scan that times out reads as empty history, so each one is named in
    // `scans_timed_out` and an empty answer is never mistaken for a real one.
    let timed_out = std::cell::RefCell::new(Vec::<String>::new());
    let scan = |args: &[&str]| -> String {
        git_opt(dir, args).unwrap_or_else(|| {
            timed_out.borrow_mut().push(args.join(" "));
            String::new()
        })
    };
    let total = scan(&["rev-list", "--count", "HEAD"]).trim().to_string();
    let authors = scan(&["log", "--format=%an"]);
    let contributors = top(tally(authors.lines()), 10);
    let names = scan(&[
        "log",
        &format!("-{CHURN_COMMITS}"),
        "--format=",
        "--name-only",
    ]);
    let churn = top(tally(names.lines()), 15);
    let subjects = scan(&["log", &format!("-{SUBJECTS}"), "--format=%s"]);
    let subject_count = subjects.lines().filter(|l| !l.is_empty()).count();
    let types = tally(subjects.lines().filter_map(commit_type));
    let conventional: usize = types.values().sum();
    let branches = scan(&["branch", "-r", "--format=%(refname:short)"]);
    let prefixes = tally(
        branches
            .lines()
            .filter_map(|b| b.split_once('/').map(|(_, rest)| rest))
            .filter_map(|rest| rest.split_once('/').map(|(p, _)| p)),
    );
    let tags = scan(&[
        "for-each-ref",
        "--sort=-creatordate",
        "--count=10",
        "--format=%(refname:short) %(creatordate:short)",
        "refs/tags",
    ]);
    let months = scan(&["log", "-3000", "--format=%ad", "--date=format:%Y-%m"]);
    let mut cadence: Vec<(String, usize)> = tally(months.lines()).into_iter().collect();
    cadence.sort_by(|a, b| b.0.cmp(&a.0));
    cadence.truncate(6);
    json!({
        "role": "git-history",
        "commits_total": total.parse::<u64>().unwrap_or(0),
        "contributors": rows(contributors, "name"),
        "churn_hotspots": rows(churn, "path"),
        "churn_commits_scanned": CHURN_COMMITS,
        "commit_subjects_read": subject_count,
        "commit_types": rows(top(types, 10), "type"),
        "conventional_commit_share": if subject_count == 0 { 0.0 } else { conventional as f64 / subject_count as f64 },
        "remote_branch_prefixes": rows(top(prefixes, 8), "prefix"),
        "recent_tags": tags.lines().map(str::to_string).collect::<Vec<_>>(),
        "commits_per_month_latest_first": rows(cadence, "month"),
        "scans_timed_out": timed_out.into_inner(),
    })
}

fn extension(path: &str) -> Option<&str> {
    let name = path.rsplit('/').next()?;
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty() && ext.len() <= 8).then_some(ext)
}

const LOCKFILES: [&str; 8] = [
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "Cargo.lock",
    "go.sum",
    "poetry.lock",
    "Gemfile.lock",
    "composer.lock",
];

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn named(files: &[String], pred: impl Fn(&str) -> bool) -> Vec<&str> {
    files
        .iter()
        .map(String::as_str)
        .filter(|f| pred(f))
        .collect()
}

fn cap(mut v: Vec<&str>, n: usize) -> Vec<String> {
    v.truncate(n);
    v.into_iter().map(str::to_string).collect()
}

/// `make` targets: `name:` at the start of a line, not `.PHONY` or a variable.
pub fn make_targets(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let Some((head, rest)) = line.split_once(':') else {
            continue;
        };
        let ok = !head.is_empty()
            && !head.starts_with(['.', '\t', ' ', '#'])
            && head
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            && !rest.starts_with('=');
        if ok && !out.iter().any(|t| t == head) {
            out.push(head.to_string());
        }
    }
    out
}

/// `just` recipe names: a name at the start of a line followed by `:`.
pub fn just_recipes(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        if line.starts_with([' ', '\t', '#']) || line.contains(":=") {
            continue;
        }
        let Some((head, _)) = line.split_once(':') else {
            continue;
        };
        let name = head.split_whitespace().next().unwrap_or("");
        if !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
            && !out.iter().any(|t: &String| t == name)
        {
            out.push(name.to_string());
        }
    }
    out
}

/// The script names in a `package.json`.
pub fn package_scripts(text: &str) -> Vec<String> {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v.get("scripts").and_then(Value::as_object).cloned())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// Layout facts from the tracked file list; `read` returns a tracked file's text.
pub fn structure_from(files: &[String], read: &dyn Fn(&str) -> Option<String>) -> Value {
    let mut top_level: Vec<String> = files
        .iter()
        .map(|f| match f.split_once('/') {
            Some((dir, _)) => format!("{dir}/"),
            None => f.clone(),
        })
        .collect();
    top_level.sort();
    top_level.dedup();
    let languages = top(
        tally(
            files
                .iter()
                .filter(|f| !LOCKFILES.contains(&file_name(f)))
                .filter_map(|f| extension(f)),
        ),
        8,
    );
    let entry = |p: &str| {
        let n = file_name(p);
        let stem = n.split('.').next().unwrap_or("");
        matches!(
            stem,
            "main" | "__main__" | "index" | "app" | "server" | "cli" | "manage"
        ) && !p.contains("/test")
            && !p.contains("node_modules")
            && !n.contains(".test.")
            && !n.contains(".spec.")
            && !n.contains("_test.")
            && !n.ends_with(".tf")
    };
    let manifests = [
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "go.mod",
        "Makefile",
        "justfile",
        "build.gradle",
        "pom.xml",
        "Gemfile",
        "composer.json",
    ];
    let mut build = Map::new();
    for name in manifests {
        let found = named(files, |f| file_name(f) == name);
        if found.is_empty() {
            continue;
        }
        let mut entry = Map::new();
        entry.insert("paths".into(), json!(cap(found.clone(), 6)));
        let text = read(found[0]).unwrap_or_default();
        match name {
            "package.json" => {
                entry.insert("scripts".into(), json!(package_scripts(&text)));
            }
            "Makefile" => {
                entry.insert("targets".into(), json!(make_targets(&text)));
            }
            "justfile" => {
                entry.insert("recipes".into(), json!(just_recipes(&text)));
            }
            _ => {}
        }
        build.insert(name.to_string(), Value::Object(entry));
    }
    let tooling = |p: &str| {
        let n = file_name(p);
        n.starts_with("jest.config")
            || n.starts_with("vitest.config")
            || n.starts_with(".eslintrc")
            || n.starts_with("eslint.config")
            || n.starts_with(".prettierrc")
            || n.starts_with(".golangci")
            || matches!(
                n,
                "pytest.ini"
                    | "tox.ini"
                    | "ruff.toml"
                    | ".ruff.toml"
                    | "rustfmt.toml"
                    | "clippy.toml"
                    | "rust-toolchain.toml"
                    | "tsconfig.json"
                    | "mypy.ini"
            )
    };
    let ci = |p: &str| {
        p.starts_with(".github/workflows/")
            || p.starts_with(".circleci/")
            || matches!(
                file_name(p),
                ".gitlab-ci.yml" | "Jenkinsfile" | "azure-pipelines.yml"
            )
    };
    let iac = |p: &str| {
        let n = file_name(p);
        n.ends_with(".tf")
            || matches!(
                n,
                "Pulumi.yaml" | "cdk.json" | "serverless.yml" | "Chart.yaml" | "kustomization.yaml"
            )
    };
    let migrations = |p: &str| {
        ["migrations/", "migrate/", "alembic/", "prisma/"]
            .iter()
            .any(|d| p.contains(d))
    };
    let models = |p: &str| {
        let n = file_name(p);
        n == "models.py"
            || n == "schema.prisma"
            || p.contains("/models/")
            || p.contains("/entities/")
    };
    json!({
        "role": "code-structure",
        "tracked_files": files.len(),
        "top_level": top_level,
        "languages_by_file_count": rows(languages, "extension"),
        "entry_points": cap(named(files, entry), 12),
        "build_manifests": Value::Object(build),
        "test_lint_config": cap(named(files, tooling), 20),
        "docker": cap(named(files, |p| file_name(p).starts_with("Dockerfile") || file_name(p).starts_with("docker-compose")), 10),
        "ci_cd": cap(named(files, ci), 20),
        "iac": cap(named(files, iac), 20),
        "migrations": cap(named(files, migrations), 12),
        "orm_models": cap(named(files, models), 12),
        "scripts": cap(named(files, |p| p.starts_with("scripts/")), 20),
    })
}

/// Layout facts for the repo containing `dir`.
pub fn code_structure(dir: &Path) -> Value {
    let list = git(dir, &["ls-files"]);
    let files: Vec<String> = list.lines().map(str::to_string).collect();
    structure_from(&files, &|path| std::fs::read_to_string(dir.join(path)).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_types_read_conventional_subjects_only() {
        assert_eq!(commit_type("feat(x)!: add y"), Some("feat"));
        assert_eq!(commit_type("fix: a"), Some("fix"));
        assert_eq!(commit_type("Merge branch 'x'"), None);
        assert_eq!(commit_type("WIP: stuff"), None);
        assert_eq!(commit_type("no colon here"), None);
        assert_eq!(commit_type(": empty"), None);
    }

    #[test]
    fn counts_are_ordered_by_count_then_name_and_capped() {
        let counts = tally(["b", "a", "a", "c", "c", ""].into_iter());
        let rows = top(counts, 2);
        assert_eq!(rows, vec![("a".to_string(), 2), ("c".to_string(), 2)]);
    }

    #[test]
    fn make_targets_skip_phony_variables_and_recipes() {
        let text =
            ".PHONY: all\nCC := gcc\nall: build\n\techo hi\nbuild: src\ntest-unit:\n# comment: x\n";
        assert_eq!(make_targets(text), ["all", "build", "test-unit"]);
    }

    #[test]
    fn just_recipes_skip_assignments_and_indented_lines() {
        let text = "set shell := [\"sh\"]\nbin := \"x\"\n# note: y\nbuild flags='':\n  cargo build\ntest:\n  cargo test\n";
        assert_eq!(just_recipes(text), ["build", "test"]);
    }

    #[test]
    fn package_scripts_come_from_the_scripts_object() {
        assert_eq!(
            package_scripts(r#"{"scripts":{"build":"x","test":"y"}}"#),
            ["build", "test"]
        );
        assert!(package_scripts("{not json").is_empty());
        assert!(package_scripts("{}").is_empty());
    }

    fn files(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn structure_reports_layout_tooling_and_build_scripts() {
        let list = files(&[
            "Cargo.toml",
            "Cargo.lock",
            "Makefile",
            "package.json",
            "src/main.rs",
            "src/lib.rs",
            "src/models/user.rs",
            "web/index.ts",
            "web/app.test.ts",
            ".github/workflows/ci.yml",
            "infra/main.tf",
            "db/migrations/001.sql",
            "scripts/setup.sh",
            "Dockerfile",
            "tsconfig.json",
        ]);
        let read = |p: &str| match p {
            "Makefile" => Some("build:\n\tcargo build\ntest:\n".to_string()),
            "package.json" => Some(r#"{"scripts":{"lint":"eslint ."}}"#.to_string()),
            _ => None,
        };
        let v = structure_from(&list, &read);
        assert_eq!(v["tracked_files"], 15);
        assert!(v["top_level"].as_array().unwrap().contains(&json!("src/")));
        assert!(v["top_level"]
            .as_array()
            .unwrap()
            .contains(&json!("Cargo.toml")));
        assert_eq!(
            v["build_manifests"]["Makefile"]["targets"],
            json!(["build", "test"])
        );
        assert_eq!(
            v["build_manifests"]["package.json"]["scripts"],
            json!(["lint"])
        );
        assert_eq!(v["entry_points"], json!(["src/main.rs", "web/index.ts"]));
        assert_eq!(v["ci_cd"], json!([".github/workflows/ci.yml"]));
        assert_eq!(v["iac"], json!(["infra/main.tf"]));
        assert_eq!(v["migrations"], json!(["db/migrations/001.sql"]));
        assert_eq!(v["docker"], json!(["Dockerfile"]));
        assert_eq!(v["scripts"], json!(["scripts/setup.sh"]));
        assert!(v["orm_models"]
            .as_array()
            .unwrap()
            .contains(&json!("src/models/user.rs")));
        // Lockfiles do not count as a language.
        let langs = v["languages_by_file_count"].as_array().unwrap();
        assert!(langs.iter().all(|r| r["extension"] != "lock"));
    }

    #[test]
    fn an_empty_repo_still_yields_a_well_formed_object() {
        let v = structure_from(&[], &|_| None);
        assert_eq!(v["tracked_files"], 0);
        assert_eq!(v["top_level"], json!([]));
        assert_eq!(v["build_manifests"], json!({}));
    }
}
