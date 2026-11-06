// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr stack <pr> --digest`: what the open PRs above one PR change,
//! as a short per-file list. A review of one PR reads it to know which code
//! is rewritten higher up, without reading the upper diffs in full.

use super::shared::gh;
use super::stack::{detect, State};
use crate::common::par;
use std::collections::BTreeMap;

/// Most file lines in one digest, across all upper PRs.
const DIGEST_MAX_LINES: usize = 60;
/// Most line ranges listed for one file.
const RANGES_MAX: usize = 8;
/// The path a diff gives a file that does not exist on one side.
const NO_FILE: &str = "/dev/null";

/// What one upper PR does to one file.
#[derive(Default)]
struct FileChange {
    added: u64,
    removed: u64,
    /// Old-side `(start, end)` ranges: the lines of the lower PR it touches.
    touched: Vec<(u64, u64)>,
}

fn range(spec: &str, mark: char) -> Option<(u64, u64)> {
    let spec = spec.strip_prefix(mark)?;
    let (start, len) = match spec.split_once(',') {
        Some((s, l)) => (s.parse::<u64>().ok()?, l.parse::<u64>().ok()?),
        None => (spec.parse::<u64>().ok()?, 1),
    };
    (len > 0).then(|| (start, start + len - 1))
}

fn changes(diff: &str) -> BTreeMap<String, FileChange> {
    let mut out: BTreeMap<String, FileChange> = BTreeMap::new();
    let mut old: Option<&str> = None;
    let mut current: Option<String> = None;
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("--- ") {
            old = Some(rest.strip_prefix("a/").unwrap_or(rest));
            current = None;
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            let new = rest.strip_prefix("b/").unwrap_or(rest);
            let name = match (new, old) {
                (n, _) if n != NO_FILE => n,
                (_, Some(o)) => o,
                _ => continue,
            };
            current = Some(name.to_string());
            out.entry(name.to_string()).or_default();
        } else if let Some(file) = &current {
            let entry = out.entry(file.clone()).or_default();
            if line.starts_with("@@") {
                let old_range = line.split_whitespace().find_map(|t| range(t, '-'));
                entry.touched.extend(old_range);
            } else if line.starts_with('+') {
                entry.added += 1;
            } else if line.starts_with('-') {
                entry.removed += 1;
            }
        }
    }
    out
}

/// One line per file and upper PR, in stack order, cut at the line budget.
pub(super) fn digest(upper: &[(u64, String)]) -> String {
    let mut lines = Vec::new();
    let mut unlisted = 0;
    for (n, diff) in upper {
        for (file, c) in changes(diff) {
            if lines.len() >= DIGEST_MAX_LINES {
                unlisted += 1;
                continue;
            }
            let shown: Vec<String> = c
                .touched
                .iter()
                .take(RANGES_MAX)
                .map(|(a, b)| {
                    if a == b {
                        a.to_string()
                    } else {
                        format!("{a}-{b}")
                    }
                })
                .collect();
            let more = c.touched.len().saturating_sub(RANGES_MAX);
            let tail = match (shown.is_empty(), more) {
                (true, _) => String::from("only adds lines"),
                (false, 0) => format!("changes lines {}", shown.join(", ")),
                (false, m) => format!("changes lines {} and {m} more ranges", shown.join(", ")),
            };
            lines.push(format!("#{n} {file} (+{} -{}): {tail}", c.added, c.removed));
        }
    }
    if unlisted > 0 {
        lines.push(format!("and {unlisted} more files not listed"));
    }
    if lines.is_empty() {
        return "No open PR above this one.".to_string();
    }
    format!(
        "Lines below are numbered as this PR leaves them. A finding on one of them is changed above: drop it, or point at the PR.\n{}",
        lines.join("\n")
    )
}

/// `pr stack <pr> --digest`: fetch the diffs of the open PRs above and print.
pub fn run(pr: Option<u64>) -> Result<String, String> {
    let stack = detect(&|a| gh(a), pr)?;
    let above: Vec<u64> = stack
        .prs
        .iter()
        .skip_while(|p| p.number != stack.current)
        .skip(1)
        .filter(|p| p.state == State::Open)
        .map(|p| p.number)
        .collect();
    let fetched = par::map(&above, par::MAX_CONCURRENT, |n| {
        gh(&["pr", "diff", &n.to_string()]).map(|d| (*n, d))
    });
    let upper = fetched
        .into_iter()
        .collect::<Result<Vec<_>, String>>()
        .map_err(|e| format!("error: could not read a PR above: {e}"))?;
    Ok(digest(&upper))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIDDLE: &str = "--- a/src/a.rs\n+++ b/src/a.rs\n@@ -10,3 +10,2 @@\n x\n-y\n-z\n+w\n@@ -40 +39,2 @@\n+p\n+q\n";
    const TOP: &str = "--- a/src/b.rs\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-gone\n-too\n--- a/src/c.rs\n+++ b/src/c.rs\n@@ -0,0 +1,2 @@\n+n\n+m\n";

    #[test]
    fn it_lists_files_counts_and_old_side_ranges_per_upper_pr() {
        let d = digest(&[(2, MIDDLE.into()), (3, TOP.into())]);
        assert!(
            d.contains("#2 src/a.rs (+3 -2): changes lines 10-12, 40"),
            "{d}"
        );
        assert!(d.contains("#3 src/b.rs (+0 -2): changes lines 1-2"), "{d}");
        assert!(d.contains("#3 src/c.rs (+2 -0): only adds lines"), "{d}");
    }

    #[test]
    fn it_says_so_when_nothing_is_above() {
        assert_eq!(digest(&[]), "No open PR above this one.");
    }

    #[test]
    fn it_stops_at_the_line_budget_and_counts_the_rest() {
        let diff: String = (0..DIGEST_MAX_LINES + 5)
            .map(|i| format!("--- a/f{i}\n+++ b/f{i}\n@@ -1 +1 @@\n-a\n+b\n"))
            .collect();
        let d = digest(&[(2, diff)]);
        assert_eq!(d.lines().count(), 1 + DIGEST_MAX_LINES + 1, "{d}");
        assert!(d.ends_with("and 5 more files not listed"), "{d}");
    }
}
