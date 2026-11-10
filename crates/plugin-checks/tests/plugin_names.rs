// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The plugin name guard: the naming rule, removed names and leftover old
//! names. The pure checks and the registry are in `support/renames.rs`; each
//! runs on literals first and on the real tree second.

#[path = "support/rename_table.rs"]
mod rename_table;
#[path = "support/renames.rs"]
mod renames;

use rename_table::*;
use renames::*;
use std::fs;
use std::path::{Path, PathBuf};

/// The least command files the real tree holds (the count after the review
/// merge and the finish fold).
const MIN_COMMAND_FILES: usize = 14;
/// The least skill folders the real tree holds.
const MIN_SKILL_FOLDERS: usize = 11;
/// The least files the leftover scan reads. A broken scan path reads none.
const MIN_SCANNED_FILES: usize = 600;
/// The frontmatter line that hides a skill from the slash menu.
const HIDE_LINE: &str = "user-invocable: false";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("PLAYBOOK_REPO_ROOT"))
}

fn plugin_root() -> PathBuf {
    PathBuf::from(env!("PLAYBOOK_PLUGIN_ROOT"))
}

fn names(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

/// The command file stems, sorted.
fn command_names() -> Vec<String> {
    let mut out: Vec<String> = fs::read_dir(plugin_root().join("commands"))
        .expect("commands/ is readable")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect();
    out.sort();
    out
}

/// The skill folders with whether the skill is visible in the slash menu.
fn skill_folders() -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = fs::read_dir(plugin_root().join("skills"))
        .expect("skills/ is readable")
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| {
            let text = fs::read_to_string(e.path().join("SKILL.md")).unwrap_or_default();
            let front = text.split("\n---\n").next().unwrap_or_default();
            let hidden = front.lines().any(|l| l.trim() == HIDE_LINE);
            (e.file_name().to_string_lossy().into_owned(), !hidden)
        })
        .collect();
    out.sort();
    out
}

fn skill_names() -> Vec<String> {
    skill_folders().into_iter().map(|(n, _)| n).collect()
}

/// Every regular file the leftover scan reads, as (path from the root, text).
fn scanned_files() -> Vec<(String, String)> {
    let root = repo_root();
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = ["plugins", "seed", "docs"]
        .iter()
        .map(|d| root.join(d))
        .collect();
    for krate in fs::read_dir(root.join("crates"))
        .expect("crates/ is readable")
        .filter_map(Result::ok)
    {
        stack.push(krate.path().join("src"));
    }
    stack.push(root.join("README.md"));
    stack.push(root.join("CONTRIBUTING.md"));
    while let Some(path) = stack.pop() {
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            stack.extend(
                fs::read_dir(&path)
                    .into_iter()
                    .flatten()
                    .filter_map(Result::ok)
                    .map(|e| e.path()),
            );
        } else if meta.is_file() {
            let rel = path.strip_prefix(&root).unwrap_or(Path::new(""));
            let text = String::from_utf8_lossy(&fs::read(&path).unwrap_or_default()).into_owned();
            out.push((rel.to_string_lossy().replace('\\', "/"), text));
        }
    }
    out.sort();
    out
}

fn as_scanned(files: &[(String, String)]) -> Vec<ScannedFile<'_>> {
    files
        .iter()
        .map(|(p, t)| ScannedFile { path: p, text: t })
        .collect()
}

