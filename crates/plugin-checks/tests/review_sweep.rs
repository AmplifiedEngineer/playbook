// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Structural checks for the verification sweep that every review flow runs
//! before findings are shown or posted.
//!
//! Each file that presents review findings must carry a heading-delimited
//! sweep section holding the three checks (true, label, anchor) and the
//! counts line. The two review commands must also run the sweep before the
//! section that presents or posts. These tests parse headings outside fenced
//! code instead of grepping, so a marker in prose or an example cannot pass
//! for a real section. Model behaviour is not testable here: they cover prose
//! structure only.
//!
//! The same parsing checks when `playbook:writing-style` loads: reviewers
//! return plain findings and never load it, and the review commands load it
//! only at the posting step, after the sweep and outside self mode.

use std::fs;
use std::path::PathBuf;

/// Files that carry the sweep as a section of their own.
const SWEEP_FILES: &[&str] = &[
    "skills/grounding-review/references/orchestrator.md",
    "skills/grounding-review/references/quick-pass.md",
    "skills/grounding-review/references/deep-pass.md",
    "commands/implement.md",
];

/// Reviewer agents that tell their output will be swept.
const SWEPT_AGENTS: &[&str] = &["agents/reviewer.md", "agents/cheap-checker.md"];

/// The phrase a load instruction for the writing skill uses, lowercase.
const WRITING_STYLE_LOAD: &str = "load `playbook:writing-style`";

/// The three checks, as the numbered lead of each item.
const CHECKS: &[&str] = &["1. **True.**", "2. **Label.**", "3. **Anchor.**"];

struct Doc {
    path: String,
    lines: Vec<String>,
}

struct Heading {
    level: usize,
    title: String,
    line: usize,
}

/// A heading and the lines it owns, `start..end` (end exclusive).
struct Section {
    start: usize,
    end: usize,
}

fn load(rel: &str) -> Doc {
    let path = PathBuf::from(env!("PLAYBOOK_PLUGIN_ROOT")).join(rel);
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
    Doc {
        path: rel.to_owned(),
        lines: text.lines().map(str::to_owned).collect(),
    }
}

impl Doc {
    /// Markdown headings outside fenced code blocks, so a `# comment` in a
    /// shell example is not mistaken for a section.
    fn headings(&self) -> Vec<Heading> {
        let mut out = Vec::new();
        let mut in_fence = false;
        for (line, text) in self.lines.iter().enumerate() {
            if text.trim_start().starts_with("```") {
                in_fence = !in_fence;
                continue;
            }
            let level = text.chars().take_while(|&c| c == '#').count();
            if !in_fence && level > 0 && text[level..].starts_with(' ') {
                out.push(Heading {
                    level,
                    title: text[level..].trim().to_owned(),
                    line,
                });
            }
        }
        out
    }

    /// The first section whose heading title satisfies `matches`. A section
    /// ends at the next heading of the same or a shallower level.
    fn section(&self, matches: impl Fn(&str) -> bool) -> Option<Section> {
        let headings = self.headings();
        let idx = headings.iter().position(|h| matches(&h.title))?;
        let end = headings[idx + 1..]
            .iter()
            .find(|h| h.level <= headings[idx].level)
            .map_or(self.lines.len(), |h| h.line);
        Some(Section {
            start: headings[idx].line,
            end,
        })
    }

    fn sweep(&self) -> Option<Section> {
        self.section(|t| t.to_lowercase().contains("verification sweep"))
    }

    fn step(&self, prefix: &str) -> Section {
        self.section(|t| t.starts_with(prefix))
            .unwrap_or_else(|| panic!("{} has no section titled `{prefix}...`", self.path))
    }

    fn text_of(&self, s: &Section) -> String {
        self.lines[s.start..s.end].join("\n")
    }

    /// Indexes of lines in `start..end` that tell the reader to load the
    /// skill. A mention that is negated ("never load", "do not load") or that
    /// merely names the skill does not count.
    fn writing_style_loads(&self, start: usize, end: usize) -> Vec<usize> {
        (start..end)
            .filter(|&i| {
                let line = self.lines[i].to_lowercase();
                line.match_indices(WRITING_STYLE_LOAD).any(|(at, _)| {
                    let before = line[..at].trim_end();
                    !before.ends_with("never") && !before.ends_with("not")
                })
            })
            .collect()
    }

    /// Index of the first line at or after `from` that contains `needle`.
    fn find_line(&self, from: usize, needle: &str) -> Option<usize> {
        (from..self.lines.len()).find(|&i| self.lines[i].contains(needle))
    }
}

