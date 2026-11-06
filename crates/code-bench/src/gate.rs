// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The accuracy and token gate. Every comparison runs per repo and query kind
//! pair, never on an average, and uses integer basis points or counts.
//! Latency stays informational. The run date is a parameter, so nothing here
//! reads the clock.

use crate::metrics::{cost_per_correct, BasisPoints, BASIS_POINTS_PER_UNIT};
use crate::runner::{Cell, QueryKind, RepoReport, RepoStatus, Report};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Precision may not drop at all.
pub const PRECISION_DROP_TOLERANCE: BasisPoints = 0;
/// Recall may not drop at all.
pub const RECALL_DROP_TOLERANCE: BasisPoints = 0;
/// Rise of bytes (and tokens) per correct result that covers tokenizer
/// rounding, in basis points of the baseline value.
pub const BYTES_PER_CORRECT_RISE_TOLERANCE: BasisPoints = 50;
/// Bytes rise allowed per whole point (100 basis points) of precision plus
/// recall gained, in basis points of the baseline value.
pub const MAX_BYTES_RISE_PER_GAIN_POINT: BasisPoints = 200;
/// A cell with fewer tasks is unmeasured.
pub const MIN_TASKS_PER_CELL: u32 = 10;

const BASIS_POINTS_PER_POINT: u32 = 100;
const DATE_LEN: usize = 10;
const DATE_DASHES: [usize; 2] = [4, 7];

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GateError {
    #[error("{file} does not parse: {reason}")]
    Parse { file: &'static str, reason: String },
}

fn parse_error(file: &'static str, e: impl ToString) -> GateError {
    GateError::Parse {
        file,
        reason: e.to_string(),
    }
}

/// A listed skip. Without a kind it skips the whole repo; with one it lists
/// an unmeasured cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Waiver {
    pub repo: String,
    pub kind: Option<QueryKind>,
    pub reason: String,
}

/// `baseline.json`: the committed measurements and the listed skips.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    pub tasks_version: u32,
    pub waivers: Vec<Waiver>,
    pub repos: Vec<RepoReport>,
}

impl Baseline {
    pub fn parse(text: &str) -> Result<Self, GateError> {
        let file: Self = serde_json::from_str(text).map_err(|e| parse_error("baseline", e))?;
        match file.waivers.iter().find(|w| w.reason.trim().is_empty()) {
            Some(w) => Err(parse_error("baseline", format!("{} has no reason", w.repo))),
            None => Ok(file),
        }
    }

    fn cell(&self, repo: &str, kind: QueryKind) -> Option<&Cell> {
        let found = self.repos.iter().find(|r| r.repo == repo)?;
        found.cells.iter().find(|c| c.kind == kind)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FloorCell {
    pub repo: String,
    pub kind: QueryKind,
    pub precision_bps: BasisPoints,
    pub recall_bps: BasisPoints,
}

/// `floor.json`: the lowest accepted values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Floor {
    pub cells: Vec<FloorCell>,
}

impl Floor {
    pub fn parse(text: &str) -> Result<Self, GateError> {
        serde_json::from_str(text).map_err(|e| parse_error("floor", e))
    }
}

/// One recorded exception. A task removal uses the id `task-removed:<id>`;
/// a lowered data file uses `<file>:<repo>:<kind>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Override {
    pub id: String,
    pub reason: String,
    /// Last day the override applies, as `YYYY-MM-DD`, inclusive.
    pub expires: String,
    pub repo: Option<String>,
    pub kind: Option<QueryKind>,
    #[serde(default)]
    pub precision_drop_bps: BasisPoints,
    #[serde(default)]
    pub recall_drop_bps: BasisPoints,
}

/// `overrides.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overrides {
    pub entries: Vec<Override>,
}

