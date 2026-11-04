// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook setup --dry-run`: what a run would change, part by part, with
//! nothing written to the real files. It copies the few small files the parts
//! edit into a scratch home, runs the same step code against that copy and
//! reports the difference. The scratch home is removed afterwards.

use crate::init::attribution::Choice as Attribution;
use crate::init::catalog;
use crate::init::choices::Choices;
use crate::init::path;
use crate::init::run::{self, InitPaths, StepReport, StepStatus};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Home-relative files the parts can edit. Each is copied when it exists.
const TRACKED: &[&str] = &[
    ".claude/settings.json",
    ".claude/.settings.base.json",
    ".zshrc",
    ".zshenv",
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".profile",
    ".config/fish/conf.d/playbook.fish",
];

/// The part a top-level settings key belongs to.
fn part_of_key(key: &str) -> &'static str {
    match key {
        "hooks" => "hooks",
        "statusLine" => "statusline",
        "permissions" => "security",
        "attribution" => "attribution",
        _ => "hooks", // a key no part claims, never shipped
    }
}

fn read(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

/// A home-relative path as the user knows it.
fn tilde(rel: &str) -> String {
    format!("~/{rel}")
}

/// (event, matcher, command) of every hook entry in a settings value.
fn hook_entries(settings: &Value) -> BTreeSet<(String, String, String)> {
    let mut out = BTreeSet::new();
    let Some(hooks) = settings.get("hooks").and_then(Value::as_object) else {
        return out;
    };
    for (event, groups) in hooks {
        for group in groups.as_array().into_iter().flatten() {
            let matcher = group["matcher"].as_str().unwrap_or("").to_string();
            for hook in group["hooks"].as_array().into_iter().flatten() {
                let command = hook["command"].as_str().unwrap_or("").to_string();
                out.insert((event.clone(), matcher.clone(), command));
            }
        }
    }
    out
}

/// What changed in settings.json, one `(part, text)` pair per change.
pub fn settings_changes(before: &Value, after: &Value) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let empty = serde_json::Map::new();
    let (b, a) = (
        before.as_object().unwrap_or(&empty),
        after.as_object().unwrap_or(&empty),
    );
    for (key, new) in a {
        let old = b.get(key);
        if old == Some(new) {
            continue;
        }
        match (key.as_str(), old) {
            ("hooks", _) => {
                let was = hook_entries(before);
                let added = hook_entries(after).difference(&was).count();
                out.push(("hooks", format!("add {added} hook entries")));
            }
            ("env", old_env) => {
                // Only the telemetry part writes `env` entries.
                let no_env = serde_json::Map::new();
                let old_env = old_env.and_then(Value::as_object).unwrap_or(&no_env);
                for (k, v) in new.as_object().into_iter().flatten() {
                    if old_env.get(k) != Some(v) {
                        let verb = if old_env.contains_key(k) {
                            "change"
                        } else {
                            "add"
                        };
                        out.push(("telemetry", format!("{verb} env.{k}")));
                    }
                }
            }
            (_, None) => out.push((part_of_key(key), format!("add {key}"))),
            (_, Some(_)) => out.push((part_of_key(key), format!("change {key}"))),
        }
    }
    out
}

/// One line per kind of change for a part: `add keys: a, b`, `change keys: c`.
fn grouped(changes: &[(&'static str, String)], id: &str, file: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for verb in ["add", "change"] {
        let names: Vec<&str> = changes
            .iter()
            .filter(|(p, _)| *p == id)
            .filter_map(|(_, t)| t.strip_prefix(&format!("{verb} ")))
            .collect();
        match names.as_slice() {
            [] => {}
            [one] if one.ends_with("hook entries") => lines.push(format!("{file}: {verb} {one}")),
            many => lines.push(format!("{file}: {verb} {}", many.join(", "))),
        }
    }
    lines
}

/// Lines present after and not before, in order.
fn added_lines(before: &str, after: &str) -> Vec<String> {
    let had: BTreeSet<&str> = before.lines().collect();
    after
        .lines()
        .filter(|l| !l.trim().is_empty() && !had.contains(l))
        .map(str::to_string)
        .collect()
}

fn verb(step: &StepReport) -> &'static str {
    match step.status {
        StepStatus::Wired => "would change",
        StepStatus::AlreadyCorrect => "already up to date",
        StepStatus::Skipped => "skipped",
        StepStatus::Failed => "would fail",
    }
}

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn make_scratch(home: &Path) -> std::io::Result<Scratch> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!("playbook-dry-run-{}-{nanos}", std::process::id()));
    fs::create_dir_all(dir.join(".claude"))?;
    for rel in TRACKED {
        let from = home.join(rel);
        if from.is_file() {
            let to = dir.join(rel);
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&from, &to)?;
        }
    }
    Ok(Scratch(dir))
}

