// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Narrows the committed gate inputs to one language, so each leg of the
//! nightly matrix judges only the repos it ran.

use crate::corpus::Manifest;
use crate::gate::{Baseline, Floor};

fn names<'a>(manifest: &'a Manifest, language: &str) -> Vec<&'a str> {
    manifest
        .repos
        .iter()
        .filter(|r| r.language == language)
        .map(|r| r.name.as_str())
        .collect()
}

/// Keeps the baseline rows, waivers and floor cells of repos written in
/// `language`.
pub fn scope_to_language(
    manifest: &Manifest,
    language: &str,
    mut baseline: Baseline,
    mut floor: Floor,
) -> (Baseline, Floor) {
    let keep = names(manifest, language);
    baseline.repos.retain(|r| keep.contains(&r.repo.as_str()));
    baseline.waivers.retain(|w| keep.contains(&w.repo.as_str()));
    floor.cells.retain(|c| keep.contains(&c.repo.as_str()));
    (baseline, floor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::Waiver;
    use crate::runner::{RepoReport, RepoStatus};

    fn manifest() -> Manifest {
        Manifest::bundled().unwrap()
    }

    fn row(repo: &str) -> RepoReport {
        RepoReport {
            repo: repo.into(),
            status: RepoStatus::Measured,
            cells: Vec::new(),
        }
    }

    fn waiver(repo: &str) -> Waiver {
        Waiver {
            repo: repo.into(),
            kind: None,
            reason: "not yet in the corpus".into(),
        }
    }

    #[test]
    fn keeps_only_repos_of_the_language() {
        let baseline = Baseline {
            tasks_version: 1,
            waivers: vec![waiver("hugo"), waiver("pandas")],
            repos: vec![row("cobra"), row("ripgrep")],
        };
        let (kept, _) = scope_to_language(&manifest(), "go", baseline, Floor { cells: vec![] });
        let repos: Vec<_> = kept.repos.iter().map(|r| r.repo.as_str()).collect();
        let waivers: Vec<_> = kept.waivers.iter().map(|w| w.repo.as_str()).collect();
        assert_eq!(repos, ["cobra"]);
        assert_eq!(waivers, ["hugo"]);
    }

    #[test]
    fn an_unknown_language_keeps_nothing() {
        let baseline = Baseline {
            tasks_version: 1,
            waivers: vec![waiver("hugo")],
            repos: vec![row("cobra")],
        };
        let (kept, _) = scope_to_language(&manifest(), "cobol", baseline, Floor { cells: vec![] });
        assert!(kept.repos.is_empty() && kept.waivers.is_empty());
    }
}