#[test]
fn every_review_flow_has_a_sweep_section_with_the_three_checks() {
    for rel in SWEEP_FILES {
        // Arrange
        let doc = load(rel);

        // Act
        let sweep = doc
            .sweep()
            .unwrap_or_else(|| panic!("{rel} has no verification sweep section"));
        let text = doc.text_of(&sweep);

        // Assert
        for check in CHECKS {
            assert!(text.contains(check), "{rel}: sweep lacks `{check}`");
        }
        assert!(
            text.contains("kept, dropped, relabelled"),
            "{rel}: sweep does not say it reports kept, dropped, relabelled and moved counts"
        );
        assert!(
            text.contains("[unverified]"),
            "{rel}: sweep does not say what happens to an `[unverified]` finding"
        );
    }
}

#[test]
fn quick_review_sweeps_before_it_relays_or_posts() {
    // Arrange
    let doc = load("skills/grounding-review/references/quick-pass.md");

    // Act
    let contract = doc.step("Step 3:");
    let sweep = doc.sweep().expect("quick-pass has a sweep section");
    let posting = doc.step("Step 4:");

    // Assert
    assert!(
        contract.start < sweep.start && sweep.start < posting.start,
        "quick-pass: the sweep must sit after the report contract and before posting"
    );
    let text = doc.lines.join("\n");
    assert!(
        !text.contains("Relay the report to the user unchanged"),
        "quick-pass still relays the reviewer report unchanged"
    );
    assert!(
        !text.contains("the orchestrator does NOT re-read source files"),
        "quick-pass still forbids the orchestrator from re-reading cited lines"
    );
}

#[test]
fn deep_review_sweeps_before_it_presents_or_posts() {
    // Arrange
    let doc = load("skills/grounding-review/references/deep-pass.md");

    // Act
    let consolidate = doc.step("Step 4:");
    let sweep = doc.sweep().expect("deep-pass has a sweep section");
    let present = doc.step("Step 5:");
    let posting = doc.step("Step 6:");

    // Assert
    assert!(
        consolidate.start < sweep.start,
        "deep-pass: the sweep must follow consolidation"
    );
    assert!(
        sweep.start < present.start && sweep.start < posting.start,
        "deep-pass: the sweep must come before presenting or posting"
    );
}

#[test]
fn implement_sweeps_inside_step_9_before_acting_on_findings() {
    // Arrange
    let doc = load("commands/implement.md");

    // Act
    let step9 = doc.step("Step 9:");
    let sweep = doc.sweep().expect("implement has a sweep section");
    let act = doc
        .section(|t| t.starts_with("Fix, open the PRs"))
        .expect("implement has a section that applies the fixes");

    // Assert
    assert!(
        step9.start < sweep.start && sweep.end <= step9.end,
        "implement: the sweep must live inside Step 9"
    );
    assert!(
        sweep.start < act.start,
        "implement: the sweep must come before the fixes are applied"
    );
}

#[test]
fn every_review_flow_recomputes_the_verdict_from_the_swept_list() {
    for rel in SWEEP_FILES {
        // Arrange
        let doc = load(rel);

        // Act
        let text = doc.lines.join("\n");

        // Assert
        assert!(
            text.contains(
                "recompute the verdict, confidence and finding order from the swept list"
            ),
            "{rel}: does not recompute the verdict, confidence and order after the sweep"
        );
    }
}

#[test]
fn quick_review_sweep_traces_but_never_runs_pr_code() {
    // Arrange
    let doc = load("skills/grounding-review/references/quick-pass.md");

    // Act
    let sweep = doc.sweep().expect("quick-pass has a sweep section");
    let text = doc.text_of(&sweep);

    // Assert
    assert!(
        text.contains("Trace the failure scenario"),
        "quick-pass: the sweep does not say to trace the failure scenario"
    );
    assert!(
        !text.contains("or run"),
        "quick-pass: the sweep must not tell the orchestrator to run PR code"
    );
    assert!(
        text.contains("never run PR code"),
        "quick-pass: the sweep does not forbid running PR code"
    );
}

#[test]
fn implement_step_9_sweeps_before_it_fixes() {
    // Arrange
    let doc = load("commands/implement.md");

    // Act
    let text = doc.text_of(&doc.step("Step 9:"));
    let sweep_at = text.find("Verification sweep");
    let fix_at = text.find("Apply the fixes");

    // Assert
    let (sweep_at, fix_at) = (
        sweep_at.expect("implement Step 9 does not mention the verification sweep"),
        fix_at.expect("implement Step 9 has no fix instruction"),
    );
    assert!(
        sweep_at < fix_at,
        "implement Step 9: the sweep must come before the fix instruction"
    );
}

