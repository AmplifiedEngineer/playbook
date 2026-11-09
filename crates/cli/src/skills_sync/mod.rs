// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Copies the playbook skills and commands into other agents' skill folders.
//! See `docs/internals/15-codex-skills.md`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

pub mod apply;
pub mod plan;
pub mod render;

/// An agent that reads skills from a shared folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Codex,
}

impl Agent {
    /// The name used in ownership record keys.
    pub fn key(self) -> &'static str {
        match self {
            Agent::Codex => "codex",
        }
    }
}

const SKILL_FILE: &str = "SKILL.md";

/// Where Codex reads skills, relative to the home folder.
pub const TARGET_DIR: &str = ".agents/skills";

const ALL_AGENTS: [Agent; 1] = [Agent::Codex];
const RECORD_PREFIX: &str = "skills-sync/";
/// The refresh claim lives beside the folder records and is not one.
const REFRESH_KEY: &str = "refresh";
const VERSION_FIELD: &str = "version ";

/// The key prefix of every ownership row of `agent`.
fn record_prefix(agent: Agent) -> String {
    format!("{RECORD_PREFIX}{}/", agent.key())
}

fn refresh_key(agent: Agent) -> String {
    format!("{}{REFRESH_KEY}", record_prefix(agent))
}

/// What a refresh did: the report lines of every agent it handled, and
/// whether each one ended clean.
#[derive(Debug, Default)]
pub struct Refreshed {
    pub lines: Vec<String>,
    pub clean: bool,
}

/// The plugin folder and its version, or `None` when no plugin is found.
fn resolve_plugin(home: &Path) -> Option<(PathBuf, String)> {
    let resolved = crate::plugin_root::resolve_from(home).ok()?;
    // Without a version there is nothing to compare a record against.
    let version = crate::plugin_root::plugin_version(&resolved.root).filter(|v| !v.is_empty())?;
    Some((resolved.root, version))
}

/// Whether any folder record of `agent` was written for another plugin
/// version. `false` when the agent has no records, so a first install stays
/// an explicit `skills sync`.
fn has_stale_records(root: &Path, agent: Agent, version: &str) -> bool {
    let Ok(rows) = crate::state::list_read_only(root, &record_prefix(agent)) else {
        return false;
    };
    rows.iter()
        .filter(|(key, _)| key != &refresh_key(agent))
        .any(|(_, text)| {
            text.lines()
                .find_map(|l| l.strip_prefix(VERSION_FIELD))
                .is_some_and(|recorded| recorded != version)
        })
}

fn read_claim(root: &Path, agent: Agent) -> Option<String> {
    let key = refresh_key(agent);
    crate::state::list_read_only(root, &key)
        .ok()?
        .into_iter()
        .find_map(|(k, v)| (k == key && !v.is_empty()).then_some(v))
}

/// The refresh claim rules, same as the plugin update marker: nothing for a
/// version already `done`, a retry a day after `failed`, and a `running`
/// claim that went stale after ten minutes.
fn claim_due(claim: Option<&str>, version: &str, now: u64) -> bool {
    use crate::plugin_update::{Marker, State, FAILED_RETRY, RUNNING_STALE};
    let Some(marker) = claim
        .and_then(Marker::parse)
        .filter(|m| m.version == version)
    else {
        return true;
    };
    let age = std::time::Duration::from_secs(now.saturating_sub(marker.at));
    match marker.state {
        State::Done => false,
        State::Failed | State::Behind => age > FAILED_RETRY,
        State::Running => age > RUNNING_STALE,
    }
}

/// Reads the claim again and, unless a dry run, takes it. `false` when a
/// refresh that finished since the caller's read left nothing due, or when
/// another process took it first.
fn try_claim(root: &Path, agent: Agent, version: &str, dry_run: bool, now: u64) -> bool {
    let seen = read_claim(root, agent);
    if !claim_due(seen.as_deref(), version, now) {
        return false;
    }
    dry_run || take_claim(root, agent, seen.as_deref(), version, now)
}

/// Moves the claim from `seen` to `running` in one transaction, so of two
/// processes that read the same value only one refreshes.
fn take_claim(root: &Path, agent: Agent, seen: Option<&str>, version: &str, now: u64) -> bool {
    use crate::plugin_update::{Marker, State};
    let key = refresh_key(agent);
    let running = Marker {
        version: version.to_string(),
        state: State::Running,
        at: now,
    };
    crate::state::transaction(root, |tx| {
        if tx.get(&key)?.filter(|v| !v.is_empty()).as_deref() == seen {
            tx.set(&key, &running.format())?;
            return Ok(true);
        }
        Ok(false)
    })
    .unwrap_or(false)
}