#[test]
fn a_visible_name_is_one_word_or_a_group_and_a_word() {
    // Arrange
    let passing = ["commit", "pr-create", "handoff-save", "review"];
    let failing = [
        "create-pull-request",
        "pr-create-draft",
        "deep-review",
        "Commit",
        "pr_create",
        "",
        "pr-",
        "-pr",
        "pr--create",
        "review-deep",
    ];
    let mut problems = Vec::new();

    // Act
    for name in passing {
        if let Some(why) = check_name(name) {
            problems.push(format!("`{name}` should pass: {why}"));
        }
    }
    for name in failing {
        match check_name(name) {
            None => problems.push(format!("`{name}` should fail")),
            Some(why) if !why.contains("name rule") => {
                problems.push(format!("`{name}` fails without naming the rule: {why}"));
            }
            Some(_) => {}
        }
    }

    // Assert
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn every_visible_command_and_public_skill_follows_the_rule_or_is_listed() {
    // Arrange
    let commands = command_names();
    let skills = skill_folders();

    // Act
    let visible_skills: Vec<String> = skills
        .iter()
        .filter(|(_, v)| *v)
        .map(|(n, _)| n.clone())
        .collect();
    let visible: Vec<String> = commands.iter().chain(&visible_skills).cloned().collect();
    let problems = check_visible_names(&visible, LEGACY_NAMES);

    // Assert
    assert!(
        commands.len() >= MIN_COMMAND_FILES,
        "the command walk looks broken: {commands:?}"
    );
    assert!(
        skills.len() >= MIN_SKILL_FOLDERS,
        "the skill walk looks broken: {skills:?}"
    );
    assert!(
        commands.iter().any(|c| c == "plan"),
        "commands/plan.md is missing from the walk"
    );
    assert!(
        skills.iter().any(|(n, _)| n == "writing-style"),
        "skills/writing-style is missing from the walk"
    );
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn every_legacy_name_still_exists() {
    // Arrange
    let commands = names(&["commit-and-push", "plan"]);
    let skills = names(&["playbook-usage"]);

    // Act
    let literal = check_legacy_exist(&["commit-and-push", "gone-name"], &commands, &skills);
    let real = check_legacy_exist(LEGACY_NAMES, &command_names(), &skill_names());

    // Assert
    assert_eq!(literal.len(), 1, "{literal:?}");
    assert!(literal[0].contains("gone-name"), "{literal:?}");
    assert!(real.is_empty(), "{real:#?}");
}

#[test]
fn a_rename_needs_its_new_name_and_no_old_file() {
    // Arrange
    let commands = names(&["commit", "plan", "old-name"]);
    let skills = names(&["help"]);
    let missing_new = [Rename {
        kind: Kind::Command,
        old: "x",
        new: "absent",
    }];
    let old_remains = [Rename {
        kind: Kind::Command,
        old: "old-name",
        new: "commit",
    }];
    let good = [Rename {
        kind: Kind::Skill,
        old: "playbook-usage",
        new: "help",
    }];

    // Act
    let a = check_renames(&missing_new, &commands, &skills);
    let b = check_renames(&old_remains, &commands, &skills);
    let c = check_renames(&good, &commands, &skills);
    let real = check_renames(RENAMES, &command_names(), &skill_names());

    // Assert
    assert!(
        a.iter().any(|p| p.contains("new name does not exist")),
        "{a:?}"
    );
    assert!(
        b.iter().any(|p| p.contains("old name still exists")),
        "{b:?}"
    );
    assert!(c.is_empty(), "{c:?}");
    assert!(real.is_empty(), "{real:#?}");
}

#[test]
fn removed_names_appear_nowhere() {
    // Arrange
    let removed = ["deep-review", "quick-review"];
    let doc = |path: &'static str, text: &'static str| ScannedFile { path, text };
    let cases: Vec<(&str, ScannedFile, bool)> = vec![
        (
            "a slash name in a doc",
            doc("docs/guide.md", "run /playbook:deep-review 12"),
            true,
        ),
        (
            "an effort key in the effort file",
            doc(
                "crates/cli/src/effort/component.rs",
                "effort.commands.deep-review",
            ),
            false,
        ),
        (
            "a slash name in the effort file",
            doc(
                "crates/cli/src/effort/component.rs",
                "/playbook:deep-review",
            ),
            true,
        ),
        (
            "a skill id in the closing list",
            doc(
                "crates/cli/src/hooks/auto_cost/closing.rs",
                "playbook:quick-review",
            ),
            true,
        ),
        (
            "the reference file and plain words",
            doc("docs/guide.md", "see quick-pass.md for a quick review"),
            false,
        ),
        (
            "a longer name",
            doc("docs/guide.md", "playbook:deep-review-extra"),
            false,
        ),
        (
            "the changelog",
            doc("CHANGELOG.md", "removed /playbook:deep-review"),
            false,
        ),
    ];
    let mut problems = Vec::new();

    // Act
    for (label, file, should_fail) in cases {
        let found = find_old_names(&[], &removed, &[file], OLD_NAME_ALLOWED);
        if found.is_empty() == should_fail {
            problems.push(format!(
                "{label}: expected failure {should_fail}, found {found:?}"
            ));
        }
    }
    let listing = check_removed_files(
        &removed,
        &names(&["quick-review", "plan"]),
        &names(&["writing-style"]),
    );
    let folder = check_removed_files(&removed, &names(&["plan"]), &names(&["deep-review"]));
    let files = scanned_files();
    let real = find_old_names(&[], REMOVED, &as_scanned(&files), OLD_NAME_ALLOWED);
    let real_files = check_removed_files(REMOVED, &command_names(), &skill_names());

    // Assert
    assert!(problems.is_empty(), "{problems:#?}");
    assert_eq!(listing.len(), 1, "{listing:?}");
    assert_eq!(folder.len(), 1, "{folder:?}");
    assert!(
        files.len() >= MIN_SCANNED_FILES,
        "the scan read {} files",
        files.len()
    );
    assert!(real.is_empty(), "{real:#?}");
    assert!(real_files.is_empty(), "{real_files:#?}");
}

#[test]
fn commands_have_no_subdirectories() {
    // Arrange
    let nested = vec![("pr".to_owned(), true), ("review.md".to_owned(), false)];
    let flat = vec![
        ("pr-create.md".to_owned(), false),
        ("review.md".to_owned(), false),
    ];
    let on_disk: Vec<(String, bool)> = fs::read_dir(plugin_root().join("commands"))
        .expect("commands/ is readable")
        .filter_map(Result::ok)
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                e.path().is_dir(),
            )
        })
        .collect();

    // Act
    let bad = check_commands_flat(&nested);
    let good = check_commands_flat(&flat);
    let real = check_commands_flat(&on_disk);

    // Assert
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(good.is_empty(), "{good:?}");
    assert!(real.is_empty(), "{real:#?}");
}

