// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The size budget for plugin markdown: a main file (a command or a
//! `SKILL.md`) stays within 200 lines and 20,000 bytes, a reference file
//! within 300 lines and 24,000 bytes, and a file over its limit has a row that
//! only goes down. The pure checks are in `support/size_budget.rs`.

#[path = "support/markdown.rs"]
mod markdown;
#[path = "support/markdown_refs.rs"]
mod markdown_refs;
#[path = "support/rename_table.rs"]
mod rename_table;
#[path = "support/size_budget.rs"]
mod size_budget;

use size_budget::*;
use std::fs;
use std::path::{Path, PathBuf};

/// The least files of each kind the real walk measures, so an empty walk fails.
const MIN_COMMANDS: usize = 14;
const MIN_SKILLS: usize = 11;
const MIN_REFERENCES: usize = 40;

const ALLOWLIST: &str = include_str!("size_budget_allowlist.txt");
const WITHOUT_BINARY: &str = "skills/delegating-subagents/references/without-binary.md";

fn plugin_root() -> PathBuf {
    PathBuf::from(env!("PLAYBOOK_PLUGIN_ROOT"))
}

fn rows() -> Vec<Row> {
    parse_rows(ALLOWLIST).expect("the allowlist parses")
}

fn rel(path: &Path) -> String {
    path.strip_prefix(plugin_root())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Every plugin markdown file under commands/ and skills/, with its budget kind.
fn plugin_files() -> Vec<(String, FileKind)> {
    let mut all = Vec::new();
    for dir in ["commands", "skills"] {
        markdown::markdown_files(&plugin_root().join(dir), &mut all);
    }
    let mut out: Vec<(String, FileKind)> = all
        .iter()
        .map(|p| {
            let r = rel(p);
            let main = (r.starts_with("commands/") && r.matches('/').count() == 1)
                || (r.starts_with("skills/")
                    && r.ends_with("/SKILL.md")
                    && r.matches('/').count() == 2);
            (
                r,
                if main {
                    FileKind::Main
                } else {
                    FileKind::Reference
                },
            )
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// The old and new plugin paths of every rename, so a target follows it.
fn renamed_paths(renames: &[rename_table::Rename]) -> Vec<(String, String)> {
    renames
        .iter()
        .map(|r| match r.kind {
            rename_table::Kind::Command => (
                format!("commands/{}.md", r.old),
                format!("commands/{}.md", r.new),
            ),
            rename_table::Kind::Skill => (
                format!("skills/{}/SKILL.md", r.old),
                format!("skills/{}/SKILL.md", r.new),
            ),
        })
        .collect()
}

fn size_row(path: &str, lines: usize, bytes: usize) -> Row {
    Row::Size {
        path: path.to_owned(),
        max_lines: lines,
        max_bytes: bytes,
    }
}

fn fallback_row(path: &str, max: usize) -> Row {
    Row::Fallbacks {
        path: path.to_owned(),
        max,
    }
}

#[test]
fn a_main_file_at_the_budget_passes_and_one_over_fails() {
    // Arrange
    let cases = [
        (200, 20_000, true),
        (201, 20_000, false),
        (200, 20_001, false),
        (150, 20_001, false),
        (201, 10_000, false),
    ];
    let mut problems = Vec::new();

    // Act
    for (lines, bytes, passes) in cases {
        let found = check_file(FileKind::Main, "commands/x.md", lines, bytes, 0, &[]);
        if found.is_empty() != passes {
            problems.push(format!(
                "{lines} lines, {bytes} bytes: passes should be {passes}, got {found:?}"
            ));
        }
    }

    // Assert
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn a_reference_file_at_the_budget_passes_and_one_over_fails() {
    // Arrange
    let cases = [
        (300, 24_000, true),
        (301, 24_000, false),
        (300, 24_001, false),
        (301, 10_000, false),
        (150, 24_001, false),
    ];
    let mut problems = Vec::new();

    // Act
    for (lines, bytes, passes) in cases {
        let found = check_file(
            FileKind::Reference,
            "skills/s/references/x.md",
            lines,
            bytes,
            0,
            &[],
        );
        if found.is_empty() != passes {
            problems.push(format!(
                "{lines} lines, {bytes} bytes: passes should be {passes}, got {found:?}"
            ));
        }
    }

    // Assert
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn a_size_row_allows_up_to_its_ceiling_and_not_beyond() {
    // Arrange
    let rows = [size_row("commands/x.md", 250, 30_000)];
    let cases = [
        (250, 30_000, true),
        (251, 30_000, false),
        (250, 30_001, false),
    ];
    let mut problems = Vec::new();

    // Act
    for (lines, bytes, passes) in cases {
        let found = check_file(FileKind::Main, "commands/x.md", lines, bytes, 0, &rows);
        if found.is_empty() != passes {
            problems.push(format!(
                "{lines} lines, {bytes} bytes: passes should be {passes}, got {found:?}"
            ));
        }
    }

    // Assert
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn a_size_row_for_a_file_within_budget_fails() {
    // Arrange
    let main_row = [size_row("commands/x.md", 250, 30_000)];
    let ref_row = [size_row("skills/s/references/quick-pass.md", 253, 23_363)];

    // Act
    let main = check_file(FileKind::Main, "commands/x.md", 200, 20_000, 0, &main_row);
    let reference = check_file(
        FileKind::Reference,
        "skills/s/references/quick-pass.md",
        253,
        23_363,
        0,
        &ref_row,
    );

    // Assert
    assert!(main.iter().any(|p| p.contains("within budget")), "{main:?}");
    assert!(
        reference.iter().any(|p| p.contains("within budget")),
        "{reference:?}"
    );
}

#[test]
fn a_fallback_row_allows_up_to_its_count_and_a_zero_count_row_fails() {
    // Arrange
    let main_rows = [fallback_row("commands/x.md", 5)];
    let ref_rows = [fallback_row("skills/s/references/y.md", 2)];

    // Act
    let at_max = check_file(FileKind::Main, "commands/x.md", 10, 100, 5, &main_rows);
    let over = check_file(FileKind::Main, "commands/x.md", 10, 100, 6, &main_rows);
    let zero = check_file(FileKind::Main, "commands/x.md", 10, 100, 0, &main_rows);
    let ref_at_max = check_file(
        FileKind::Reference,
        "skills/s/references/y.md",
        10,
        100,
        2,
        &ref_rows,
    );
    let ref_zero = check_file(
        FileKind::Reference,
        "skills/s/references/y.md",
        10,
        100,
        0,
        &ref_rows,
    );

    // Assert
    assert!(at_max.is_empty(), "{at_max:?}");
    assert!(!over.is_empty(), "{over:?}");
    assert!(!zero.is_empty(), "{zero:?}");
    assert!(ref_at_max.is_empty(), "{ref_at_max:?}");
    assert!(!ref_zero.is_empty(), "{ref_zero:?}");
}

#[test]
fn a_main_file_with_a_fallback_and_no_row_fails() {
    // Arrange
    let none: [Row; 0] = [];

    // Act
    let with_one = check_file(FileKind::Main, "commands/x.md", 10, 100, 1, &none);
    let with_zero = check_file(FileKind::Main, "commands/x.md", 10, 100, 0, &none);

    // Assert
    assert!(!with_one.is_empty(), "{with_one:?}");
    assert!(with_zero.is_empty(), "{with_zero:?}");
}

#[test]
fn every_plugin_file_is_within_budget_or_listed() {
    // Arrange
    let rows = rows();
    let files = plugin_files();
    let existing: Vec<String> = files.iter().map(|(p, _)| p.clone()).collect();
    let mut problems = check_rows_exist(&rows, &existing);

    // Act
    for (path, kind) in &files {
        let text = fs::read_to_string(plugin_root().join(path)).expect("readable markdown");
        problems.extend(check_file(
            *kind,
            path,
            text.lines().count(),
            text.len(),
            count_fallbacks(&text),
            &rows,
        ));
    }
    let count = |kind: FileKind| files.iter().filter(|(_, k)| *k == kind).count();
    let commands = files
        .iter()
        .filter(|(p, k)| *k == FileKind::Main && p.starts_with("commands/"))
        .count();
    let skills = count(FileKind::Main) - commands;

    // Assert
    assert!(
        commands >= MIN_COMMANDS,
        "only {commands} command files measured"
    );
    assert!(
        skills >= MIN_SKILLS,
        "only {skills} SKILL.md files measured"
    );
    assert!(
        count(FileKind::Reference) >= MIN_REFERENCES,
        "only {} reference files measured",
        count(FileKind::Reference)
    );
    assert!(
        existing.iter().any(|p| p == "commands/implement.md"),
        "commands/implement.md is missing from the walk"
    );
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn the_allowlist_parses_and_rejects_a_bad_row() {
    // Arrange
    let bad = [
        "size commands/x.md 10",
        "fallbacks commands/x.md many",
        "keep commands/x.md 1",
    ];

    // Act
    let results: Vec<_> = bad.iter().map(|b| parse_rows(b)).collect();

    // Assert
    assert!(results.iter().all(Result::is_err), "{results:?}");
    assert_eq!(
        rows()
            .iter()
            .filter(|r| matches!(r, Row::Size { .. }))
            .count(),
        5
    );
}

#[test]
fn slimmed_files_hold_no_gh_api_or_jq_block() {
    // Arrange
    let with_gh = "text\n```bash\ngh api repos/x/y\n```\n";
    let with_jq = "```bash\ngh pr view | jq .x\n```\n";
    let prose_only = "Run gh api by hand, or jq, in prose.\n```bash\nplaybook pr status\n```\n";
    let rows = rows();

    // Act
    let real: Vec<String> = SLIMMED
        .iter()
        .filter_map(|p| match fs::read_to_string(plugin_root().join(p)) {
            Err(_) => Some(format!("{p}: SLIMMED names a missing file")),
            Ok(t) => has_gh_api_or_jq_block(&t).then(|| format!("{p}: holds a gh api or jq block")),
        })
        .collect();
    let targets = check_slimmed_targets(
        SLIMMED_TARGETS,
        SLIMMED,
        &rows,
        MOVED,
        &renamed_paths(rename_table::RENAMES),
    );

    // Assert
    assert!(has_gh_api_or_jq_block(with_gh));
    assert!(has_gh_api_or_jq_block(with_jq));
    assert!(!has_gh_api_or_jq_block(prose_only));
    assert_eq!(SLIMMED_TARGETS.len(), 15);
    assert!(real.is_empty(), "{real:#?}");
    assert!(targets.is_empty(), "{targets:#?}");
}

#[test]
fn a_slimmed_target_needs_a_row_or_a_slimmed_entry_wherever_it_moved() {
    // Arrange
    let rows = [size_row("skills/g/references/deep-pass.md", 314, 32_224)];
    let moved = [(
        "commands/deep-review.md",
        "skills/g/references/deep-pass.md",
    )];
    let renamed = [("commands/old.md".to_owned(), "commands/new.md".to_owned())];

    // Act
    let missing = check_slimmed_targets(&["commands/a.md"], &[], &[], &[], &[]);
    let still_has_row = check_slimmed_targets(
        &["commands/a.md"],
        &[],
        &[fallback_row("commands/a.md", 1)],
        &[],
        &[],
    );
    let in_slimmed = check_slimmed_targets(&["commands/a.md"], &["commands/a.md"], &[], &[], &[]);
    let moved_ok = check_slimmed_targets(&["commands/deep-review.md"], &[], &rows, &moved, &[]);
    let renamed_missing = check_slimmed_targets(
        &["commands/old.md"],
        &["commands/old.md"],
        &[],
        &[],
        &renamed,
    );
    let big_ref = check_file(
        FileKind::Reference,
        "skills/g/references/deep-pass.md",
        314,
        32_224,
        0,
        &rows,
    );
    let bigger = check_file(
        FileKind::Reference,
        "skills/g/references/deep-pass.md",
        315,
        32_224,
        0,
        &rows,
    );
    let stale_row = check_rows_exist(
        &[size_row("commands/deep-review.md", 314, 32_224)],
        &["skills/g/references/deep-pass.md".to_owned()],
    );

    // Assert
    assert_eq!(missing.len(), 1, "{missing:?}");
    assert!(still_has_row.is_empty(), "{still_has_row:?}");
    assert!(in_slimmed.is_empty(), "{in_slimmed:?}");
    assert!(moved_ok.is_empty(), "{moved_ok:?}");
    assert_eq!(renamed_missing.len(), 1, "{renamed_missing:?}");
    assert!(big_ref.is_empty(), "{big_ref:?}");
    assert!(!bigger.is_empty(), "{bigger:?}");
    assert_eq!(stale_row.len(), 1, "{stale_row:?}");
}

#[test]
fn a_rename_maps_to_the_paths_of_its_kind() {
    // Arrange
    let renames = [
        rename_table::Rename {
            kind: rename_table::Kind::Command,
            old: "a",
            new: "b",
        },
        rename_table::Rename {
            kind: rename_table::Kind::Skill,
            old: "c",
            new: "d",
        },
    ];

    // Act
    let paths = renamed_paths(&renames);

    // Assert
    assert_eq!(
        paths[0],
        ("commands/a.md".to_owned(), "commands/b.md".to_owned())
    );
    assert_eq!(
        paths[1],
        (
            "skills/c/SKILL.md".to_owned(),
            "skills/d/SKILL.md".to_owned()
        )
    );
}

#[test]
fn each_intent_section_appears_once_in_without_binary() {
    // Arrange
    let duplicate = "## run-mode\ntext\n## run-mode\ntext\n";
    let good = "## run-mode\ntext\n## config-get\ntext\n";
    let pointer = |t: &str| vec![("commands/x.md".to_owned(), t.to_owned())];
    let reference =
        fs::read_to_string(plugin_root().join(WITHOUT_BINARY)).expect("without-binary.md exists");
    let mut pointers = Vec::new();
    for (path, _) in plugin_files() {
        let text = fs::read_to_string(plugin_root().join(&path)).expect("readable markdown");
        pointers.push((path, text));
    }

    // Act
    let dup = check_anchors(duplicate, &[]);
    let dangling = check_anchors(
        good,
        &pointer("follow `skills/delegating-subagents/references/without-binary.md#absent`"),
    );
    let fine = check_anchors(
        good,
        &pointer("follow `references/without-binary.md#config-get`"),
    );
    let real = check_anchors(&reference, &pointers);

    // Assert
    assert_eq!(dup.len(), 1, "{dup:?}");
    assert_eq!(dangling.len(), 1, "{dangling:?}");
    assert!(fine.is_empty(), "{fine:?}");
    assert!(real.is_empty(), "{real:#?}");
    assert!(reference.lines().count() <= size_budget::REF_MAX_LINES);
    assert!(reference.len() <= size_budget::REF_MAX_BYTES);
}

#[test]
fn command_text_with_refs_returns_the_main_file_and_its_references() {
    // Arrange
    let read = |p: &str| match p {
        "commands/x.md" => Some("main text, see skills/s/references/r.md".to_owned()),
        "skills/s/references/r.md" => Some("only in the reference".to_owned()),
        "skills/s/SKILL.md" => {
            Some("skill text, see references/own.md and references/none.md".to_owned())
        }
        "skills/s/references/own.md" => Some("own reference text".to_owned()),
        _ => None,
    };

    // Act
    let command = markdown_refs::text_with_refs("commands/x.md", &read);
    let skill = markdown_refs::text_with_refs("skills/s/SKILL.md", &read);
    let real = markdown_refs::command_text_with_refs("skills/delegating-subagents/SKILL.md");

    // Assert
    assert!(
        command.contains("main text") && command.contains("only in the reference"),
        "{command}"
    );
    assert!(skill.contains("own reference text"), "{skill}");
    assert!(
        real.contains("playbook:delegating-subagents") || real.len() > 1000,
        "the real read looks empty"
    );
}