/// The report for `--dry-run`. Writes nothing outside a scratch directory it
/// removes before returning.
pub fn dry_run(paths: &InitPaths, choices: &Choices) -> String {
    let mut out = String::from("playbook setup --dry-run: nothing is changed on this run.\n\n");
    let scratch = match make_scratch(&paths.home) {
        Ok(s) => s,
        Err(err) => return format!("{out}could not prepare a scratch copy: {err}\n"),
    };
    let sh = scratch.0.clone();
    let scratch_paths = InitPaths {
        plugin_root: paths.plugin_root.clone(),
        self_root: paths.self_root.clone(),
        claude_home: sh.join(".claude"),
        home: sh.clone(),
        system_prompt: crate::init::system_prompt::Answer::Unasked,
        hooks: paths.hooks,
        statusline: paths.statusline,
        security: paths.security,
        telemetry: paths.telemetry,
        telemetry_replace_off: paths.telemetry_replace_off,
        only: paths.only,
        short_name: paths.short_name,
        policy_guard: false,
        codex_hooks: false,
        pb_target: paths.pb_target.clone(),
        attribution: paths.attribution,
        completions: paths.completions,
        path_setup: paths.path_setup.as_ref().map(|s| path::Setup {
            bin_dir: s.bin_dir.clone(),
            shell: s.shell,
        }),
        repo: None,
    };
    let epoch = crate::common::time::now_epoch_secs();
    let (settings_step, hooks_step) = run::settings_and_hooks_steps(&scratch_paths, epoch);
    let telemetry_step = run::telemetry_step(&scratch_paths, epoch);
    let attribution_step = run::attribution_step(&scratch_paths, epoch);
    let short_line =
        crate::init::pb_link::preview(scratch_paths.short_name, scratch_paths.pb_target.as_ref());
    let path_step = run::path_step(&sh, scratch_paths.path_setup.as_ref());
    let after_path = rc_snapshot(&sh);
    let completions_step = run::completions_step(&sh, scratch_paths.completions.as_ref());
    let after_completions = rc_snapshot(&sh);
    let before_rc = rc_snapshot(&paths.home);

    let settings_rel = ".claude/settings.json";
    let parse = |p: &Path| -> Value {
        read(p)
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null)
    };
    let changes = settings_changes(
        &parse(&paths.home.join(settings_rel)),
        &parse(&sh.join(settings_rel)),
    );
    let settings_file = tilde(settings_rel);

    let on = |b: bool| if b { "on" } else { "off" };
    let flags = [
        ("hooks", choices.hooks),
        ("statusline", choices.statusline),
        ("security", choices.security),
        ("path", choices.path),
        ("system-prompt", choices.system_prompt),
        ("telemetry", choices.telemetry),
        ("short-name", choices.short_name),
        ("completions", choices.completions),
        ("policy-guard", choices.policy_guard),
        ("attribution", paths.attribution != Attribution::Skip),
        ("codex-hooks", choices.codex_hooks),
    ];
    for (id, enabled) in flags {
        if paths.only && !enabled {
            continue;
        }
        let state = match (id, paths.attribution) {
            ("attribution", Attribution::On) => crate::init::attribution::ON_LABEL,
            ("attribution", Attribution::Skip) => "left as it is",
            ("attribution", _) => crate::init::attribution::OFF_LABEL,
            _ => on(enabled),
        };
        let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{id} ({state})\n"));
        if !enabled {
            out.push_str(match id {
                "attribution" => "  skipped, no flag and no answer\n\n",
                _ => "  skipped, you chose off\n\n",
            });
            continue;
        }
        let lines: Vec<String> = match id {
            "hooks" | "statusline" | "security" => {
                let mine = grouped(&changes, id, &settings_file);
                if mine.is_empty() {
                    let step = if id == "hooks" {
                        &hooks_step
                    } else {
                        &settings_step
                    };
                    vec![format!(
                        "{settings_file}: {}{}",
                        if step.status == StepStatus::Skipped {
                            "skipped, "
                        } else {
                            "no change, "
                        },
                        if id == "hooks" {
                            step.detail.as_str()
                        } else {
                            "already as configured"
                        }
                    )]
                } else {
                    mine
                }
            }
            "telemetry" => own_key_lines(&changes, id, &settings_file, &telemetry_step),
            "attribution" => own_key_lines(&changes, id, &settings_file, &attribution_step),
            "short-name" => vec![format!("pb: {short_line}")],
            "completions" => {
                completions_lines(paths, &completions_step, &after_path, &after_completions)
            }
            "policy-guard" => vec![policy_guard_line(&paths.home)],
            "codex-hooks" => vec![codex_hooks_line(&paths.home)],
            "path" => file_lines(&before_rc, &after_path, &path_step),
            _ => vec![system_prompt_line(paths)],
        };
        for line in lines {
            let _ = std::fmt::Write::write_fmt(&mut out, format_args!("  {line}\n"));
        }
        out.push('\n');
    }
    if !paths.only {
        out.push_str("Runs every time\n");
        for line in always_lines(paths) {
            let _ = std::fmt::Write::write_fmt(&mut out, format_args!("  {line}\n"));
        }
    }
    out.push_str(
        "\nA backup of settings.json, and of an rc file setup edits, is saved before any change. One-time migrations are not simulated.\n",
    );
    if paths.self_root.is_none() {
        out.push_str(
            "No plugin or settings template was found. A real run writes embedded copies under ~/.config/playbook/embedded first.\n",
        );
    }
    out
}

