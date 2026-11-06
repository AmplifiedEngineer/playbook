// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Retrieval metrics. Every ratio is an integer count of basis points (one
//! hundredth of a percent), rounded half up, so comparisons never touch
//! floats. `retrieved` is a ranked list and `truth` the set of relevant items.
//! A metric with no defined value returns `None`, never 1.

use crate::tokenizer::count_tokens;
use std::collections::HashSet;
use std::hash::Hash;

/// A ratio in hundredths of a percent.
pub type BasisPoints = u32;

/// One whole unit (100 percent) in basis points.
pub const BASIS_POINTS_PER_UNIT: u64 = 10_000;

/// Rounds `num / den` half up. `den` must not be zero.
fn round_div(num: u64, den: u64) -> u64 {
    (2 * num + den) / (2 * den)
}

fn ratio(num: u64, den: u64) -> BasisPoints {
    u32::try_from(round_div(num * BASIS_POINTS_PER_UNIT, den)).unwrap_or(u32::MAX)
}

fn distinct<T: Eq + Hash>(retrieved: &[T]) -> Vec<&T> {
    let mut seen = HashSet::new();
    retrieved.iter().filter(|item| seen.insert(*item)).collect()
}

fn hits<T: Eq + Hash>(retrieved: &[T], truth: &[T]) -> (u64, u64) {
    let truth: HashSet<&T> = truth.iter().collect();
    let unique = distinct(retrieved);
    let found = unique.iter().filter(|item| truth.contains(**item)).count();
    (found as u64, unique.len() as u64)
}

/// Precision is distinct correct results divided by distinct results.
/// `None` when nothing was retrieved.
pub fn precision<T: Eq + Hash>(retrieved: &[T], truth: &[T]) -> Option<BasisPoints> {
    let (found, total) = hits(retrieved, truth);
    (total > 0).then(|| ratio(found, total))
}

/// Recall is distinct correct results divided by distinct true items.
/// `None` when the truth is empty; 0 when truth exists and nothing was found.
pub fn recall<T: Eq + Hash>(retrieved: &[T], truth: &[T]) -> Option<BasisPoints> {
    let wanted = truth.iter().collect::<HashSet<_>>().len() as u64;
    (wanted > 0).then(|| ratio(hits(retrieved, truth).0, wanted))
}

/// F1 is `2PR / (P + R)`, and 0 when both are 0.
pub fn f1(precision: BasisPoints, recall: BasisPoints) -> BasisPoints {
    let (p, r) = (u64::from(precision), u64::from(recall));
    if p + r == 0 {
        return 0;
    }
    u32::try_from(round_div(2 * p * r, p + r)).unwrap_or(u32::MAX)
}

/// The 1-based rank of the first relevant result.
fn first_rank<T: Eq + Hash>(retrieved: &[T], truth: &[T]) -> Option<usize> {
    let truth: HashSet<&T> = truth.iter().collect();
    retrieved
        .iter()
        .position(|item| truth.contains(item))
        .map(|i| i + 1)
}

/// One query's ranked results and the truth they are judged against.
pub type Run<'a, T> = (&'a [T], &'a [T]);

