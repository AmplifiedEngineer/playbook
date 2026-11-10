// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The Step 0, ordering and posting checks of `commands_auto_parity.rs`.

use super::ap::{is_ask, require_section, CommandFile, Section};
use regex::Regex;
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

impl CommandFile {
    /// The `Step 0` section, at whatever heading level the file uses.
    pub(super) fn step0(&self) -> Option<Section> {
        self.section(|t| t.starts_with("Step 0"))
    }

    /// The section that documents Step `n`, e.g. `Step 6: Present the design`.
    pub(super) fn step(&self, n: &str) -> Option<Section> {
        let prefix = format!("Step {n}");
        self.section(|t| {
            t.strip_prefix(&prefix)
                .is_some_and(|rest| !rest.starts_with(|c: char| c.is_ascii_digit() || c == '.'))
        })
    }

    /// The named auto-path subsection: a heading titled `Auto path`,
    /// optionally followed by a qualifier. It holds every instruction that
    /// applies only when the command runs unattended.
    pub(super) fn auto_path(&self) -> Option<Section> {
        self.section(|t| t.to_lowercase().starts_with("auto path"))
    }

    /// Blank-line separated blocks inside `s`, each with its first line
    /// index. A sentence wrapped over several lines stays in one block, so
    /// checks on a block do not depend on where the prose was wrapped.
    pub(super) fn paragraphs(&self, s: &Section) -> Vec<(usize, String)> {
        let mut out: Vec<(usize, String)> = Vec::new();
        let mut open = false;
        for i in s.start..s.end {
            let line = &self.lines[i];
            if line.trim().is_empty() {
                open = false;
            } else if open {
                let last = out.last_mut().expect("an open block exists");
                last.1.push('\n');
                last.1.push_str(line);
            } else {
                out.push((i, line.clone()));
                open = true;
            }
        }
        out
    }

    /// First body line outside `skip` that the ask detector matches.
    pub(super) fn first_ask_outside(&self, skip: Option<&Section>) -> Option<(usize, &str)> {
        self.lines
            .iter()
            .enumerate()
            .skip(self.body_start)
            .filter(|(i, _)| skip.is_none_or(|s| !(s.start..s.end).contains(i)))
            .find(|(_, l)| is_ask(l))
            .map(|(i, l)| (i, l.as_str()))
    }
}

/// Step 0 reads the run mode through the `run-mode` or `mode-status` intent,
/// (the older call forms still count).
pub(super) fn reads_mode(text: &str) -> bool {
    text.contains("playbook mode status")
        || text.contains("playbook run-context")
        || text.contains("Intent: run-mode")
        || text.contains("Intent: mode-status")
        || text.contains("references/run-context.md")
}

/// The section that creates and submits the GitHub review.
pub(super) fn posting_section(file: &CommandFile) -> Section {
    require_section(
        file,
        file.section(|t| t.to_lowercase().contains("orchestrate posting")),
        "`Orchestrate posting`",
    )
}

/// A shell line that calls the GitHub API to create or submit a review.
pub(super) fn is_post_call(line: &str) -> bool {
    static POST: OnceLock<Regex> = OnceLock::new();
    POST.get_or_init(|| Regex::new(r"gh api[^\n]*(-X POST|/reviews)").expect("post pattern"))
        .is_match(line)
}

/// The two review passes `/playbook:review` reads. They post a pending review,
/// so the posting rules apply to their files. Auto mode does not skip the post
/// question; only `--self` and `--no-post` do.
pub(super) const PASSES: [&str; 2] = ["quick-pass", "deep-pass"];

/// A pass file under `skills/grounding-review/references/`. It has no
/// frontmatter, so its body starts at the first line.
pub(super) fn load_pass(name: &str) -> CommandFile {
    let path = PathBuf::from(env!("PLAYBOOK_PLUGIN_ROOT"))
        .join("skills/grounding-review/references")
        .join(format!("{name}.md"));
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
    CommandFile {
        name: name.to_owned(),
        lines: text.lines().map(str::to_owned).collect(),
        body_start: 0,
    }
}
