// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Every skill folder has exactly one kind row, and the kinds stay few: ten
//! rule skills, the one catalog skill and the one recipe skill that folds into
//! `pr-create`. The list is in `support/skill_kinds.rs`. The menu flag stays
//! with `slash_menu_visibility.rs`, which reads the catalog rows.

#[path = "support/skill_kinds.rs"]
mod skill_kinds;

use skill_kinds::{skills_of, SkillKind, SKILL_KINDS};
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

/// The catalog skill. A rule skill cannot become visible by a kind change
/// alone, so this name is a literal here.
const CATALOG_NAME: &str = "playbook-usage";

/// The one recipe skill that may stay until it folds into `pr-create`.
const RECIPE_NAME: &str = "finish-pull-request";

const MIN_FOLDERS: usize = 11;

/// One bad list: a label, the rows, the folders and the message it must give.
type Case<'a> = (&'a str, Vec<(&'a str, SkillKind)>, Vec<&'a str>, &'a str);

/// The problems between the kind rows and the skill folders.
fn kind_problems(rows: &[(&str, SkillKind)], folders: &[&str]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen = BTreeSet::new();
    for (name, _) in rows {
        if !seen.insert(*name) {
            problems.push(format!("`{name}` has more than one kind row"));
        }
    }
    for folder in folders {
        if !seen.contains(folder) {
            problems.push(format!("skills/{folder} has no kind row"));
        }
    }
    for (name, kind) in rows {
        if !folders.contains(name) {
            problems.push(format!("the row `{name}` has no skills/{name} folder"));
        }
        if *kind == SkillKind::Recipe && *name != RECIPE_NAME {
            problems.push(format!(
                "`{name}` is a recipe skill; only {RECIPE_NAME} may be"
            ));
        }
    }
    let catalog: Vec<&str> = rows
        .iter()
        .filter(|(_, k)| *k == SkillKind::Catalog)
        .map(|(n, _)| *n)
        .collect();
    if catalog != [CATALOG_NAME] {
        problems.push(format!(
            "the catalog rows must be exactly `{CATALOG_NAME}`, got {catalog:?}"
        ));
    }
    problems
}

fn real_folders() -> Vec<String> {
    let dir = PathBuf::from(env!("PLAYBOOK_PLUGIN_ROOT")).join("skills");
    let mut out: Vec<String> = fs::read_dir(dir)
        .expect("skills dir")
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

#[test]
fn a_bad_kind_list_fails_naming_the_skill() {
    // Arrange
    let good: &[(&str, SkillKind)] = &[
        ("a", SkillKind::Rule),
        ("playbook-usage", SkillKind::Catalog),
        ("finish-pull-request", SkillKind::Recipe),
    ];
    let folders = ["a", "playbook-usage", "finish-pull-request"];
    let cases: Vec<Case> = vec![
        (
            "a folder with no row",
            good.to_vec(),
            vec!["a", "playbook-usage", "finish-pull-request", "foo"],
            "skills/foo has no kind row",
        ),
        (
            "a row with no folder",
            [good, &[("foo", SkillKind::Rule)]].concat(),
            folders.to_vec(),
            "the row `foo` has no skills/foo folder",
        ),
        (
            "two kinds for one skill",
            [good, &[("a", SkillKind::Catalog)]].concat(),
            folders.to_vec(),
            "`a` has more than one kind row",
        ),
        (
            "a second recipe",
            [good, &[("foo", SkillKind::Recipe)]].concat(),
            vec!["a", "playbook-usage", "finish-pull-request", "foo"],
            "`foo` is a recipe skill",
        ),
        (
            "a rule skill made visible",
            vec![
                ("a", SkillKind::Catalog),
                ("playbook-usage", SkillKind::Catalog),
                ("finish-pull-request", SkillKind::Recipe),
            ],
            folders.to_vec(),
            "the catalog rows must be exactly",
        ),
    ];
    let mut problems = Vec::new();

    // Act
    for (label, rows, folders, want) in &cases {
        let found = kind_problems(rows, folders);
        if !found.iter().any(|p| p.contains(want)) {
            problems.push(format!("{label}: want `{want}`, got {found:?}"));
        }
    }
    let clean = kind_problems(good, &folders);

    // Assert
    assert!(problems.is_empty(), "{problems:#?}");
    assert!(clean.is_empty(), "{clean:?}");
}

#[test]
fn every_skill_has_one_kind() {
    // Arrange
    let folders = real_folders();
    let names: Vec<&str> = folders.iter().map(String::as_str).collect();

    // Act
    let problems = kind_problems(SKILL_KINDS, &names);

    // Assert
    assert!(problems.is_empty(), "{problems:#?}");
    assert!(
        folders.len() >= MIN_FOLDERS,
        "only {} skill folders",
        folders.len()
    );
    assert_eq!(SKILL_KINDS.len(), folders.len());
    assert_eq!(skills_of(SkillKind::Catalog), [CATALOG_NAME]);
    assert_eq!(skills_of(SkillKind::Recipe), [RECIPE_NAME]);
    assert_eq!(skills_of(SkillKind::Rule).len(), 10);
}
