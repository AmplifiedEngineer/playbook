// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `rg` adapter. `candidates` runs `rg -w` for one name and returns the
//! sorted match set that the oracles later accept or reject. `baseline` is the
//! tuned comparison arm: it ranks files by match count and reads the top few.
//! The pinned version comes from an injected function, and a mismatch fails.

use crate::metrics::{bytes, tokens};
use crate::process::{ProcessRunner, RunError};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const PROGRAM: &str = "rg";

/// Files the baseline reads per query, best ranked first.
pub const MAX_BASELINE_FILES: usize = 5;

const EXCLUDED_GLOBS: [&str; 6] = [
    "!.git",
    "!node_modules",
    "!vendor",
    "!target",
    "!dist",
    "!build",
];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Candidate {
    pub path: String,
    pub line: u32,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RgError {
    #[error("rg skipped: {0}")]
    Skipped(String),
    #[error("rg {found} is installed and {pinned} is pinned")]
    VersionMismatch { found: String, pinned: String },
    #[error("rg failed: {0}")]
    Run(String),
    #[error("rg output line is not valid: {0}")]
    Parse(String),
    #[error("io error: {0}")]
    Io(String),
}

/// The tuned baseline for one query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Baseline {
    /// Files read, best ranked first.
    pub files: Vec<String>,
    pub bytes: u64,
    pub tokens: u64,
}

/// Reads the installed version, such as `15.2.0`.
pub type VersionFn<'a> = &'a dyn Fn() -> Result<String, RunError>;

fn check_version(installed: VersionFn<'_>, pinned: &str) -> Result<(), RgError> {
    match installed() {
        Ok(found) if found == pinned => Ok(()),
        Ok(found) => Err(RgError::VersionMismatch {
            found,
            pinned: pinned.to_string(),
        }),
        Err(RunError::NotFound) => Err(RgError::Skipped(format!("{PROGRAM} is not installed"))),
        Err(RunError::Failed(e)) => Err(RgError::Run(e)),
    }
}

/// Every whole-word match of `name`, sorted by path and line.
pub fn candidates(
    runner: &dyn ProcessRunner,
    installed: VersionFn<'_>,
    pinned: &str,
    checkout: &Path,
    name: &str,
) -> Result<Vec<Candidate>, RgError> {
    check_version(installed, pinned)?;
    let mut args: Vec<String> = [
        "-w",
        "-F",
        "-n",
        "--no-heading",
        "--color=never",
        "--sort=path",
    ]
    .map(String::from)
    .into();
    for glob in EXCLUDED_GLOBS {
        args.extend(["-g".to_string(), glob.to_string()]);
    }
    args.extend(["--".to_string(), name.to_string(), ".".to_string()]);
    let out = match runner.run(PROGRAM, &args, checkout) {
        Ok(out) => out,
        Err(RunError::NotFound) => {
            return Err(RgError::Skipped(format!("{PROGRAM} is not installed")))
        }
        Err(RunError::Failed(e)) => return Err(RgError::Run(e)),
    };
    let mut found = Vec::new();
    for row in out.lines().filter(|l| !l.is_empty()) {
        let mut parts = row.splitn(3, ':');
        let (Some(path), Some(line), Some(_)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(RgError::Parse(row.to_string()));
        };
        let line: u32 = line.parse().map_err(|_| RgError::Parse(row.to_string()))?;
        let path = path.strip_prefix("./").unwrap_or(path);
        found.push(Candidate {
            path: path.to_string(),
            line,
        });
    }
    found.sort();
    Ok(found)
}