fn write_claim(root: &Path, agent: Agent, version: &str, state: crate::plugin_update::State) {
    let marker = crate::plugin_update::Marker {
        version: version.to_string(),
        state,
        at: crate::plugin_update::now(),
    };
    let _ = crate::state::set(root, &refresh_key(agent), &marker.format());
}

/// Agents that were synced before, whose records name another plugin version
/// and whose refresh claim is due. Reads only.
fn due_agents(root: &Path, version: &str) -> Vec<Agent> {
    let now = crate::plugin_update::now();
    ALL_AGENTS
        .into_iter()
        .filter(|&a| has_stale_records(root, a, version))
        .filter(|&a| claim_due(read_claim(root, a).as_deref(), version, now))
        .collect()
}

/// Whether [`refresh`] would do work now. Reads only, for the session hook.
pub fn refresh_needed(home: &Path) -> bool {
    let root = crate::common::paths::playbook_root_from(home);
    std::panic::catch_unwind(|| {
        resolve_plugin(home).is_some_and(|(_, version)| !due_agents(&root, &version).is_empty())
    })
    .unwrap_or(false)
}

/// Brings the copies of already synced agents up to the plugin version.
/// An agent with no records is never touched. A dry run plans only: no
/// claim, no writes. A panic inside is dropped, so a touch point never fails
/// because of it.
pub fn refresh(home: &Path, dry_run: bool) -> Vec<String> {
    refresh_report(home, dry_run).lines
}

/// [`refresh`] with the outcome, for the caller that stores a notice.
pub fn refresh_report(home: &Path, dry_run: bool) -> Refreshed {
    std::panic::catch_unwind(|| refresh_inner(home, dry_run)).unwrap_or_default()
}

fn refresh_inner(home: &Path, dry_run: bool) -> Refreshed {
    let mut out = Refreshed {
        lines: Vec::new(),
        clean: true,
    };
    let root = crate::common::paths::playbook_root_from(home);
    let Some((plugin, version)) = resolve_plugin(home) else {
        return out;
    };
    let agents = due_agents(&root, &version);
    if agents.is_empty() {
        return out;
    }
    let rendered = plan::plan(&plugin).and_then(|targets| render_targets(&plugin, &targets));
    for agent in agents {
        if !try_claim(&root, agent, &version, dry_run, crate::plugin_update::now()) {
            continue;
        }
        let (lines, clean) = match &rendered {
            Ok(folders) => {
                let report = apply::apply_refresh(
                    &root,
                    agent,
                    &home.join(TARGET_DIR),
                    &version,
                    folders,
                    dry_run,
                );
                (report.lines.clone(), report.is_clean())
            }
            Err(why) => (vec![format!("failed {}: {why}", agent.key())], false),
        };
        if !dry_run {
            let state = if clean {
                crate::plugin_update::State::Done
            } else {
                crate::plugin_update::State::Failed
            };
            write_claim(&root, agent, &version, state);
        }
        out.clean &= clean;
        out.lines.extend(lines);
    }
    out
}

/// The line the next session shows for a finished background refresh.
pub fn notice_line(refreshed: &Refreshed) -> Option<String> {
    let summary = refreshed.lines.last()?;
    // A release that left the skills as they were is not worth a line.
    if refreshed.clean && summary == apply::NOTHING_TO_CHANGE {
        return None;
    }
    Some(if refreshed.clean {
        format!("playbook: refreshed the Codex skills ({summary}).")
    } else {
        format!(
            "playbook: refreshing the Codex skills did not finish ({summary}). \
            Run `playbook skills sync` to see why."
        )
    })
}

/// Renders every planned target from the plugin folder into the files to
/// write: markdown gets its references rewritten, `SKILL.md` and commands
/// are rendered and split when large, and other files are copied as they are.
pub fn render_targets(
    plugin: &Path,
    targets: &[plan::Target],
) -> Result<Vec<apply::RenderedFolder>, String> {
    let known: BTreeSet<String> = targets
        .iter()
        .filter_map(|t| t.name.strip_prefix(plan::NAME_PREFIX))
        .map(str::to_string)
        .collect();
    targets
        .iter()
        .map(|target| render_target(plugin, target, &known))
        .collect()
}

