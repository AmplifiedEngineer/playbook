// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The pure checks behind the skill call graph guard.
//!
//! A recipe is a command or a skill with steps. A rule skill holds knowledge.
//! A recipe loads rule skills, dispatches agents and calls the binary, and it
//! never invokes another recipe. A rule skill never loads another skill. The
//! scan reads each plugin markdown file for the names of recipes and rule
//! skills, sorts every line into an invocation, a pointer, a load or a
//! mention, and turns the invocations and loads into rows that a shrink-only
//! list must match. Every function takes file texts as arguments, so a test
//! runs it on literals first.

use crate::skill_kinds::{skills_of, SkillKind};
use regex::Regex;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, OnceLock};

/// What reads a line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Source {
    /// A command file, or any file of a recipe skill or a reference file.
    Recipe,
    /// The `SKILL.md` of a rule skill.
    RuleSkill,
    /// An agent file.
    Agent,
}

/// What a line that names a target does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Class {
    /// A step runs the recipe.
    Invoke,
    /// A line that tells the user what to run next.
    Pointer,
    /// A line that loads, follows or applies a rule skill.
    Load,
    /// A line that names the target as context.
    Mention,
}

/// The kinds of list row.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum RowKind {
    /// A file runs a recipe.
    Invoke,
    /// A reviewed file names a recipe with no invoke verb.
    Mention,
    /// A rule skill's `SKILL.md` loads a rule skill.
    RuleLoad,
}

impl RowKind {
    fn word(self) -> &'static str {
        match self {
            RowKind::Invoke => "invoke",
            RowKind::Mention => "mention",
            RowKind::RuleLoad => "rule-load",
        }
    }
}

/// One row: a file and the target it names.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) struct Row {
    pub(crate) kind: RowKind,
    pub(crate) file: String,
    pub(crate) target: String,
}

impl Row {
    pub(crate) fn new(kind: RowKind, file: &str, target: &str) -> Row {
        Row {
            kind,
            file: file.to_owned(),
            target: target.to_owned(),
        }
    }

    fn show(&self) -> String {
        format!("{} {} {}", self.kind.word(), self.file, self.target)
    }
}

/// A plugin markdown file: its path from the plugin root and its text.
pub(crate) struct File<'a> {
    pub(crate) path: &'a str,
    pub(crate) text: &'a str,
}

