// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Structural lint for `skills/*/SKILL.md`, derived from Anthropic's Agent
//! Skills best practices: name matches the directory, a third-person
//! description under the length caps, a bounded body, and references that
//! stay one level deep. Descriptions sit in every session's context, so the
//! description cap is deliberately tighter than the platform's 1024.

use std::fs;
use std::path::{Path, PathBuf};

const MAX_BODY_LINES: usize = 500;
const MAX_DESCRIPTION_CHARS: usize = 400;
const PLATFORM_DESCRIPTION_CHARS: usize = 1024;
const TOC_THRESHOLD_LINES: usize = 100;

fn skills_dir() -> PathBuf {
    Path::new(env!("PLAYBOOK_PLUGIN_ROOT")).join("skills")
}

fn skill_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(skills_dir())
        .expect("skills dir should exist")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// Returns (name, description, body) from a SKILL.md with `---` frontmatter.
fn parse(text: &str) -> (String, String, String) {
    let rest = text.strip_prefix("---\n").expect("frontmatter must open");
    let (front, body) = rest.split_once("\n---\n").expect("frontmatter must close");
    let field = |key: &str| {
        front
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{key}:")))
            .map(|v| v.trim().trim_matches('"').to_string())
            .unwrap_or_default()
    };
    (field("name"), field("description"), body.to_string())
}

fn md_links(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("](") {
        rest = &rest[i + 2..];
        if let Some(j) = rest.find(')') {
            out.push(rest[..j].to_string());
        }
    }
    out
}

/// Names of the `references/<name>.md` files a SKILL.md body points at.
/// A path with a skill prefix (`skills/x/references/y.md`) points into
/// another skill and is checked by `intent_ids_and_reference_paths_in_the_prose_resolve`.
fn reference_pointers(body: &str) -> Vec<String> {
    let pointer = regex::Regex::new(r"(^|[^/a-z-])references/([a-z0-9][a-z0-9-]*)\.md").unwrap();
    pointer
        .captures_iter(body)
        .map(|cap| cap[2].to_string())
        .collect()
}

#[test]
fn reference_pointers_skip_paths_into_other_skills() {
    let body = "Load `references/a-b.md` first, then skills/x/references/c.md.";
    assert_eq!(reference_pointers(body), vec!["a-b".to_string()]);
}