fn render_target(
    plugin: &Path,
    target: &plan::Target,
    known: &BTreeSet<String>,
) -> Result<apply::RenderedFolder, String> {
    let mut files: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    for (rel, source) in &target.files {
        let path = plugin.join(rel);
        let bytes = fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let is_md = rel.extension().and_then(|e| e.to_str()) == Some("md");
        match source {
            plan::Source::Command => {
                let text = utf8(&path, bytes)?;
                let md = render::render_command(&text, &target.name, known)?;
                push_split(&mut files, PathBuf::from(SKILL_FILE), &md);
            }
            plan::Source::SkillFile => {
                // Skill files sit under skills/<name>/, the folder keeps the rest.
                let inner: PathBuf = rel.components().skip(2).collect();
                if inner == Path::new(SKILL_FILE) {
                    let md = render::render_skill_md(&utf8(&path, bytes)?, &target.name, known)?;
                    push_split(&mut files, inner, &md);
                } else if is_md {
                    let text = render::rewrite_refs(&utf8(&path, bytes)?, known);
                    files.push((inner, text.into_bytes()));
                } else {
                    files.push((inner, bytes));
                }
            }
        }
    }
    Ok(apply::RenderedFolder {
        name: target.name.clone(),
        files,
    })
}

fn push_split(files: &mut Vec<(PathBuf, Vec<u8>)>, skill_path: PathBuf, md: &str) {
    let (stub, body) = render::split_if_large(md);
    files.push((skill_path, stub.into_bytes()));
    if let Some(body) = body {
        files.push((PathBuf::from(render::BODY_PATH), body.into_bytes()));
    }
}

fn utf8(path: &Path, bytes: Vec<u8>) -> Result<String, String> {
    String::from_utf8(bytes).map_err(|_| format!("{} is not valid UTF-8 text", path.display()))
}

/// The agents found from explicit inputs, so tests never read the real
/// environment. Codex counts when `$home/.codex` is a folder or a `codex`
/// program is on `path`.
pub fn detect(home: &Path, path: &str) -> Vec<Agent> {
    let mut found = Vec::new();
    if home.join(".codex").is_dir() || on_path("codex", path) {
        found.push(Agent::Codex);
    }
    found
}

