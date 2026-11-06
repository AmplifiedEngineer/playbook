// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The data files may not be lower than their merge-base copies unless
//! `overrides.json` has a matching entry. The reader is injected, and a
//! missing merge base fails the check: it never skips.

use crate::gate::{Baseline, Floor, GateError, Overrides};
use crate::process::ProcessRunner;
use crate::runner::QueryKind;
use std::path::Path;

/// The branch whose copy of each file is the merge-base reference.
pub const BASE_REF: &str = "origin/main";

const GIT: &str = "git";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Baseline,
    Floor,
}

/// A data file as it is in the working tree. `current` is `None` when the
/// file does not exist there.
pub struct DataFile {
    /// The name used in override ids: `<name>:<repo>:<kind>`.
    pub name: &'static str,
    pub path: &'static str,
    pub shape: Shape,
    pub current: Option<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MergeBaseError {
    #[error("no merge base with {BASE_REF}: {0}")]
    Missing(String),
}

/// Reads a file as it is at the merge base. `Ok(None)` means the file did not
/// exist there.
pub trait MergeBaseReader {
    fn read(&self, path: &str) -> Result<Option<String>, MergeBaseError>;
}

/// Reads through git, with the process runner injected.
pub struct GitReader<'a> {
    pub runner: &'a dyn ProcessRunner,
    pub root: &'a Path,
}

impl GitReader<'_> {
    fn git(&self, args: &[&str]) -> Result<String, MergeBaseError> {
        let args: Vec<String> = args.iter().map(ToString::to_string).collect();
        self.runner
            .run(GIT, &args, self.root)
            .map_err(|e| MergeBaseError::Missing(format!("{e:?}")))
    }
}

