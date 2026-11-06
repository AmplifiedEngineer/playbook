// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook prose check <files>`: the prose lint of the writing-style skill
//! run on files, for docs and ADRs. The same table backs the
//! `commit-message-sanitizer` hook.
//!
//! A file `<file>.facts` next to a checked file adds two checks: every
//! number, identifier, path and reference in the text must appear in the
//! facts, and the text must fit the length limit of the `kind:` named on the
//! first line of the facts (see `common::prose_ground`).

use crate::common::prose_ground;
use crate::common::prose_lint::{has_hard, lint, Finding};
use std::fs;
use std::path::Path;

/// What a run found.
pub struct Report {
    /// The lines to print.
    pub lines: Vec<String>,
    /// Findings of every kind.
    pub total: usize,
    /// Findings of a hard class.
    pub hard: usize,
}

fn line_for(path: &Path, finding: &Finding, github: bool) -> String {
    if github {
        let message = finding
            .render()
            .replace('%', "%25")
            .replace('\r', "%0D")
            .replace('\n', "%0A");
        format!(
            "::warning file={},line={}::{message}",
            path.display(),
            finding.line
        )
    } else {
        format!(
            "{}:{}",
            path.display(),
            finding.render().replacen("line ", "", 1)
        )
    }
}

/// Lint every file. `github` prints GitHub Actions annotations.
pub fn check(files: &[std::path::PathBuf], github: bool) -> Result<Report, String> {
    let mut report = Report {
        lines: Vec::new(),
        total: 0,
        hard: 0,
    };
    for path in files {
        let text = fs::read_to_string(path)
            .map_err(|e| format!("could not read {}: {e}", path.display()))?;
        let mut findings = lint(&text);
        // A sibling `<file>.facts` turns on the grounding and length checks.
        let mut facts_name = path.as_os_str().to_os_string();
        facts_name.push(".facts");
        if let Ok(facts) = fs::read_to_string(&facts_name) {
            findings.extend(
                prose_ground::check(&text, &facts)
                    .map_err(|e| format!("{}: {e}", Path::new(&facts_name).display()))?,
            );
        }
        report.total += findings.len();
        if has_hard(&findings) {
            report.hard += findings.iter().filter(|f| f.class.hard()).count();
        }
        report
            .lines
            .extend(findings.iter().map(|f| line_for(path, f, github)));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;

    fn file(tag: &str, text: &str) -> std::path::PathBuf {
        let dir = scratch_dir(tag);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.md");
        fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn a_clean_file_has_no_lines() {
        let path = file("prose-clean", "# Title\n\nThe cache is cold.\n");

        let report = check(&[path], false).unwrap();

        assert!(report.lines.is_empty());
        assert_eq!((report.total, report.hard), (0, 0));
    }

    #[test]
    fn a_finding_prints_as_path_line_message() {
        let path = file("prose-hit", "# Title\n\nLet that sink in.\n");

        let report = check(std::slice::from_ref(&path), false).unwrap();

        assert_eq!(report.hard, 1);
        let expected = format!("{}:3: emphasis crutch", path.display());
        assert!(report.lines[0].starts_with(&expected), "{:?}", report.lines);
    }

    #[test]
    fn github_mode_prints_a_warning_annotation() {
        let path = file("prose-gh", "Let that sink in.\n");

        let report = check(&[path], true).unwrap();

        assert!(report.lines[0].starts_with("::warning file="));
        assert!(report.lines[0].contains(",line=1::"));
    }

    #[test]
    fn a_newline_inside_a_snippet_does_not_split_an_annotation() {
        let path = file(
            "prose-nl",
            "Not because it is slow.\nBecause it is wrong.\n",
        );

        let report = check(&[path], true).unwrap();

        assert_eq!(report.lines.len(), 1, "{:?}", report.lines);
        assert!(!report.lines[0].contains('\n'));
    }

    #[test]
    fn a_soft_finding_counts_but_is_not_hard() {
        let path = file("prose-soft", "It always fails.\n");

        let report = check(&[path], false).unwrap();

        assert_eq!((report.total, report.hard), (1, 0));
    }

    #[test]
    fn a_facts_file_next_to_the_text_turns_on_the_grounding_check() {
        let path = file("prose-facts", "Fixed. The cap is 9.\n");
        let mut facts = path.clone().into_os_string();
        facts.push(".facts");
        fs::write(&facts, "kind: reply\nThe cap is 3.\n").unwrap();

        let report = check(&[path], false).unwrap();

        assert_eq!(report.hard, 1, "{:?}", report.lines);
        assert!(report.lines[0].contains("token not in the facts \"9\""));
    }

    #[test]
    fn a_bad_kind_in_the_facts_file_is_an_error() {
        let path = file("prose-facts-bad", "Fixed.\n");
        let mut facts = path.clone().into_os_string();
        facts.push(".facts");
        fs::write(&facts, "kind: essay\n").unwrap();

        let err = check(&[path], false).err();

        assert!(err.is_some_and(|e| e.contains("unknown kind")));
    }

    #[test]
    fn a_missing_file_is_an_error() {
        let err = check(&[std::path::PathBuf::from("/nonexistent/x.md")], false).err();

        assert!(err.is_some_and(|e| e.contains("could not read")));
    }
}