fn invoke_verbs() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)skill tool|invok|hand off|hands over|\bload|run `/playbook:")
            .expect("invoke verbs")
    })
}

fn load_verbs() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\bload|\bfollow|\bapply|\bread").expect("load verbs"))
}

/// "Tell the user to run" and the commands it names, as one list.
fn pointer_clause() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)tell the user to run (?:`[^`]*`|\S+)(?:(?:,? (?:or|and) |, )`[^`]*`)*")
            .expect("pointer clause")
    })
}

/// Whether `text` names the command, skill or agent `name` as `playbook:name`
/// or `/playbook:name`, and not a longer name that starts with it.
fn names(text: &str, name: &str) -> bool {
    static CACHE: OnceLock<Mutex<HashMap<String, Regex>>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("name pattern cache");
    cache
        .entry(name.to_owned())
        .or_insert_with(|| {
            Regex::new(&format!(
                r"(?i)playbook:{}(?:[^A-Za-z0-9-]|$)",
                regex::escape(name)
            ))
            .expect("name pattern")
        })
        .is_match(text)
}

/// The sentences of a line, cut after a full stop, question mark, exclamation
/// mark or semicolon that a space follows.
fn sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if matches!(b, b'.' | b'?' | b'!' | b';') && bytes.get(i + 1) == Some(&b' ') {
            out.push(&text[start..=i]);
            start = i + 2;
        }
    }
    out.push(&text[start..]);
    out
}

/// What the sentences that name `target` hold: a pointer clause that cut the
/// name, a plain mention, or an invoke verb next to the name.
struct Seen {
    pointer: bool,
    plain: bool,
    verb: bool,
}

fn seen(text: &str, target: &str) -> Option<Seen> {
    let mut out = Seen {
        pointer: false,
        plain: false,
        verb: false,
    };
    let mut any = false;
    for sentence in sentences(text).into_iter().filter(|s| names(s, target)) {
        any = true;
        let rest = pointer_clause().replace_all(sentence, "");
        if !names(&rest, target) {
            out.pointer = true;
        } else if invoke_verbs().is_match(&rest) {
            out.verb = true;
        } else {
            out.plain = true;
        }
    }
    any.then_some(out)
}

/// Sorts one line that names `target`. `target_is_rule` says whether `target`
/// is a rule skill. `None` means the line does not name it.
pub(crate) fn classify_line(
    text: &str,
    target: &str,
    target_is_rule: bool,
    source: Source,
) -> Option<Class> {
    if !names(text, target) {
        return None;
    }
    if target_is_rule {
        return Some(if load_verbs().is_match(text) {
            Class::Load
        } else {
            Class::Mention
        });
    }
    let seen = seen(text, target)?;
    Some(if seen.verb || (seen.plain && source == Source::Recipe) {
        Class::Invoke
    } else if seen.plain {
        Class::Mention
    } else {
        Class::Pointer
    })
}

/// Whether a sentence next to the name holds an invoke verb, which only an
/// `invoke` row allows.
fn has_verb(text: &str, target: &str) -> bool {
    seen(text, target).is_some_and(|s| s.verb)
}

/// Who owns a file and how its lines count, or `None` for a skipped file.
fn owner(path: &str) -> Option<(String, Source)> {
    let mut parts = path.split('/');
    match (parts.next()?, parts.next()?) {
        ("commands", file) => Some((file.strip_suffix(".md")?.to_owned(), Source::Recipe)),
        ("agents", file) => Some((file.strip_suffix(".md")?.to_owned(), Source::Agent)),
        ("skills", skill) if skills_of(SkillKind::Catalog).contains(&skill) => None,
        ("skills", skill) => {
            let main = parts.next() == Some("SKILL.md") && parts.next().is_none();
            let source = if main && skills_of(SkillKind::Rule).contains(&skill) {
                Source::RuleSkill
            } else {
                Source::Recipe
            };
            Some((skill.to_owned(), source))
        }
        _ => None,
    }
}

/// Every row the files produce. `recipes` holds the names of the commands and
/// recipe skills a line may name. The owner never counts as naming itself.
pub(crate) fn edges(files: &[File], recipes: &[String]) -> BTreeSet<Row> {
    let mut rows = BTreeSet::new();
    for file in files {
        let Some((own, source)) = owner(file.path) else {
            continue;
        };
        for line in file.text.lines() {
            for target in recipes.iter().filter(|r| **r != own) {
                if classify_line(line, target, false, source) == Some(Class::Invoke) {
                    let kind = if has_verb(line, target) {
                        RowKind::Invoke
                    } else {
                        RowKind::Mention
                    };
                    rows.insert(Row::new(kind, file.path, target));
                }
            }
            if source == Source::RuleSkill {
                for target in skills_of(SkillKind::Rule).into_iter().filter(|r| *r != own) {
                    if classify_line(line, target, true, source) == Some(Class::Load) {
                        rows.insert(Row::new(RowKind::RuleLoad, file.path, target));
                    }
                }
            }
        }
    }
    collapse(rows)
}

/// An `invoke` row covers a `mention` row for the same file and target.
fn collapse(rows: BTreeSet<Row>) -> BTreeSet<Row> {
    rows.iter()
        .filter(|r| {
            r.kind != RowKind::Mention
                || !rows
                    .iter()
                    .any(|o| o.kind == RowKind::Invoke && o.file == r.file && o.target == r.target)
        })
        .cloned()
        .collect()
}

/// The number of lines in command and skill files that name a rule skill and
/// load it, so an empty scan fails.
pub(crate) fn rule_load_lines(files: &[File]) -> usize {
    files
        .iter()
        .filter(|f| matches!(owner(f.path), Some((_, Source::Recipe))))
        .flat_map(|f| f.text.lines())
        .filter(|line| {
            skills_of(SkillKind::Rule)
                .iter()
                .any(|r| classify_line(line, r, true, Source::Recipe) == Some(Class::Load))
        })
        .count()
}

/// Parses the list: one row per line, `invoke|mention|rule-load <file> <target>`,
/// with blank lines and `#` comments skipped.
pub(crate) fn parse_list(text: &str) -> Result<BTreeSet<Row>, String> {
    let mut rows = BTreeSet::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        let kind = match words.as_slice() {
            ["invoke", ..] => RowKind::Invoke,
            ["mention", ..] => RowKind::Mention,
            ["rule-load", ..] => RowKind::RuleLoad,
            _ => return Err(format!("line {}: unknown row `{line}`", i + 1)),
        };
        let [_, file, target] = words.as_slice() else {
            return Err(format!("line {}: a row needs a file and a target", i + 1));
        };
        rows.insert(Row::new(kind, file, target));
    }
    Ok(rows)
}

/// The problems between what the scan found and what the list holds: a found
/// row that is not listed, a listed row the scan no longer finds, and an
/// agent that invokes a recipe, which no row allows.
pub(crate) fn check(found: &BTreeSet<Row>, listed: &BTreeSet<Row>) -> Vec<String> {
    let mut problems = Vec::new();
    for row in found.difference(listed) {
        problems.push(format!("not on the list: {}", row.show()));
    }
    for row in listed.difference(found) {
        problems.push(format!("the scan no longer finds: {}", row.show()));
    }
    for row in found {
        if row.file.starts_with("agents/") && row.kind != RowKind::Mention {
            problems.push(format!("an agent never invokes a recipe: {}", row.show()));
        }
    }
    problems
}