#[test]
fn reviewer_agents_say_their_findings_are_swept() {
    for rel in SWEPT_AGENTS {
        // Arrange
        let doc = load(rel);

        // Act
        let text = doc.lines.join("\n");

        // Assert
        assert!(
            text.contains("The orchestrator sweeps every finding you return"),
            "{rel}: does not tell the reviewer its findings will be swept"
        );
    }
}

#[test]
fn sweep_detector_finds_nothing_in_a_file_without_one() {
    // Arrange: a fenced heading is not a section, and nothing else matches.
    let doc = Doc {
        path: "fixture".to_owned(),
        lines: vec![
            "## Step 3: Report".to_owned(),
            "```".to_owned(),
            "## Verification sweep".to_owned(),
            "```".to_owned(),
        ],
    };

    // Act
    let sweep = doc.sweep();

    // Assert
    assert!(sweep.is_none(), "a fenced heading must not count");
}

fn draft_section(doc: &Doc) -> Section {
    doc.section(|t| t.starts_with("Draft the comments"))
        .unwrap_or_else(|| panic!("{} has no `Draft the comments` section", doc.path))
}

#[test]
fn reviewer_agents_never_load_writing_style() {
    for rel in SWEPT_AGENTS {
        // Arrange
        let doc = load(rel);

        // Act
        let loads = doc.writing_style_loads(0, doc.lines.len());
        let text = doc.lines.join("\n");

        // Assert
        assert!(loads.is_empty(), "{rel}: loads writing-style: {loads:?}");
        assert!(
            text.contains("no `Post:` block"),
            "{rel}: does not say findings carry no Post block"
        );
    }
}

#[test]
fn reviewer_prompt_sections_never_load_writing_style() {
    // Arrange: the sections each command hands to a reviewer prompt.
    let quick = load("skills/grounding-review/references/quick-pass.md");
    let deep = load("skills/grounding-review/references/deep-pass.md");
    let implement = load("commands/implement.md");
    let step9 = implement.step("Step 9:");
    let fixes = implement
        .section(|t| t.starts_with("Fix, open the PRs"))
        .expect("implement has a section that applies the fixes");
    let spans = [
        (&quick, quick.step("Voice rules")),
        (&quick, quick.step("Step 2:")),
        (&deep, deep.step("Voice rules")),
        (&deep, deep.step("Step 3:")),
        (
            &implement,
            Section {
                start: step9.start,
                end: fixes.start,
            },
        ),
    ];

    for (doc, span) in spans {
        // Act
        let loads = doc.writing_style_loads(span.start, span.end);

        // Assert
        assert!(
            loads.is_empty(),
            "{}: a reviewer prompt section loads writing-style: {loads:?}",
            doc.path
        );
    }
}

#[test]
fn quick_review_loads_writing_style_once_after_the_sweep_outside_self_mode() {
    // Arrange
    let doc = load("skills/grounding-review/references/quick-pass.md");

    // Act
    let sweep = doc.sweep().expect("quick-pass has a sweep section");
    let posting = doc.step("Step 4:");
    let draft = draft_section(&doc);
    let loads = doc.writing_style_loads(0, doc.lines.len());
    let self_stop = doc
        .find_line(posting.start, "If `SELF_MODE` is true")
        .expect("Step 4 stops in self mode");
    let show = doc
        .find_line(draft.start, "Relay the swept report with each draft")
        .expect("quick-pass shows the drafts");
    let ask = doc
        .find_line(draft.end, "Post which findings")
        .expect("quick-pass asks which findings to post");

    // Assert
    assert_eq!(
        loads.len(),
        1,
        "quick-pass must load writing-style once: {loads:?}"
    );
    let load = loads[0];
    assert!(
        sweep.end <= load,
        "quick-pass: the load must sit after the sweep"
    );
    assert!(
        posting.start < self_stop && self_stop < load && load < posting.end,
        "quick-pass: the load must sit inside Step 4, after the self mode stop"
    );
    assert!(
        draft.start <= load && load < show && show < ask,
        "quick-pass: the load must come before the drafts are shown and the post question"
    );
}