/// Ranks files by match count (ties by path) and reads the top
/// `MAX_BASELINE_FILES`. Bytes and tokens cover the files read.
pub fn baseline(
    runner: &dyn ProcessRunner,
    installed: VersionFn<'_>,
    pinned: &str,
    checkout: &Path,
    name: &str,
) -> Result<Baseline, RgError> {
    let matches = candidates(runner, installed, pinned, checkout, name)?;
    let mut per_file: BTreeMap<&str, usize> = BTreeMap::new();
    for m in &matches {
        *per_file.entry(m.path.as_str()).or_default() += 1;
    }
    let mut ranked: Vec<(&str, usize)> = per_file.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    ranked.truncate(MAX_BASELINE_FILES);
    let mut result = Baseline {
        files: Vec::new(),
        bytes: 0,
        tokens: 0,
    };
    for (path, _) in ranked {
        let text =
            fs::read_to_string(checkout.join(path)).map_err(|e| RgError::Io(e.to_string()))?;
        result.bytes += bytes(&text);
        result.tokens += tokens(&text);
        result.files.push(path.to_string());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use playbook_common::test_support::scratch_dir;
    use std::cell::RefCell;

    const PINNED: &str = "15.2.0";

    struct Stub {
        out: Result<String, RunError>,
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl Stub {
        fn new(out: &str) -> Self {
            Self {
                out: Ok(out.to_string()),
                calls: RefCell::default(),
            }
        }
    }

    impl ProcessRunner for Stub {
        fn run(&self, _: &str, args: &[String], _: &Path) -> Result<String, RunError> {
            self.calls.borrow_mut().push(args.to_vec());
            self.out.clone()
        }
    }

    fn pinned_version() -> Result<String, RunError> {
        Ok(PINNED.to_string())
    }

    #[test]
    fn candidates_are_sorted_and_use_word_match() {
        let stub = Stub::new("./b.go:7:x Foo\n./a.go:12:Foo()\n./a.go:3:Foo\n");
        let dir = scratch_dir("rg-cands");
        let found = candidates(&stub, &pinned_version, PINNED, &dir, "Foo").unwrap();
        let rows: Vec<_> = found.iter().map(|c| (c.path.as_str(), c.line)).collect();
        assert_eq!(rows, [("a.go", 3), ("a.go", 12), ("b.go", 7)]);
        let args = stub.calls.borrow()[0].clone();
        assert!(args.contains(&"-w".to_string()) && args.contains(&"--sort=path".to_string()));
    }

    #[test]
    fn baseline_matches_recorded_files_and_bytes() {
        let dir = scratch_dir("rg-baseline");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.go"), "func Foo() {}\n").unwrap();
        fs::write(dir.join("b.go"), "Foo\n").unwrap();
        fs::write(dir.join("c.go"), "Foo\nFoo\nFoo\n").unwrap();
        let stub = Stub::new("./a.go:1:x\n./b.go:1:x\n./c.go:1:x\n./c.go:2:x\n./c.go:3:x\n");
        let got = baseline(&stub, &pinned_version, PINNED, &dir, "Foo").unwrap();
        assert_eq!(got.files, ["c.go", "a.go", "b.go"]);
        assert_eq!(got.bytes, 12 + 14 + 4);
        assert_eq!(got.tokens, 3 + 6 + 1);
    }

    #[test]
    fn baseline_reads_at_most_the_top_files() {
        let dir = scratch_dir("rg-top");
        fs::create_dir_all(&dir).unwrap();
        let mut rows = String::new();
        for i in 0..MAX_BASELINE_FILES + 2 {
            fs::write(dir.join(format!("f{i}.go")), "x\n").unwrap();
            rows.push_str(&format!("./f{i}.go:1:x\n"));
        }
        let got = baseline(&Stub::new(&rows), &pinned_version, PINNED, &dir, "x").unwrap();
        assert_eq!(got.files.len(), MAX_BASELINE_FILES);
    }

    #[test]
    fn other_version_fails_and_missing_tool_skips() {
        let dir = scratch_dir("rg-version");
        let stub = Stub::new("");
        let other = || Ok("14.0.0".to_string());
        let err = candidates(&stub, &other, PINNED, &dir, "Foo").unwrap_err();
        assert_eq!(
            err,
            RgError::VersionMismatch {
                found: "14.0.0".into(),
                pinned: PINNED.into()
            }
        );
        let missing = || Err(RunError::NotFound);
        let err = candidates(&stub, &missing, PINNED, &dir, "Foo").unwrap_err();
        assert_eq!(err, RgError::Skipped("rg is not installed".into()));
        assert!(stub.calls.borrow().is_empty());
    }

    #[test]
    fn bad_rows_fail() {
        let dir = scratch_dir("rg-bad");
        let stub = Stub::new("a.go:notnumber:x\n");
        let err = candidates(&stub, &pinned_version, PINNED, &dir, "Foo").unwrap_err();
        assert!(matches!(err, RgError::Parse(_)));
    }
}
