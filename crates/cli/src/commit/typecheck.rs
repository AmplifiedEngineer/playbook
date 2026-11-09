// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A guard on the conventional commit type, so the type does not rest on the
//! drafting model's effort alone. The Haiku 5.5 evaluation (#576) saw the
//! `git` agent at low effort write docs as `feat`, a refactor as `fix` and a
//! dependency bump as `fix`. The first and last are decidable from the files.
//!
//! Only clear mismatches are refused: a change that touches nothing but docs
//! is never `feat`, `fix`, `refactor` or `perf`, and so on. A mixed change is
//! never checked, and `--no-type-check` skips the guard.

use crate::common::re::static_regex;
use regex::Regex;
use std::sync::LazyLock;

static HEADER: LazyLock<Regex> =
    LazyLock::new(|| static_regex(r"^(?P<type>[a-zA-Z]+)(\([^)]*\))?!?:"));

/// What every changed file is, when they all are the same kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Docs,
    Tests,
    Ci,
    Deps,
}

fn is_docs(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let file = lower.rsplit('/').next().unwrap_or(&lower);
    lower.starts_with("docs/")
        || [".md", ".mdx", ".rst"].iter().any(|e| file.ends_with(e))
        || ["license", "notice", "changelog", "readme"].contains(&file)
}

fn is_test(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let file = lower.rsplit('/').next().unwrap_or(&lower);
    lower
        .split('/')
        .any(|c| c == "tests" || c == "test" || c == "__tests__")
        || file.ends_with("_test.rs")
        || file.contains(".test.")
        || file.contains(".spec.")
}

fn is_ci(path: &str) -> bool {
    path.starts_with(".github/") || path == ".gitlab-ci.yml" || path.starts_with(".circleci/")
}

const LOCKS: [&str; 7] = [
    "Cargo.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "go.sum",
    "poetry.lock",
    "Gemfile.lock",
];
const MANIFESTS: [&str; 5] = [
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "Gemfile",
];

/// Whether `files` are only lock files, or manifests next to a lock file.
fn is_deps(files: &[&str]) -> bool {
    let base = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
    let has_lock = files.iter().any(|f| LOCKS.contains(&base(f).as_str()));
    has_lock
        && files.iter().all(|f| {
            let b = base(f);
            LOCKS.contains(&b.as_str()) || MANIFESTS.contains(&b.as_str())
        })
}

pub(super) fn kind_of(files: &[&str]) -> Option<Kind> {
    if files.is_empty() {
        return None;
    }
    if files.iter().all(|f| is_docs(f)) {
        Some(Kind::Docs)
    } else if files.iter().all(|f| is_test(f)) {
        Some(Kind::Tests)
    } else if files.iter().all(|f| is_ci(f)) {
        Some(Kind::Ci)
    } else if is_deps(files) {
        Some(Kind::Deps)
    } else {
        None
    }
}

/// Refuse a commit whose type contradicts what the files are. `message` is
/// the full commit message, `files` the staged paths.
pub fn check(message: &str, files: &[&str]) -> Result<(), String> {
    let Some(first) = message.lines().find(|l| !l.trim().is_empty()) else {
        return Ok(());
    };
    let Some(caps) = HEADER.captures(first.trim()) else {
        return Ok(());
    };
    let ty = caps["type"].to_ascii_lowercase();
    let (kind, bad, want): (Kind, &[&str], &str) = match kind_of(files) {
        Some(Kind::Docs) => (Kind::Docs, &["feat", "fix", "refactor", "perf"], "docs"),
        Some(Kind::Tests) => (Kind::Tests, &["feat", "refactor", "perf", "docs"], "test"),
        Some(Kind::Ci) => (Kind::Ci, &["feat", "docs"], "ci"),
        Some(Kind::Deps) => (
            Kind::Deps,
            &["feat", "fix", "refactor", "docs"],
            "build or chore",
        ),
        None => return Ok(()),
    };
    if !bad.contains(&ty.as_str()) {
        return Ok(());
    }
    let what = match kind {
        Kind::Docs => "only documentation files",
        Kind::Tests => "only test files",
        Kind::Ci => "only CI files",
        Kind::Deps => "only dependency manifests and lock files",
    };
    Err(format!(
        "commit type check: the staged change touches {what}, so the type '{ty}' is wrong, use '{want}'. Change the first line and run again, or pass --no-type-check if this is intended."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_docs_only_change_is_not_a_feature() {
        let err = check(
            "feat(docs): explain config\n\nbody",
            &["docs/guide.md", "README.md"],
        )
        .unwrap_err();
        assert!(err.contains("use 'docs'"), "{err}");
        assert!(check("docs: explain config", &["docs/guide.md"]).is_ok());
    }

    #[test]
    fn a_dependency_bump_is_not_a_fix() {
        let err = check("fix(deps): bump serde", &["Cargo.toml", "Cargo.lock"]).unwrap_err();
        assert!(err.contains("build or chore"), "{err}");
        assert!(check("chore(deps): bump serde", &["Cargo.lock"]).is_ok());
        assert!(check("build(deps): bump serde", &["Cargo.toml", "Cargo.lock"]).is_ok());
    }

    #[test]
    fn a_manifest_alone_is_not_a_dependency_bump() {
        // Cargo.toml can carry a real feature change, so only with a lock file.
        assert!(check("feat: add a feature flag", &["Cargo.toml"]).is_ok());
    }

    #[test]
    fn a_tests_only_change_is_not_a_feature() {
        assert!(check("feat: cover the sweep", &["tests/sweep.rs"]).is_err());
        assert!(check("test: cover the sweep", &["tests/sweep.rs"]).is_ok());
        assert!(check("fix(tests): a flaky test", &["tests/sweep.rs"]).is_ok());
    }

    #[test]
    fn a_ci_only_change_is_not_a_feature() {
        assert!(check("feat: cache builds", &[".github/workflows/rust-ci.yml"]).is_err());
        assert!(check("ci: cache builds", &[".github/workflows/rust-ci.yml"]).is_ok());
    }

    #[test]
    fn a_mixed_change_is_never_checked() {
        assert!(check("feat: a thing", &["src/a.rs", "docs/a.md"]).is_ok());
        assert!(check("fix: a thing", &["Cargo.lock", "src/a.rs"]).is_ok());
    }

    #[test]
    fn a_message_without_a_conventional_header_is_left_to_commitlint() {
        assert!(check("update stuff", &["docs/a.md"]).is_ok());
        assert!(check("", &["docs/a.md"]).is_ok());
    }

    #[test]
    fn an_empty_file_list_is_never_checked() {
        assert!(check("feat: x", &[]).is_ok());
    }
}
