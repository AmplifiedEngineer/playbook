// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The end-to-end report and its statistics. Every published number is an
//! integer: accuracy and differences in basis points, tokens as counts.

use super::Kind;
use crate::metrics::BASIS_POINTS_PER_UNIT;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Two-sided 95 percent normal quantile, in hundredths.
const Z_HUNDREDTHS: f64 = 196.0;
const HUNDRED: f64 = 100.0;
const BP: f64 = BASIS_POINTS_PER_UNIT as f64;

/// One task and repeat, both arms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trial {
    pub task: usize,
    pub kind: Kind,
    pub with_correct: bool,
    pub without_correct: bool,
    pub with_tokens: u64,
    pub without_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KindReport {
    pub kind: Kind,
    pub tasks: u32,
    pub difference_bp: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    /// Where the numbers came from. A fixture says so here.
    pub source: String,
    pub model: String,
    pub repeats: u32,
    pub tasks: u32,
    pub excluded: u32,
    /// False when the dollar cap cut the run short.
    pub complete: bool,
    pub spent_cents: u64,
    pub trials_with: u32,
    pub trials_without: u32,
    pub with_accuracy_bp: i64,
    pub without_accuracy_bp: i64,
    pub difference_bp: i64,
    pub lower_bp: i64,
    pub upper_bp: i64,
    pub with_repeat_variance: u64,
    pub without_repeat_variance: u64,
    pub with_tokens_per_task: u64,
    pub without_tokens_per_task: u64,
    pub kinds: Vec<KindReport>,
}

impl Report {
    /// The file text: pretty JSON with a trailing newline.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        Ok(text)
    }
}

fn rate_bp(correct: usize, total: usize) -> i64 {
    if total == 0 {
        return 0;
    }
    ((correct as f64 * BP) / total as f64).round() as i64
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len().max(1) as f64
}

/// Per task: the mean over repeats of with minus without, in basis points.
fn paired_differences(trials: &[Trial]) -> Vec<f64> {
    let mut by_task: BTreeMap<usize, Vec<f64>> = BTreeMap::new();
    for t in trials {
        let d = f64::from(u8::from(t.with_correct)) - f64::from(u8::from(t.without_correct));
        by_task.entry(t.task).or_default().push(d * BP);
    }
    by_task.values().map(|d| mean(d)).collect()
}

/// Mean over tasks of the variance of one arm's correctness across repeats.
fn repeat_variance(trials: &[Trial], arm: impl Fn(&Trial) -> bool) -> u64 {
    let mut by_task: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
    for t in trials {
        let e = by_task.entry(t.task).or_default();
        e.0 += usize::from(arm(t));
        e.1 += 1;
    }
    let variances: Vec<f64> = by_task
        .values()
        .map(|&(right, n)| {
            let p = right as f64 / n as f64;
            p * (1.0 - p) * BP * BP
        })
        .collect();
    mean(&variances).round() as u64
}

fn tokens_per_trial(total: u64, trials: usize) -> u64 {
    total / trials.max(1) as u64
}

/// Builds the report. The interval treats tasks as the sampling unit and
/// uses the normal approximation of the paired mean.
pub fn build(
    source: &str,
    model: &str,
    repeats: u32,
    excluded: u32,
    complete: bool,
    spent_cents: u64,
    trials: &[Trial],
) -> Report {
    let n = trials.len();
    let with = trials.iter().filter(|t| t.with_correct).count();
    let without = trials.iter().filter(|t| t.without_correct).count();
    let diffs = paired_differences(trials);
    let point = mean(&diffs);
    let var =
        diffs.iter().map(|d| (d - point).powi(2)).sum::<f64>() / (diffs.len().max(2) - 1) as f64;
    let half = Z_HUNDREDTHS / HUNDRED * (var / diffs.len().max(1) as f64).sqrt();
    let kinds = trials
        .iter()
        .map(|t| t.kind)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|kind| {
            let of_kind: Vec<Trial> = trials.iter().filter(|t| t.kind == kind).cloned().collect();
            KindReport {
                kind,
                tasks: of_kind
                    .iter()
                    .map(|t| t.task)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len() as u32,
                difference_bp: rate_bp(
                    of_kind.iter().filter(|t| t.with_correct).count(),
                    of_kind.len(),
                ) - rate_bp(
                    of_kind.iter().filter(|t| t.without_correct).count(),
                    of_kind.len(),
                ),
            }
        })
        .collect();
    Report {
        source: source.to_string(),
        model: model.to_string(),
        repeats,
        tasks: trials
            .iter()
            .map(|t| t.task)
            .collect::<std::collections::BTreeSet<_>>()
            .len() as u32,
        excluded,
        complete,
        spent_cents,
        trials_with: n as u32,
        trials_without: n as u32,
        with_accuracy_bp: rate_bp(with, n),
        without_accuracy_bp: rate_bp(without, n),
        difference_bp: point.round() as i64,
        lower_bp: (point - half).round() as i64,
        upper_bp: (point + half).round() as i64,
        with_repeat_variance: repeat_variance(trials, |t| t.with_correct),
        without_repeat_variance: repeat_variance(trials, |t| t.without_correct),
        with_tokens_per_task: tokens_per_trial(trials.iter().map(|t| t.with_tokens).sum(), n),
        without_tokens_per_task: tokens_per_trial(trials.iter().map(|t| t.without_tokens).sum(), n),
        kinds,
    }
}