/// Mean reciprocal rank, `1 / rank` of the first relevant result per query,
/// 0 for a query with none, averaged over queries. `None` without queries.
pub fn mrr<T: Eq + Hash>(runs: &[Run<'_, T>]) -> Option<BasisPoints> {
    if runs.is_empty() {
        return None;
    }
    let sum: u64 = runs
        .iter()
        .filter_map(|(retrieved, truth)| first_rank(retrieved, truth))
        .map(|rank| round_div(BASIS_POINTS_PER_UNIT, rank as u64))
        .sum();
    u32::try_from(round_div(sum, runs.len() as u64)).ok()
}

/// Acc@k is the share of queries with a relevant result at rank `k` or
/// better. `None` without queries.
pub fn acc_at_k<T: Eq + Hash>(runs: &[Run<'_, T>], k: usize) -> Option<BasisPoints> {
    if runs.is_empty() {
        return None;
    }
    let hit = runs
        .iter()
        .filter(|(retrieved, truth)| first_rank(retrieved, truth).is_some_and(|rank| rank <= k))
        .count();
    Some(ratio(hit as u64, runs.len() as u64))
}

/// Output size in bytes.
pub fn bytes(text: &str) -> u64 {
    text.len() as u64
}

/// Output size in tokens by the pinned tokenizer.
pub fn tokens(text: &str) -> u64 {
    count_tokens(text)
}

/// Cost per correct result, rounded half up. `None` when nothing was correct.
pub fn cost_per_correct(cost: u64, correct: u64) -> Option<u64> {
    (correct > 0).then(|| round_div(cost, correct))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRUTH: [&str; 4] = ["a", "b", "c", "d"];

    #[test]
    fn precision_and_recall_match_hand_values() {
        let got = ["a", "b", "x"];
        assert_eq!(precision(&got, &TRUTH), Some(6667));
        assert_eq!(recall(&got, &TRUTH), Some(5000));
    }

    #[test]
    fn duplicate_results_count_once() {
        let got = ["a", "a", "x"];
        assert_eq!(precision(&got, &TRUTH), Some(5000));
        assert_eq!(recall(&got, &TRUTH), Some(2500));
    }

    #[test]
    fn empty_results_with_truth_give_zero_recall() {
        assert_eq!(recall::<&str>(&[], &TRUTH), Some(0));
    }

    #[test]
    fn empty_results_give_undefined_precision() {
        assert_eq!(precision::<&str>(&[], &TRUTH), None);
    }

    #[test]
    fn empty_truth_gives_undefined_recall_not_one() {
        assert_eq!(recall(&["a"], &[]), None);
        assert_eq!(recall::<&str>(&[], &[]), None);
    }

    #[test]
    fn f1_of_two_zeros_is_zero() {
        assert_eq!(f1(0, 0), 0);
    }

    #[test]
    fn f1_matches_hand_value() {
        assert_eq!(f1(6667, 5000), 5714);
        assert_eq!(f1(10_000, 10_000), 10_000);
    }

    #[test]
    fn mrr_and_acc_are_zero_when_nothing_relevant() {
        let runs: [Run<'_, &str>; 1] = [(&["x", "y"], &TRUTH)];
        assert_eq!(mrr(&runs), Some(0));
        assert_eq!(acc_at_k(&runs, 5), Some(0));
    }

    #[test]
    fn mrr_and_acc_are_zero_with_no_retrieved_results() {
        let runs: [Run<'_, &str>; 1] = [(&[], &TRUTH)];
        assert_eq!(mrr(&runs), Some(0));
        assert_eq!(acc_at_k(&runs, 3), Some(0));
    }

    #[test]
    fn mrr_averages_reciprocal_ranks() {
        let runs: [Run<'_, &str>; 2] = [(&["a"], &TRUTH), (&["x", "b"], &TRUTH)];
        assert_eq!(mrr(&runs), Some(7500));
    }

    #[test]
    fn acc_at_k_counts_rank_k_but_not_k_plus_one() {
        let third: [Run<'_, &str>; 1] = [(&["x", "y", "a"], &TRUTH)];
        assert_eq!(acc_at_k(&third, 3), Some(10_000));
        assert_eq!(acc_at_k(&third, 2), Some(0));
    }

    #[test]
    fn acc_at_zero_never_hits() {
        let runs: [Run<'_, &str>; 1] = [(&["a"], &TRUTH)];
        assert_eq!(acc_at_k(&runs, 0), Some(0));
    }

    #[test]
    fn no_queries_are_undefined() {
        let runs: [Run<'_, &str>; 0] = [];
        assert_eq!(mrr(&runs), None);
        assert_eq!(acc_at_k(&runs, 1), None);
    }

    #[test]
    fn bytes_and_tokens_use_the_pinned_counter() {
        assert_eq!(bytes("fn a() {}"), 9);
        assert_eq!(tokens("fn a() {}"), 6);
    }

    #[test]
    fn cost_per_correct_rounds_and_guards_zero() {
        assert_eq!(cost_per_correct(10, 4), Some(3));
        assert_eq!(cost_per_correct(10, 0), None);
    }
}