/// What the codex-hooks part would do. Writes nothing.
fn codex_hooks_line(home: &Path) -> String {
    use crate::init::codex_hooks::{audit, hooks_path};
    if !run::codex_found(home) {
        return "skipped, Codex was not found".to_string();
    }
    match audit(&hooks_path(home)).missing.len() {
        0 => "~/.codex/hooks.json already has the guards".to_string(),
        n => format!("adds {n} guard entries to ~/.codex/hooks.json"),
    }
}

/// What the policy-guard part would do. Writes nothing.
fn policy_guard_line(home: &Path) -> String {
    use crate::hooks::policy_guard::{stored_enabled, ENABLED_KEY};
    match stored_enabled(home) {
        true => format!("{ENABLED_KEY} is already true"),
        false => format!("stores {ENABLED_KEY} true in ~/.config/playbook"),
    }
}

/// The settings.json lines for a part that writes its own key: what changed,
/// else the step's own detail.
fn own_key_lines(
    changes: &[(&'static str, String)],
    id: &str,
    file: &str,
    step: &StepReport,
) -> Vec<String> {
    let mine = grouped(changes, id, file);
    if mine.is_empty() {
        vec![format!("{file}: {}", step.detail)]
    } else {
        mine
    }
}

/// The contents of every tracked shell file that exists, by relative path.
fn rc_snapshot(home: &Path) -> Vec<(&'static str, String)> {
    TRACKED
        .iter()
        .filter(|r| !r.starts_with(".claude/"))
        .map(|r| (*r, read(&home.join(r)).unwrap_or_default()))
        .collect()
}

/// Files a rc-style step would edit, with the lines it would add.
fn file_lines(
    before: &[(&'static str, String)],
    after: &[(&'static str, String)],
    step: &StepReport,
) -> Vec<String> {
    let mut lines = Vec::new();
    for ((rel, old), (_, new)) in before.iter().zip(after) {
        for add in added_lines(old, new) {
            lines.push(format!("{}: add `{add}`", tilde(rel)));
        }
    }
    if lines.is_empty() {
        lines.push(format!("{}: {}", verb(step), step.detail));
    }
    lines
}

/// The files the part would write, and the line it would add to a shell
/// startup file.
fn completions_lines(
    paths: &InitPaths,
    step: &StepReport,
    before: &[(&'static str, String)],
    after: &[(&'static str, String)],
) -> Vec<String> {
    let Some(setup) = paths.completions else {
        return vec![format!("{}: {}", verb(step), step.detail)];
    };
    use crate::init::completion::{self, Shell};
    let mut lines: Vec<String> = completion::wired_files(&paths.home, setup.shell)
        .iter()
        .filter_map(|(file, text)| {
            let rel = file.strip_prefix(&paths.home).ok()?.to_str()?;
            Some(match read(file) {
                None => format!("{}: add the file", tilde(rel)),
                Some(have) if have == *text => format!("{}: already up to date", tilde(rel)),
                Some(have)
                    if setup.shell == Shell::Zsh && completion::loader_is_ours(have.as_bytes()) =>
                {
                    format!("{}: rewrite the file", tilde(rel))
                }
                Some(_) => format!("{}: left as it is", tilde(rel)),
            })
        })
        .collect();
    let rc_changes = file_lines(before, after, step);
    if rc_changes.iter().any(|l| l.contains(": add `")) {
        lines.extend(rc_changes);
    } else if lines.is_empty() {
        lines.push(format!("{}: {}", verb(step), step.detail));
    }
    lines
}

fn system_prompt_line(paths: &InitPaths) -> String {
    let rel = ".config/playbook/prompts/SYSTEM_PROMPT.md";
    let dest = paths.home.join(rel);
    let shown = tilde(rel);
    let source = paths
        .self_root
        .as_ref()
        .map(|r| r.join("prompts/SYSTEM_PROMPT.md"))
        .and_then(|p| read(&p));
    match (source, read(&dest)) {
        (None, _) => format!("{shown}: skipped, no system prompt found to copy"),
        (Some(_), None) => format!("{shown}: add the file"),
        (Some(s), Some(d)) if s == d => format!("{shown}: already up to date"),
        (Some(_), Some(_)) => {
            format!("{shown}: refresh the file, or keep your copy when you edited it")
        }
    }
}

fn always_lines(paths: &InitPaths) -> Vec<String> {
    let root = crate::common::paths::playbook_root_from(&paths.home);
    let old = paths.home.join(".claude/memory");
    let mut lines = Vec::new();
    for part in catalog::PARTS.iter().filter(|p| !p.asked) {
        let text = match part.id {
            "memory-migration" => {
                if old.is_dir() && !root.join("memory/.migration-complete").exists() {
                    "~/.claude/memory moves to ~/.config/playbook/memory".to_string()
                } else {
                    "nothing to move".to_string()
                }
            }
            "trust" => {
                let json = paths.home.join(".claude.json");
                let dir = root.to_string_lossy().to_string();
                if !json.exists() {
                    "~/.claude.json does not exist, skipped".to_string()
                } else if crate::trust::is_trusted(&json, &dir) {
                    "already trusted".to_string()
                } else {
                    "~/.claude.json: mark ~/.config/playbook as trusted".to_string()
                }
            }
            "config-hash" => {
                if root.join("hooks/lib/config-hash.sh").is_file() {
                    "remove ~/.config/playbook/hooks/lib/config-hash.sh".to_string()
                } else {
                    "nothing to remove".to_string()
                }
            }
            "security" => continue,
            other => format!("{other}: runs"),
        };
        lines.push(format!("{}: {text}", part.id));
    }
    let retired = crate::init::rcfile::retired_rc_files(&paths.home);
    if !retired.is_empty() {
        let names: Vec<String> = retired.iter().map(|f| f.display().to_string()).collect();
        lines.push(format!(
            "rc-line: {}: remove the retired launcher lines, after a backup",
            names.join(", ")
        ));
    }
    lines
}