fn is_date(text: &str) -> bool {
    text.len() == DATE_LEN
        && text.bytes().enumerate().all(|(i, b)| {
            if DATE_DASHES.contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
}

impl Overrides {
    pub fn parse(text: &str) -> Result<Self, GateError> {
        let file: Self = serde_json::from_str(text).map_err(|e| parse_error("overrides", e))?;
        for e in &file.entries {
            if e.id.is_empty() || e.reason.trim().is_empty() || !is_date(&e.expires) {
                return Err(parse_error(
                    "overrides",
                    format!("entry {:?} is incomplete", e.id),
                ));
            }
        }
        Ok(file)
    }

    /// The entry with this id, when it has not expired. An override that
    /// expires on the run date still applies.
    pub fn active(&self, id: &str, run_date: &str) -> Option<&Override> {
        self.entries
            .iter()
            .find(|e| e.id == id && e.expires.as_str() >= run_date)
    }

    fn delta(&self, repo: &str, kind: QueryKind, run_date: &str) -> (BasisPoints, BasisPoints) {
        self.entries
            .iter()
            .filter(|e| e.expires.as_str() >= run_date)
            .filter(|e| e.repo.as_deref() == Some(repo) && e.kind == Some(kind))
            .fold((0, 0), |(p, r), e| {
                (p + e.precision_drop_bps, r + e.recall_drop_bps)
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Precision,
    Recall,
    Bytes,
    Tokens,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Failure {
    #[error("baseline is task set version {baseline} and the report is {report}")]
    VersionMismatch { baseline: u32, report: u32 },
    #[error("repo {repo} failed and the baseline does not list it as a skip: {reason}")]
    RepoFailed { repo: String, reason: String },
    #[error("repo {repo} is in the baseline and missing from the report")]
    RepoMissing { repo: String },
    #[error("{repo} {kind:?} has {tasks} tasks and the baseline does not list it as unmeasured")]
    Unmeasured {
        repo: String,
        kind: QueryKind,
        tasks: u32,
    },
    #[error("{repo} {kind:?} {metric:?} moved {observed} basis points against the {against}, {allowed} allowed")]
    Regression {
        repo: String,
        kind: QueryKind,
        metric: Metric,
        against: &'static str,
        observed: u64,
        allowed: u64,
    },
    #[error("task {0} left the task set without an override")]
    TaskRemoved(String),
}

pub struct Inputs<'a> {
    pub baseline: &'a Baseline,
    pub floor: &'a Floor,
    pub overrides: &'a Overrides,
    pub report: &'a Report,
    /// `YYYY-MM-DD`.
    pub run_date: &'a str,
    pub prior_task_ids: &'a BTreeSet<String>,
    pub task_ids: &'a BTreeSet<String>,
}

fn waived(i: &Inputs<'_>, repo: &str, kind: Option<QueryKind>) -> bool {
    i.baseline
        .waivers
        .iter()
        .any(|w| w.repo == repo && w.kind == kind)
}

/// Bytes or tokens per correct result may rise by the tolerance when
/// precision and recall are flat, and by the per-point budget when either
/// rises. The budget then replaces the tolerance.
fn rise_failure(base: &Cell, cell: &Cell, gain: u32, metric: Metric) -> Option<(u64, u64)> {
    let (b, c) = match metric {
        Metric::Tokens => (base.tokens, cell.tokens),
        _ => (base.bytes, cell.bytes),
    };
    let (b, c) = (
        cost_per_correct(b, base.correct)?,
        cost_per_correct(c, cell.correct)?,
    );
    let allowed = if gain > 0 {
        u64::from(gain * MAX_BYTES_RISE_PER_GAIN_POINT / BASIS_POINTS_PER_POINT)
    } else {
        u64::from(BYTES_PER_CORRECT_RISE_TOLERANCE)
    };
    let over = c * BASIS_POINTS_PER_UNIT > b * (BASIS_POINTS_PER_UNIT + allowed);
    over.then(|| ((c - b) * BASIS_POINTS_PER_UNIT).div_ceil(b.max(1)))
        .map(|observed| (observed, allowed))
}

fn over(
    found: &mut Vec<Failure>,
    (repo, kind): (&str, QueryKind),
    (metric, against): (Metric, &'static str),
    (observed, allowed): (u64, u64),
) {
    if observed > allowed {
        found.push(Failure::Regression {
            repo: repo.to_string(),
            kind,
            metric,
            against,
            observed,
            allowed,
        });
    }
}

fn cell_failures(i: &Inputs<'_>, repo: &str, cell: &Cell) -> Vec<Failure> {
    let (dp, dr) = i.overrides.delta(repo, cell.kind, i.run_date);
    let at = (repo, cell.kind);
    let mut found = Vec::new();
    let (p, r) = (
        cell.precision_bps.unwrap_or(0),
        cell.recall_bps.unwrap_or(0),
    );
    if let Some(base) = i.baseline.cell(repo, cell.kind) {
        let (bp, br) = (
            base.precision_bps.unwrap_or(0),
            base.recall_bps.unwrap_or(0),
        );
        let (drop_p, drop_r) = (bp.saturating_sub(p), br.saturating_sub(r));
        let (tol_p, tol_r) = (PRECISION_DROP_TOLERANCE + dp, RECALL_DROP_TOLERANCE + dr);
        over(
            &mut found,
            at,
            (Metric::Precision, "baseline"),
            (drop_p.into(), tol_p.into()),
        );
        over(
            &mut found,
            at,
            (Metric::Recall, "baseline"),
            (drop_r.into(), tol_r.into()),
        );
        let gain = p.saturating_sub(bp) + r.saturating_sub(br);
        for metric in [Metric::Bytes, Metric::Tokens] {
            if let Some(rise) = rise_failure(base, cell, gain, metric) {
                over(&mut found, at, (metric, "baseline"), rise);
            }
        }
    }
    if let Some(f) = i
        .floor
        .cells
        .iter()
        .find(|f| f.repo == repo && f.kind == cell.kind)
    {
        let (drop_p, drop_r) = (
            f.precision_bps.saturating_sub(p),
            f.recall_bps.saturating_sub(r),
        );
        over(
            &mut found,
            at,
            (Metric::Precision, "floor"),
            (drop_p.into(), dp.into()),
        );
        over(
            &mut found,
            at,
            (Metric::Recall, "floor"),
            (drop_r.into(), dr.into()),
        );
    }
    found
}

fn repo_failures(i: &Inputs<'_>, repo: &RepoReport) -> Vec<Failure> {
    if waived(i, &repo.repo, None) {
        return Vec::new();
    }
    if let RepoStatus::Failed { reason } = &repo.status {
        return vec![Failure::RepoFailed {
            repo: repo.repo.clone(),
            reason: reason.clone(),
        }];
    }
    let mut found = Vec::new();
    for cell in &repo.cells {
        if cell.tasks >= MIN_TASKS_PER_CELL {
            found.extend(cell_failures(i, &repo.repo, cell));
        } else if !waived(i, &repo.repo, Some(cell.kind)) {
            found.push(Failure::Unmeasured {
                repo: repo.repo.clone(),
                kind: cell.kind,
                tasks: cell.tasks,
            });
        }
    }
    found
}

/// Every failure of the run; an empty list means the gate passes.
pub fn evaluate(i: &Inputs<'_>) -> Vec<Failure> {
    let mut found = Vec::new();
    if i.baseline.tasks_version != i.report.tasks_version {
        found.push(Failure::VersionMismatch {
            baseline: i.baseline.tasks_version,
            report: i.report.tasks_version,
        });
    }
    for id in i.prior_task_ids.difference(i.task_ids) {
        if i.overrides
            .active(&format!("task-removed:{id}"), i.run_date)
            .is_none()
        {
            found.push(Failure::TaskRemoved(id.clone()));
        }
    }
    for repo in &i.report.repos {
        found.extend(repo_failures(i, repo));
    }
    for base in &i.baseline.repos {
        let seen = i.report.repos.iter().any(|r| r.repo == base.repo);
        if !seen && !waived(i, &base.repo, None) {
            found.push(Failure::RepoMissing {
                repo: base.repo.clone(),
            });
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUN_DATE: &str = "2026-11-07";
    const KIND: QueryKind = QueryKind::Refs;

    fn cell(kind: QueryKind, tasks: u32, p: u32, r: u32, bytes: u64) -> Cell {
        Cell {
            kind,
            tasks,
            excluded: 0,
            precision_bps: Some(p),
            recall_bps: Some(r),
            correct: 1,
            bytes,
            tokens: 0,
            bytes_per_correct: Some(bytes),
        }
    }

    fn repo(name: &str, cells: Vec<Cell>) -> RepoReport {
        RepoReport {
            repo: name.into(),
            status: RepoStatus::Measured,
            cells,
        }
    }

    fn baseline(repos: Vec<RepoReport>, waivers: Vec<Waiver>) -> Baseline {
        Baseline {
            tasks_version: 1,
            waivers,
            repos,
        }
    }

    fn run(
        base: &Baseline,
        floor: &Floor,
        entries: Vec<Override>,
        repos: Vec<RepoReport>,
    ) -> Vec<Failure> {
        let ids = BTreeSet::new();
        let report = Report {
            tasks_version: 1,
            repos,
        };
        let overrides = Overrides { entries };
        let inputs = Inputs {
            baseline: base,
            floor,
            overrides: &overrides,
            report: &report,
            run_date: RUN_DATE,
            prior_task_ids: &ids,
            task_ids: &ids,
        };
        evaluate(&inputs)
    }

    fn no_floor() -> Floor {
        Floor { cells: Vec::new() }
    }

    /// One cell of `r` against a baseline of 8000, 6000 and 10,000 bytes.
    fn change(p: u32, r: u32, bytes: u64, entries: Vec<Override>) -> Vec<Failure> {
        let base = baseline(
            vec![repo("r", vec![cell(KIND, 10, 8000, 6000, 10_000)])],
            vec![],
        );
        run(
            &base,
            &no_floor(),
            entries,
            vec![repo("r", vec![cell(KIND, 10, p, r, bytes)])],
        )
    }

    #[test]
    fn drops_fail_from_one_basis_point() {
        for (p, r, fails) in [(8000, 6000, false), (7999, 6000, true), (8000, 5999, true)] {
            assert_eq!(!change(p, r, 10_000, vec![]).is_empty(), fails, "{p} {r}");
        }
    }

    #[test]
    fn bytes_tolerance_is_fifty_basis_points_when_flat() {
        assert!(change(8000, 6000, 10_050, vec![]).is_empty());
        assert!(!change(8000, 6000, 10_051, vec![]).is_empty());
    }

    #[test]
    fn the_budget_is_two_basis_points_of_bytes_per_gain_basis_point() {
        let table = [
            (8010, 6010, 10_040, true),
            (8010, 6010, 10_041, false),
            (8010, 6000, 10_020, true),
            (8010, 6000, 10_021, false),
            (8000, 6100, 10_200, true),
            (8000, 6100, 10_201, false),
            (8000, 6010, 50_000, false),
        ];
        for (p, r, bytes, passes) in table {
            assert_eq!(
                change(p, r, bytes, vec![]).is_empty(),
                passes,
                "{p} {r} {bytes}"
            );
        }
    }
}