#[test]
fn every_skill_passes_structural_lint() {
    let mut problems = Vec::new();
    for dir in skill_dirs() {
        let dir_name = dir.file_name().unwrap().to_string_lossy().to_string();
        let text = fs::read_to_string(dir.join("SKILL.md"))
            .unwrap_or_else(|_| panic!("{dir_name} needs a SKILL.md"));
        let (name, desc, body) = parse(&text);
        if name != dir_name {
            problems.push(format!(
                "{dir_name}: name `{name}` must match the directory"
            ));
        }
        if name.len() > 64
            || !name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            || name.contains("anthropic")
            || name.contains("claude")
        {
            problems.push(format!("{dir_name}: invalid name `{name}`"));
        }
        if desc.is_empty() {
            problems.push(format!("{dir_name}: description is empty"));
        }
        if desc.chars().count() > MAX_DESCRIPTION_CHARS {
            problems.push(format!(
                "{dir_name}: description is {} chars, cap {MAX_DESCRIPTION_CHARS} (platform {PLATFORM_DESCRIPTION_CHARS})",
                desc.chars().count()
            ));
        }
        if desc.contains('<') || desc.contains('>') {
            problems.push(format!("{dir_name}: description must not contain XML tags"));
        }
        let lines = body.lines().count();
        if lines > MAX_BODY_LINES {
            problems.push(format!(
                "{dir_name}: body is {lines} lines, cap {MAX_BODY_LINES}"
            ));
        }
        let refs = dir.join("references");
        // Prose may name another skill's reference ("the grounding-review
        // skill, `references/x.md`"), so a match in any skill counts.
        for name in reference_pointers(&body) {
            let file = format!("references/{name}.md");
            if !refs.join(format!("{name}.md")).is_file()
                && !skill_dirs().iter().any(|d| d.join(&file).is_file())
            {
                problems.push(format!(
                    "{dir_name}/SKILL.md: names `references/{name}.md`, which does not exist"
                ));
            }
        }
        if let Ok(entries) = fs::read_dir(&refs) {
            for entry in entries.filter_map(Result::ok) {
                let p = entry.path();
                if p.is_dir() {
                    problems.push(format!("{dir_name}: references/ must be flat, found {p:?}"));
                    continue;
                }
                let t = fs::read_to_string(&p).unwrap_or_default();
                let fname = p.file_name().unwrap().to_string_lossy().to_string();
                for link in md_links(&t) {
                    if !link.starts_with("http") && link.contains(".md") && !link.starts_with('#') {
                        problems.push(format!(
                            "{dir_name}/references/{fname}: links to `{link}`, references must be one level deep"
                        ));
                    }
                }
                if t.lines().count() > TOC_THRESHOLD_LINES && !t.contains("## Contents") {
                    problems.push(format!(
                        "{dir_name}/references/{fname}: over {TOC_THRESHOLD_LINES} lines needs a `## Contents` section"
                    ));
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "skill lint failures:\n{}",
        problems.join("\n")
    );
}

fn read_skill(rel: &str) -> String {
    fs::read_to_string(skills_dir().join(rel)).unwrap_or_else(|_| panic!("{rel} should exist"))
}

#[test]
fn lens_reviewers_do_not_load_the_orchestrator_sections() {
    let skill = read_skill("grounding-review/SKILL.md");
    for heading in [
        "## Review Report Format",
        "## Verification Sweep",
        "## Subject Lines",
        "## Severity",
    ] {
        assert!(
            !skill.contains(heading),
            "grounding-review SKILL.md must not carry `{heading}`, it is orchestrator-only"
        );
    }
    let orch = read_skill("grounding-review/references/orchestrator.md");
    assert!(orch.contains("## Review Report Format"));
    assert!(orch.contains("## Verification Sweep (MUST)"));
    assert!(skill.contains("references/orchestrator.md"));
}

#[test]
fn writing_style_keeps_the_loading_heading_and_every_reference() {
    let skill = read_skill("writing-style/SKILL.md");
    assert!(
        skill
            .lines()
            .any(|l| l.starts_with("## Loading the references")),
        "src/pr/rules.rs ends the core slice at this heading"
    );
    // `playbook pr rules` cuts on these markers; a rename breaks it at run time.
    for marker in [
        "> **IRON RULE:** MUST NEVER use em dashes",
        "## Banned Words",
    ] {
        assert!(skill.contains(marker), "src/pr/rules.rs cuts on `{marker}`");
    }
    let commits = read_skill("writing-style/references/commits-and-prs.md");
    assert!(commits.contains("- Commit messages MUST"));
    assert!(commits.contains("## Pull requests"));
    let prohibited = read_skill("writing-style/references/github-prohibited.md");
    assert!(prohibited.contains("# Prohibited GitHub"));
    for name in [
        "examples",
        "phrases",
        "structures",
        "review-examples",
        "shell-quoting",
        "commits-and-prs",
        "github-review-comments",
        "github-prohibited",
    ] {
        assert!(skill.contains(&format!("`{name}`")), "{name}");
        read_skill(&format!("writing-style/references/{name}.md"));
    }
}

#[test]
fn playbook_usage_names_every_command() {
    let usage = read_skill("playbook-usage/SKILL.md");
    let commands = Path::new(env!("PLAYBOOK_PLUGIN_ROOT")).join("commands");
    for entry in fs::read_dir(commands).unwrap().filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "md") {
            let name = path.file_stem().unwrap().to_string_lossy().to_string();
            assert!(
                usage.contains(&format!("/playbook:{name}")),
                "playbook-usage does not list /playbook:{name}"
            );
        }
    }
    assert!(
        !usage.contains("14 slash commands"),
        "do not hard-code a command count"
    );
}

/// `playbook skill ref <skill> <name>` silently falls back to the skill's
/// `SKILL.md` when the reference file is missing, so a renamed or deleted
/// reference would go unnoticed. Every literal pointer must resolve.
#[test]
fn every_skill_ref_pointer_resolves_to_a_reference_file() {
    let root = Path::new(env!("PLAYBOOK_PLUGIN_ROOT"));
    let pointer =
        regex::Regex::new(r"playbook skill ref ([a-z][a-z-]*) ([a-z][a-z0-9-]*)").unwrap();
    let mut checked = 0;
    for dir in ["commands", "agents", "skills"] {
        let mut stack = vec![root.join(dir)];
        while let Some(path) = stack.pop() {
            if path.is_dir() {
                stack.extend(
                    fs::read_dir(&path)
                        .unwrap()
                        .filter_map(Result::ok)
                        .map(|e| e.path()),
                );
            } else if path.extension().is_some_and(|e| e == "md") {
                let text = fs::read_to_string(&path).unwrap();
                for cap in pointer.captures_iter(&text) {
                    let file = root.join(format!("skills/{}/references/{}.md", &cap[1], &cap[2]));
                    assert!(
                        file.is_file(),
                        "{}: no reference file {}",
                        path.display(),
                        file.display()
                    );
                    checked += 1;
                }
            }
        }
    }
    // The pointers are being retired in favour of relative paths, so no minimum.
    let _ = checked;
}

#[test]
fn the_run_context_reference_carries_the_run_mode_intent() {
    let text = read_skill("delegating-subagents/references/run-context.md");
    assert!(
        text.contains("Intent: run-mode"),
        "run-context.md must name the run-mode intent, the commands point at it"
    );
}

fn markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            markdown_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}

