// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The one list of skill kinds. Each skill folder has exactly one row. The
//! rule skills, the catalog and the public skills are read from it, so no test
//! keeps its own copy.

/// What a skill is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SkillKind {
    /// Holds knowledge. Its `SKILL.md` runs no tools or skills.
    Rule,
    /// The catalog of commands. The one visible kind.
    Catalog,
    /// Has steps, so it counts as a recipe. Only `finish-pull-request` until
    /// it folds into `pr-create`.
    Recipe,
}

/// One row per skill folder under `plugins/playbook/skills`.
pub(crate) const SKILL_KINDS: &[(&str, SkillKind)] = &[
    ("atlassian", SkillKind::Rule),
    ("delegating-subagents", SkillKind::Rule),
    ("engineering-standards", SkillKind::Rule),
    ("engineering-standards-javascript", SkillKind::Rule),
    ("engineering-standards-python", SkillKind::Rule),
    ("engineering-standards-rust", SkillKind::Rule),
    ("finish-pull-request", SkillKind::Recipe),
    ("grounding-research", SkillKind::Rule),
    ("grounding-review", SkillKind::Rule),
    ("playbook-usage", SkillKind::Catalog),
    ("systematic-debugging", SkillKind::Rule),
    ("writing-style", SkillKind::Rule),
];

/// The skill names of one kind, in list order.
pub(crate) fn skills_of(kind: SkillKind) -> Vec<&'static str> {
    SKILL_KINDS
        .iter()
        .filter(|(_, k)| *k == kind)
        .map(|(name, _)| *name)
        .collect()
}