#[test]
fn old_names_appear_only_in_allowed_places() {
    // Arrange
    let renamed = ["commit-and-push"];
    let doc = ScannedFile {
        path: "docs/guides/04-auto-mode.md",
        text: "run `playbook:commit-and-push` now",
    };
    let changelog = ScannedFile {
        path: "CHANGELOG.md",
        text: "renamed /playbook:commit-and-push",
    };
    let renamed_page = ScannedFile {
        path: "docs/renamed.md",
        text: "| /playbook:commit-and-push | /playbook:commit |",
    };
    let adr = ScannedFile {
        path: "docs/adr/0012-x.md",
        text: "skills/commit-and-push/",
    };
    let closing = ScannedFile {
        path: "crates/cli/src/hooks/auto_cost/closing.rs",
        text: "\"playbook:commit-and-push\"",
    };
    let closing_path = ScannedFile {
        path: "crates/cli/src/hooks/auto_cost/closing.rs",
        text: "commands/commit-and-push.md",
    };

    // Act
    let in_doc = find_old_names(&renamed, &[], &[doc], OLD_NAME_ALLOWED);
    let kept = find_old_names(
        &renamed,
        &[],
        &[changelog, renamed_page, adr, closing],
        OLD_NAME_ALLOWED,
    );
    let wrong_form = find_old_names(&renamed, &[], &[closing_path], OLD_NAME_ALLOWED);
    let files = scanned_files();
    let real = find_old_names(
        &RENAMES.iter().map(|r| r.old).collect::<Vec<_>>(),
        REMOVED,
        &as_scanned(&files),
        OLD_NAME_ALLOWED,
    );

    // Assert
    assert_eq!(in_doc.len(), 1, "{in_doc:?}");
    assert!(kept.is_empty(), "{kept:?}");
    assert_eq!(wrong_form.len(), 1, "{wrong_form:?}");
    for known in [
        "plugins/playbook/commands/plan.md",
        "seed/prompts/SYSTEM_PROMPT.md",
        "crates/cli/src/lib.rs",
    ] {
        assert!(
            files.iter().any(|(p, _)| p == known),
            "the scan did not read {known}"
        );
    }
    assert!(
        files.len() >= MIN_SCANNED_FILES,
        "the scan read {} files",
        files.len()
    );
    assert!(real.is_empty(), "{real:#?}");
}