#[test]
fn deep_review_loads_writing_style_once_after_the_sweep_outside_self_mode() {
    // Arrange
    let doc = load("skills/grounding-review/references/deep-pass.md");

    // Act
    let sweep = doc.sweep().expect("deep-pass has a sweep section");
    let present = doc.step("Step 5:");
    let posting = doc.step("Step 6:");
    let draft = draft_section(&doc);
    let loads = doc.writing_style_loads(0, doc.lines.len());
    let show = doc
        .find_line(draft.start, "Present the report with each draft")
        .expect("deep-pass shows the drafts");
    let ask = doc
        .find_line(posting.start, "Post which findings")
        .expect("deep-pass asks which findings to post");

    // Assert
    assert_eq!(
        loads.len(),
        1,
        "deep-pass must load writing-style once: {loads:?}"
    );
    let load = loads[0];
    let line = doc.lines[load].to_lowercase();
    assert!(
        sweep.end <= load,
        "deep-pass: the load must sit after the sweep"
    );
    assert!(
        present.start < draft.start && draft.start <= load && load < draft.end,
        "deep-pass: the load must sit in the draft section of the presenting step"
    );
    assert!(
        line.find("skip this in `self_mode`")
            .is_some_and(|skip| skip < line.find(WRITING_STYLE_LOAD).unwrap_or(0)),
        "deep-pass: the load must come after the self mode skip"
    );
    assert!(
        load < show && show < posting.start && posting.start < ask,
        "deep-pass: the load must come before the drafts are shown and the post question"
    );
}

#[test]
fn deep_review_loads_skills_only_after_triage_settles_the_lenses() {
    // Arrange
    let doc = load("skills/grounding-review/references/deep-pass.md");

    // Act
    let triage = doc.step("Step 2d:");
    let load_step = doc.step("Step 2e:");

    // Assert
    assert!(
        triage.start < load_step.start,
        "deep-pass: Step 2e must start after the Step 2d triage"
    );
}

#[test]
fn quick_review_invokes_grounding_review_after_self_mode_is_settled() {
    // Arrange
    let doc = load("skills/grounding-review/references/quick-pass.md");

    // Act
    let resolve = doc.step("Step 1:");
    let delegate = doc.step("Step 2:");
    let invoke = doc
        .find_line(delegate.start, "invokes `playbook:grounding-review`")
        .expect("Step 2 invokes grounding-review");

    // Assert
    assert!(
        doc.text_of(&resolve).contains("`SELF_MODE`"),
        "quick-pass: Step 1 does not settle SELF_MODE"
    );
    assert!(
        resolve.end <= invoke,
        "quick-pass: grounding-review is invoked before Step 1 settles SELF_MODE"
    );
}

#[test]
fn posting_steps_post_the_previewed_drafts_as_they_are() {
    for rel in [
        "skills/grounding-review/references/quick-pass.md",
        "skills/grounding-review/references/deep-pass.md",
    ] {
        // Arrange
        let doc = load(rel);

        // Act
        let draft_text = doc.text_of(&draft_section(&doc));
        let build = doc
            .lines
            .iter()
            .find(|l| l.contains("previewed `Draft:` block verbatim"))
            .unwrap_or_else(|| panic!("{rel}: the payload is not built from the previewed drafts"));

        // Assert
        assert!(
            draft_text.contains("Nothing is rewritten between this preview and the post"),
            "{rel}: does not forbid rewriting between preview and post"
        );
        assert!(
            draft_text
                .contains("redraft it with `playbook:writing-style` and show the new preview"),
            "{rel}: does not redraft and re-preview on a requested change"
        );
        assert!(
            build.contains("never a rewording"),
            "{rel}: the post step may reword the previewed drafts"
        );
        assert!(
            !doc.lines.join("\n").contains("`Post:` block verbatim"),
            "{rel}: still builds comments from a reviewer Post block"
        );
    }
}

#[test]
fn grounding_review_loads_writing_style_for_human_text_only() {
    // Arrange
    let skill = load("skills/grounding-review/SKILL.md");
    let doc = load("skills/grounding-review/references/orchestrator.md");

    // Act
    let text = skill.lines.join("\n");
    let format = doc
        .section(|t| t == "Review Report Format")
        .expect("report format");
    let format_text = doc.text_of(&format);
    let sweep_text = doc.text_of(&doc.sweep().expect("sweep section"));

    // Assert
    assert!(
        !text.contains("MUST load the `playbook:writing-style` skill alongside"),
        "grounding-review still requires writing-style alongside it"
    );
    assert!(
        text.contains("loads the writing style skill")
            && text.contains("not to produce or check findings"),
        "grounding-review does not scope the writing-style load to human text"
    );
    for (name, part) in [("report format", &format_text), ("sweep", &sweep_text)] {
        assert!(
            !part.contains("Post:\n") && !part.contains("`Post:` block:"),
            "grounding-review {name} still defines a Post block"
        );
    }
}

#[test]
fn sweeps_no_longer_update_post_blocks() {
    for rel in SWEEP_FILES {
        // Arrange
        let doc = load(rel);

        // Act
        let text = doc.text_of(&doc.sweep().expect("sweep section"));

        // Assert
        assert!(
            !text.contains("`Post:`"),
            "{rel}: the sweep still touches Post blocks"
        );
    }
}