impl MergeBaseReader for GitReader<'_> {
    fn read(&self, path: &str) -> Result<Option<String>, MergeBaseError> {
        let base = self.git(&["merge-base", "HEAD", BASE_REF])?;
        let base = base.trim();
        if self
            .git(&["ls-tree", "--name-only", base, "--", path])?
            .trim()
            .is_empty()
        {
            return Ok(None);
        }
        self.git(&["show", &format!("{base}:{path}")]).map(Some)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MergeFailure {
    #[error(transparent)]
    NoMergeBase(#[from] MergeBaseError),
    #[error("{0} exists at the merge base and is gone now")]
    Deleted(String),
    #[error("{0} does not parse: {1}")]
    Parse(String, String),
    #[error("{path} is lower for {repo} {kind} and {id} has no unexpired override")]
    Lowered {
        path: String,
        repo: String,
        kind: String,
        id: String,
    },
}

type Cells = Vec<(String, QueryKind, u32, u32)>;

fn cells(shape: Shape, text: &str) -> Result<Cells, GateError> {
    Ok(match shape {
        Shape::Baseline => Baseline::parse(text)?
            .repos
            .into_iter()
            .flat_map(|r| {
                r.cells.into_iter().map(move |c| {
                    let (p, rc) = (c.precision_bps.unwrap_or(0), c.recall_bps.unwrap_or(0));
                    (r.repo.clone(), c.kind, p, rc)
                })
            })
            .collect(),
        Shape::Floor => Floor::parse(text)?
            .cells
            .into_iter()
            .map(|c| (c.repo, c.kind, c.precision_bps, c.recall_bps))
            .collect(),
    })
}

fn lowered(
    file: &DataFile,
    before: &str,
    now: &str,
    overrides: &Overrides,
    run_date: &str,
) -> Vec<MergeFailure> {
    let parse = |text| {
        cells(file.shape, text).map_err(|e| MergeFailure::Parse(file.path.into(), e.to_string()))
    };
    let (before, now) = match (parse(before), parse(now)) {
        (Ok(b), Ok(n)) => (b, n),
        (Err(e), _) | (_, Err(e)) => return vec![e],
    };
    let mut found = Vec::new();
    for (repo, kind, p, r) in before {
        let still = now.iter().find(|(nr, nk, _, _)| *nr == repo && *nk == kind);
        let below = still.is_none_or(|(_, _, np, nrc)| *np < p || *nrc < r);
        let id = format!("{}:{repo}:{}", file.name, kind.subcommand());
        if below && overrides.active(&id, run_date).is_none() {
            found.push(MergeFailure::Lowered {
                path: file.path.into(),
                repo,
                kind: kind.subcommand().into(),
                id,
            });
        }
    }
    found
}

/// Every way the files fall below their merge-base copies.
pub fn check(
    files: &[DataFile],
    reader: &dyn MergeBaseReader,
    overrides: &Overrides,
    run_date: &str,
) -> Vec<MergeFailure> {
    let mut found = Vec::new();
    for file in files {
        let before = match reader.read(file.path) {
            Ok(before) => before,
            Err(e) => {
                found.push(e.into());
                continue;
            }
        };
        match (before, &file.current) {
            (None, _) => {}
            (Some(_), None) => found.push(MergeFailure::Deleted(file.path.into())),
            (Some(before), Some(now)) => {
                found.extend(lowered(file, &before, now, overrides, run_date))
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::Override;
    use std::collections::BTreeMap;

    const RUN_DATE: &str = "2026-11-07";
    const FILES: [(&str, &str, Shape); 3] = [
        ("baseline", "data/baseline.json", Shape::Baseline),
        ("floor", "data/floor.json", Shape::Floor),
        (
            "gate-baseline",
            "fixtures/gate-baseline.json",
            Shape::Baseline,
        ),
    ];

    struct Fake {
        base: BTreeMap<&'static str, String>,
        no_merge_base: bool,
    }

    impl MergeBaseReader for Fake {
        fn read(&self, path: &str) -> Result<Option<String>, MergeBaseError> {
            if self.no_merge_base {
                return Err(MergeBaseError::Missing("shallow clone".into()));
            }
            Ok(self.base.get(path).cloned())
        }
    }

    fn text(shape: Shape, p: u32) -> String {
        match shape {
            Shape::Floor => format!(
                r#"{{"cells":[{{"repo":"r","kind":"refs","precision_bps":{p},"recall_bps":6000}}]}}"#
            ),
            Shape::Baseline => format!(
                r#"{{"tasks_version":1,"waivers":[],"repos":[{{"repo":"r","status":"measured","cells":[{{"kind":"refs","tasks":10,"excluded":0,"precision_bps":{p},"recall_bps":6000,"correct":1,"bytes":1,"tokens":1,"bytes_per_correct":1}}]}}]}}"#
            ),
        }
    }

    fn files(now: Option<u32>) -> Vec<DataFile> {
        FILES
            .map(|(name, path, shape)| DataFile {
                name,
                path,
                shape,
                current: now.map(|p| text(shape, p)),
            })
            .into()
    }

    fn reader(before: Option<u32>) -> Fake {
        let base = FILES
            .iter()
            .filter_map(|(_, path, shape)| before.map(|p| (*path, text(*shape, p))))
            .collect();
        Fake {
            base,
            no_merge_base: false,
        }
    }

    fn overrides(expires: &str) -> Overrides {
        let entry = |name: &str| Override {
            id: format!("{name}:r:refs"),
            reason: "recorded".into(),
            expires: expires.into(),
            repo: None,
            kind: None,
            precision_drop_bps: 0,
            recall_drop_bps: 0,
        };
        Overrides {
            entries: FILES.iter().map(|f| entry(f.0)).collect(),
        }
    }

    fn none() -> Overrides {
        Overrides {
            entries: Vec::new(),
        }
    }

    #[test]
    fn a_missing_merge_base_fails_and_never_skips() {
        let mut fake = reader(Some(8000));
        fake.no_merge_base = true;
        let found = check(&files(Some(8000)), &fake, &none(), RUN_DATE);
        assert_eq!(found.len(), FILES.len());
        assert!(matches!(found[0], MergeFailure::NoMergeBase(_)));
    }

    #[test]
    fn files_absent_at_the_merge_base_pass_for_all_three() {
        assert!(check(&files(Some(8000)), &reader(None), &none(), RUN_DATE).is_empty());
    }

    #[test]
    fn equal_and_higher_values_pass() {
        assert!(check(&files(Some(8000)), &reader(Some(8000)), &none(), RUN_DATE).is_empty());
        assert!(check(&files(Some(8100)), &reader(Some(8000)), &none(), RUN_DATE).is_empty());
    }

    #[test]
    fn a_lower_value_fails_without_a_matching_unexpired_override() {
        let low = files(Some(7999));
        assert_eq!(
            check(&low, &reader(Some(8000)), &none(), RUN_DATE).len(),
            FILES.len()
        );
        assert!(check(&low, &reader(Some(8000)), &overrides(RUN_DATE), RUN_DATE).is_empty());
        let expired = check(
            &low,
            &reader(Some(8000)),
            &overrides("2026-11-06"),
            RUN_DATE,
        );
        assert_eq!(expired.len(), FILES.len());
    }

    #[test]
    fn a_deleted_file_fails() {
        let found = check(&files(None), &reader(Some(8000)), &none(), RUN_DATE);
        assert!(matches!(&found[0], MergeFailure::Deleted(p) if p == FILES[0].1));
    }
}