fn on_path(program: &str, path: &str) -> bool {
    std::env::split_paths(path).any(|dir| dir.join(program).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;
    use std::fs;

    fn empty_home(tag: &str) -> std::path::PathBuf {
        let home = scratch_dir(tag);
        fs::create_dir_all(&home).unwrap();
        home
    }

    #[test]
    fn render_targets_renders_markdown_and_copies_other_files() {
        // Arrange
        let plugin = empty_home("render-targets");
        let skill = plugin.join("skills/fix");
        fs::create_dir_all(skill.join("references")).unwrap();
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: fix\n---\nsee /playbook:fix\n",
        )
        .unwrap();
        fs::write(skill.join("references/x.bin"), [0u8, 159, 255]).unwrap();
        let targets = plan::plan(&plugin).unwrap();

        // Act
        let folders = render_targets(&plugin, &targets).unwrap();

        // Assert
        let files: std::collections::BTreeMap<_, _> = folders[0].files.iter().cloned().collect();
        let md = String::from_utf8(files[Path::new("SKILL.md")].clone()).unwrap();
        assert!(
            md.contains("name: playbook-fix") && md.contains("$playbook-fix"),
            "{md}"
        );
        assert_eq!(files[Path::new("references/x.bin")], vec![0u8, 159, 255]);
    }

    const NOW: u64 = 1_000_000;
    const HOUR: u64 = 3600;

    /// A home with a version 2.0.0 plugin installed and a codex folder
    /// record written for 1.0.0, so a refresh is due until a claim says not.
    fn refresh_fixture(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let home = empty_home(tag);
        let dir = home.join(".claude/plugins/cache/amplifiedengineer/playbook/2.0.0");
        fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        fs::create_dir_all(dir.join("skills")).unwrap();
        fs::write(
            dir.join(".claude-plugin/plugin.json"),
            r#"{"name":"playbook","version":"2.0.0"}"#,
        )
        .unwrap();
        fs::write(
            home.join(".claude/plugins/installed_plugins.json"),
            format!(
                r#"{{"plugins":{{"playbook@amplifiedengineer":[{{"scope":"user","installPath":"{}"}}]}}}}"#,
                dir.display()
            ),
        )
        .unwrap();
        let root = crate::common::paths::playbook_root_from(&home);
        crate::state::set(
            &root,
            "skills-sync/codex/playbook-fix",
            "version 1.0.0\nstate done\n",
        )
        .unwrap();
        (home, dir)
    }

    #[test]
    fn the_refresh_claim_follows_the_marker_rules() {
        // Arrange
        let claim = |state: &str, age: u64| format!("2.0.0 {state} {}", NOW - age);
        let cases = [
            (None, true),
            (Some(claim("done", 0)), false),
            (Some(claim("done", 99 * HOUR)), false),
            (Some(claim("failed", 24 * HOUR)), false),
            (Some(claim("failed", 24 * HOUR + 1)), true),
            (Some(claim("running", 10 * 60)), false),
            (Some(claim("running", 10 * 60 + 1)), true),
            (Some(format!("1.0.0 done {NOW}")), true),
            (Some("garbage".to_string()), true),
        ];

        for (text, due) in cases {
            // Act
            let got = claim_due(text.as_deref(), "2.0.0", NOW);

            // Assert
            assert_eq!(got, due, "{text:?}");
        }
    }

    #[test]
    fn a_claim_moves_once_for_callers_that_read_the_same_value() {
        // Arrange
        let root = empty_home("refresh-claim");

        // Act
        let first = take_claim(&root, Agent::Codex, None, "2.0.0", NOW);
        let second = take_claim(&root, Agent::Codex, None, "2.0.0", NOW);

        // Assert
        assert!(first);
        assert!(!second);
    }

    #[test]
    fn a_claim_finished_since_the_read_is_not_taken_again() {
        // Arrange
        let root = empty_home("refresh-claim-finished");
        crate::state::set(
            &root,
            &refresh_key(Agent::Codex),
            &format!("2.0.0 done {NOW}"),
        )
        .unwrap();

        // Act
        let real = try_claim(&root, Agent::Codex, "2.0.0", false, NOW);
        let dry = try_claim(&root, Agent::Codex, "2.0.0", true, NOW);

        // Assert
        assert!(!real && !dry);
        assert_eq!(
            read_claim(&root, Agent::Codex),
            Some(format!("2.0.0 done {NOW}"))
        );
    }

    #[test]
    fn a_due_claim_is_taken_unless_it_is_a_dry_run() {
        // Arrange
        let root = empty_home("refresh-claim-due");

        // Act
        let dry = try_claim(&root, Agent::Codex, "2.0.0", true, NOW);
        let unchanged = read_claim(&root, Agent::Codex);
        let real = try_claim(&root, Agent::Codex, "2.0.0", false, NOW);

        // Assert
        assert!(dry && real);
        assert_eq!(unchanged, None);
        assert_eq!(
            read_claim(&root, Agent::Codex),
            Some(format!("2.0.0 running {NOW}"))
        );
    }

    #[test]
    fn the_hook_check_waits_a_day_after_a_failed_claim() {
        // Arrange
        let (home, _plugin) = refresh_fixture("refresh-needed");
        let root = crate::common::paths::playbook_root_from(&home);
        let now = crate::plugin_update::now();
        let claim = |at: u64| format!("2.0.0 failed {at}");

        // Act
        crate::state::set(&root, &refresh_key(Agent::Codex), &claim(now)).unwrap();
        let fresh = refresh_needed(&home);
        let past = now - 24 * HOUR - 1;
        crate::state::set(&root, &refresh_key(Agent::Codex), &claim(past)).unwrap();
        let retry = refresh_needed(&home);

        // Assert
        assert!(!fresh);
        assert!(retry);
    }

    #[test]
    fn only_records_for_another_version_are_stale() {
        // Arrange
        let root = empty_home("stale-records");
        crate::state::set(
            &root,
            "skills-sync/codex/playbook-fix",
            "version 1.0.0\nstate done\n",
        )
        .unwrap();
        // Would read as stale if the claim row were not filtered out.
        crate::state::set(
            &root,
            &refresh_key(Agent::Codex),
            "version 9.9.9\nstate done\n",
        )
        .unwrap();

        // Act
        let behind = has_stale_records(&root, Agent::Codex, "2.0.0");
        let current = has_stale_records(&root, Agent::Codex, "1.0.0");

        // Assert
        assert!(behind);
        assert!(!current, "the claim row is not a folder record");
    }

    #[test]
    fn a_codex_folder_alone_finds_codex() {
        // Arrange
        let home = empty_home("detect-folder");
        let bin = empty_home("detect-folder-bin");
        fs::create_dir(home.join(".codex")).unwrap();

        // Act
        let found = detect(&home, bin.to_str().unwrap());

        // Assert
        assert_eq!(found, vec![Agent::Codex]);
    }

    #[test]
    fn a_codex_program_on_the_given_path_alone_finds_codex() {
        // Arrange
        let home = empty_home("detect-path-home");
        let bin = empty_home("detect-path-bin");
        fs::write(bin.join("codex"), "").unwrap();

        // Act
        let found = detect(&home, bin.to_str().unwrap());

        // Assert
        assert_eq!(found, vec![Agent::Codex]);
    }

    #[test]
    fn neither_signal_finds_nothing() {
        // Arrange
        let home = empty_home("detect-none-home");
        let bin = empty_home("detect-none-bin");

        // Act
        let found = detect(&home, bin.to_str().unwrap());

        // Assert
        assert!(found.is_empty());
    }
}
