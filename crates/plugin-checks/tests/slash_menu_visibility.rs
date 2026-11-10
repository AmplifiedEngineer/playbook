// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Only public entry points show in the slash menu. A skill with
//! `user-invocable: false` is hidden from the `/` menu and stays loadable by
//! the model and by subagents (Claude Code docs, "Control who invokes a
//! skill"). A new skill is hidden by default: it needs a `catalog` row in
//! `SKILL_KINDS` on purpose to show up.
//!
//! Commands are not hidden. The docs state the field for skills only, so a
//! hidden command could break a path that runs one. The command side stays
//! visible until a live session confirms the field works there.

#[path = "support/skill_kinds.rs"]
mod skill_kinds;

use skill_kinds::{skills_of, SkillKind};
use std::fs;
use std::path::{Path, PathBuf};

/// Skills a user may type as a slash command: the catalog rows.
fn public_skills() -> Vec<&'static str> {
    skills_of(SkillKind::Catalog)
}

const HIDE_LINE: &str = "user-invocable: false";

fn plugin_dir(rel: &str) -> PathBuf {
    Path::new(env!("PLAYBOOK_PLUGIN_ROOT")).join(rel)
}

fn frontmatter(text: &str) -> &str {
    let rest = text.strip_prefix("---\n").expect("frontmatter must open");
    rest.split_once("\n---\n")
        .expect("frontmatter must close")
        .0
}

fn is_hidden(path: &Path) -> bool {
    let text = fs::read_to_string(path).expect("readable markdown");
    frontmatter(&text).lines().any(|l| l.trim() == HIDE_LINE)
}

fn skill_files() -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = fs::read_dir(plugin_dir("skills"))
        .expect("skills dir")
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                e.path().join("SKILL.md"),
            )
        })
        .collect();
    out.sort();
    out
}

#[test]
fn only_the_public_skills_show_in_the_slash_menu() {
    let skills = skill_files();
    assert!(skills.len() >= 10, "skill walk looks broken");
    let mut problems = Vec::new();
    for (name, path) in &skills {
        let public = public_skills().contains(&name.as_str());
        match (public, is_hidden(path)) {
            (true, true) => problems.push(format!("{name}: public skill must not be hidden")),
            (false, false) => problems.push(format!("{name}: helper skill needs `{HIDE_LINE}`")),
            _ => {}
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn every_public_skill_exists() {
    let names: Vec<String> = skill_files().into_iter().map(|(n, _)| n).collect();
    for public in public_skills() {
        assert!(
            names.iter().any(|n| n == public),
            "the catalog row names `{public}`, which is not a skill"
        );
    }
}

#[test]
fn commands_stay_visible_until_hiding_them_is_verified() {
    let dir = plugin_dir("commands");
    for entry in fs::read_dir(dir).expect("commands dir").flatten() {
        assert!(
            !is_hidden(&entry.path()),
            "{}: hiding commands is unverified",
            entry.path().display()
        );
    }
}