/// Every `Intent: <id>` the prose names is listed in intents.json, and every
/// `skills/<skill>/references/<name>.md` path it points at exists.
#[test]
fn intent_ids_and_reference_paths_in_the_prose_resolve() {
    let root = Path::new(env!("PLAYBOOK_PLUGIN_ROOT"));
    let listed = fs::read_dir(root)
        .ok()
        .and_then(|_| {
            ["../intents.json", "intents.json"]
                .iter()
                .find_map(|p| fs::read_to_string(root.join(p)).ok())
        })
        .expect("intents.json should sit in plugins/, above the plugin root");
    let intent = regex::Regex::new(r"Intent: ([a-z][a-z0-9-]*)").unwrap();
    let path =
        regex::Regex::new(r"skills/([a-z][a-z-]*)/references/([a-z][a-z0-9-]*)\.md").unwrap();
    let mut files = Vec::new();
    for dir in ["commands", "skills"] {
        markdown_files(&root.join(dir), &mut files);
    }
    let mut bad = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file).unwrap();
        for cap in intent.captures_iter(&text) {
            if !listed.contains(&format!("\"{}\"", &cap[1])) {
                bad.push(format!("{}: unknown intent {}", file.display(), &cap[1]));
            }
        }
        for cap in path.captures_iter(&text) {
            let target = root.join(format!("skills/{}/references/{}.md", &cap[1], &cap[2]));
            if !target.is_file() {
                bad.push(format!("{}: missing {}", file.display(), target.display()));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// A language standards index loads references only through its load-when
/// lines, so a reference it does not name is never read. The structural lint
/// accepts a pointer that resolves in any skill, and the language skills share
/// file names, so each pointer must also resolve in its own skill.
#[test]
fn language_standards_name_every_reference_and_only_their_own() {
    let mut checked = Vec::new();
    for dir in skill_dirs() {
        let skill_name = dir.file_name().unwrap().to_string_lossy().to_string();
        let refs = dir.join("references");
        if !skill_name.starts_with("engineering-standards-") || !refs.is_dir() {
            continue;
        }
        let named = reference_pointers(&read_skill(&format!("{skill_name}/SKILL.md")));
        for entry in fs::read_dir(&refs).unwrap().filter_map(Result::ok) {
            let path = entry.path();
            let name = path.file_stem().unwrap().to_string_lossy().to_string();
            assert!(
                named.contains(&name),
                "{skill_name}/SKILL.md does not name references/{name}.md"
            );
        }
        for name in &named {
            assert!(
                refs.join(format!("{name}.md")).is_file(),
                "{skill_name}/SKILL.md names references/{name}.md, which is not in its own references/"
            );
        }
        checked.push(skill_name);
    }
    for expected in ["engineering-standards-python", "engineering-standards-rust"] {
        assert!(
            checked.iter().any(|s| s == expected),
            "{expected} has no references/ folder to check"
        );
    }
}

/// Every forked command or agent that runs the worktree guard must name the
/// skill that owns `references/worktree-guard.md`, and that file must exist.
#[test]
fn forked_files_point_at_the_existing_worktree_guard() {
    let root = Path::new(env!("PLAYBOOK_PLUGIN_ROOT"));
    let mut users = Vec::new();
    for dir in ["commands", "agents"] {
        for entry in fs::read_dir(root.join(dir)).unwrap().filter_map(Result::ok) {
            let path = entry.path();
            let text = fs::read_to_string(&path).unwrap_or_default();
            if text.contains("references/worktree-guard.md") {
                let skill = text
                    .split("references/worktree-guard.md")
                    .next()
                    .and_then(|before| before.rsplit("`playbook:").next())
                    .and_then(|tail| tail.split('`').next())
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    skills_dir()
                        .join(&skill)
                        .join("references/worktree-guard.md")
                        .is_file(),
                    "{} points at skill `{skill}`, which has no references/worktree-guard.md",
                    path.display()
                );
                users.push(path.file_name().unwrap().to_string_lossy().to_string());
            }
        }
    }
    assert!(
        users.iter().any(|u| u == "git.md"),
        "git.md must use the guard"
    );
}