#[test]
fn hidden_skill_names_follow_the_skill_rule() {
    // Arrange
    let long_ok = "a".repeat(SKILL_NAME_MAX);
    let too_long = "a".repeat(SKILL_NAME_MAX + 1);
    let mut problems = Vec::new();

    // Act
    for name in ["engineering-standards-javascript", long_ok.as_str()] {
        if let Some(why) = check_skill_name(name) {
            problems.push(format!("`{name}` should pass: {why}"));
        }
    }
    for name in ["Engineering_Standards", too_long.as_str()] {
        if check_skill_name(name).is_none() {
            problems.push(format!("`{name}` should fail"));
        }
    }
    for name in skill_names() {
        if let Some(why) = check_skill_name(&name) {
            problems.push(why);
        }
    }

    // Assert
    assert!(problems.is_empty(), "{problems:#?}");
}

/// One row of the renamed page table: what the check gets and what it must say.
struct PageCase<'a> {
    label: &'a str,
    page: Option<&'a str>,
    renames: &'a [(&'a str, &'a str)],
    removed: &'a [&'a str],
    expected: Option<&'a str>,
}

#[test]
fn renamed_page_lists_every_old_slash_name() {
    // Arrange
    let renames = [("commit-and-push", "commit")];
    let removed = ["finish-pull-request"];
    let live = names(&["review", "pr-create", "commit"]);
    let good = "| Old name | New name |\n| --- | --- |\n\
        | `/playbook:commit-and-push` | `/playbook:commit` |\n\
        | `/playbook:finish-pull-request` | removed, use `/playbook:pr-create` |\n";
    let missing_row = "| `/playbook:commit-and-push` | `/playbook:commit` |\n";
    let reverted = format!("{good}| `/playbook:review` | `/playbook:plan` |\n");
    let stray = format!("{good}| `/playbook:never-was` | `/playbook:plan` |\n");
    let case = |label, page, renames, removed, expected| PageCase {
        label,
        page,
        renames,
        removed,
        expected,
    };
    let cases = [
        case("a full page", Some(good), &renames[..], &removed[..], None),
        case(
            "a missing row",
            Some(missing_row),
            &renames[..],
            &removed[..],
            Some("docs/renamed.md has no row for /playbook:finish-pull-request"),
        ),
        case(
            "a reverted rename",
            Some(reverted.as_str()),
            &renames[..],
            &removed[..],
            Some("/playbook:review is live but listed as an old name"),
        ),
        case(
            "a stray row",
            Some(stray.as_str()),
            &renames[..],
            &removed[..],
            Some("/playbook:never-was is listed but was never renamed or removed"),
        ),
        case("no names and no page", None, &[], &[], None),
        case(
            "names and no page",
            None,
            &renames[..],
            &[],
            Some("docs/renamed.md does not exist"),
        ),
    ];
    let mut problems = Vec::new();

    // Act
    for c in cases {
        let found = check_renamed_page(c.renames, c.removed, c.page, &live);
        match c.expected {
            None if !found.is_empty() => {
                problems.push(format!("{}: want a pass, got {found:?}", c.label))
            }
            Some(text) if !found.iter().any(|f| f.contains(text)) => {
                problems.push(format!("{}: want {text:?}, got {found:?}", c.label));
            }
            _ => {}
        }
    }
    let pairs: Vec<(&str, &str)> = RENAMES.iter().map(|r| (r.old, r.new)).collect();
    let page = fs::read_to_string(repo_root().join("docs/renamed.md")).ok();
    let mut live_now = command_names();
    live_now.extend(skill_names());
    let real = check_renamed_page(&pairs, REMOVED, page.as_deref(), &live_now);

    // Assert
    assert!(problems.is_empty(), "{problems:#?}");
    assert!(
        page.is_some() || (pairs.is_empty() && REMOVED.is_empty()),
        "docs/renamed.md is required"
    );
    assert!(real.is_empty(), "{real:#?}");
}
