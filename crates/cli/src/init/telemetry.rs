// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `telemetry` part of `playbook setup`: with consent, writes the
//! OpenTelemetry variables that point Claude Code at the local usage
//! collector into the `env` block of `settings.json`, so plain `claude`, the
//! IDE and the desktop app send telemetry too, not only `cc`.
//!
//! It never overrides: when the environment or any Claude Code settings file
//! already names a telemetry variable, nothing is written. The one exception
//! is a yes to "turn it on" when the only telemetry variable anywhere is an
//! off `CLAUDE_CODE_ENABLE_TELEMETRY` in the user's own `settings.json`: that
//! switch is replaced, and the old value is kept in the record. What it
//! writes is recorded in `~/.config/playbook/telemetry-env.json`, and
//! `uninstall` removes exactly those keys, only while they still hold the
//! value written, and puts back any value it replaced. Prompt, tool detail, tool content and raw body logging are never
//! turned on, and the account uuid stays out of the metrics.

use crate::init::run::StepReport;
use serde_json::{Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

fn marker_path(home: &Path) -> PathBuf {
    crate::common::paths::playbook_root_from(home).join("telemetry-env.json")
}

pub(crate) fn load(path: &Path) -> Result<Map<String, Value>, String> {
    if !path.is_file() {
        return Ok(Map::new());
    }
    let text =
        fs::read_to_string(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => Ok(map),
        _ => Err(format!("{} is not a JSON object", path.display())),
    }
}

/// Copies `path` to `.bak.<epoch>` (kept when one exists) and writes `root`.
pub(crate) fn write_with_backup(
    path: &Path,
    root: Map<String, Value>,
    epoch: u64,
) -> Result<(), String> {
    if path.is_file() {
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let backup = path.with_file_name(format!("{name}.bak.{epoch}"));
        if !backup.exists() {
            fs::copy(path, &backup)
                .map_err(|e| format!("could not back up {}: {e}", path.display()))?;
        }
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    let text = format!("{:#}\n", Value::Object(root));
    // A new file holds a bearer token, so it starts owner-only; an existing
    // file keeps the mode it has.
    let written = if path.is_file() {
        crate::common::atomic::write_atomic(path, text)
    } else {
        crate::common::atomic::write_atomic_private(path, text)
    };
    written.map_err(|e| format!("could not write {}: {e}", path.display()))
}

/// Writes `root` as [`write_with_backup`] does, sorting its keys first when
/// `was_sorted`: the settings merge writes them sorted, and a file that is
/// sorted stays sorted, so the next merge finds nothing to reorder.
pub(crate) fn write_keeping_order(
    path: &Path,
    mut root: Map<String, Value>,
    was_sorted: bool,
    epoch: u64,
) -> Result<(), String> {
    if was_sorted {
        let mut entries: Vec<(String, Value)> = std::mem::take(&mut root).into_iter().collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        root.extend(entries);
    }
    write_with_backup(path, root, epoch)
}

/// Whether the record exists and settings.json still holds every value in it.
fn already_written(home: &Path, settings_path: &Path) -> bool {
    let Ok(written) = load(&marker_path(home)) else {
        return false;
    };
    if written.is_empty() {
        return false;
    }
    let Ok(root) = load(settings_path) else {
        return false;
    };
    let env = root.get("env").and_then(Value::as_object);
    written
        .iter()
        .filter(|(k, _)| *k != REPLACED)
        .all(|(k, v)| env.is_some_and(|e| e.get(k) == Some(v)))
}

/// The marker key that holds the values init replaced, by variable name.
const REPLACED: &str = "_replaced";

/// Whether Claude Code telemetry is on, off or not mentioned, as the process
/// environment and every Claude Code settings file read together say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// `CLAUDE_CODE_ENABLE_TELEMETRY` is on.
    Enabled,
    /// Telemetry is set up some other way (variables without an on switch, or
    /// a settings file that cannot be read). Init leaves it alone.
    SetUpByYou,
    /// `CLAUDE_CODE_ENABLE_TELEMETRY` is set to an off value.
    Disabled,
    /// No telemetry variable anywhere.
    Unset,
}

/// The current telemetry state. `cwd` is where project settings are looked up.
/// `with_process` also reads the process environment; tests pass false so the
/// machine they run on cannot change the answer.
pub fn state(home: &Path, cwd: &Path, with_process: bool) -> State {
    use crate::cc::telemetry::Configured;
    let found = if with_process {
        crate::cc::telemetry::configured(home, cwd, None)
    } else {
        crate::usage::telemetry_detect::configured_from_files(home, cwd, None)
    };
    match found {
        Configured::Nothing => State::Unset,
        Configured::Disabled => State::Disabled,
        Configured::Playbook => State::Enabled,
        Configured::Other => {
            let vars = telemetry_vars(home, cwd, with_process);
            match vars
                .as_ref()
                .and_then(|v| v.get("CLAUDE_CODE_ENABLE_TELEMETRY"))
            {
                Some(_) => State::Enabled,
                None => State::SetUpByYou,
            }
        }
    }
}

/// The merged telemetry variables, `None` when a settings file cannot be read.
fn telemetry_vars(
    home: &Path,
    cwd: &Path,
    with_process: bool,
) -> Option<std::collections::BTreeMap<String, String>> {
    crate::usage::telemetry_detect::merged_vars(home, cwd, with_process)
}

/// The one line init prints when telemetry is already on and so asks nothing.
pub const ENABLED_LINE: &str =
    "telemetry: already enabled in your environment or Claude Code settings, so nothing to ask";

/// The line when telemetry is set up some other way and init leaves it alone.
pub const SET_UP_LINE: &str =
    "telemetry: already set up by you, left as it is (`playbook usage status` shows how it is read)";

/// The line when telemetry is off and init cannot turn it on without
/// overriding a setting of yours.
pub const OFF_ELSEWHERE_LINE: &str =
    "telemetry: switched off by a setting that `playbook setup` will not change (`playbook usage status` shows where), so nothing to ask";

/// The line init prints before the telemetry question is skipped, or `None`
/// when the question should be asked.
pub fn skip_line(
    home: &Path,
    claude_home: &Path,
    cwd: &Path,
    process_configured: bool,
) -> Option<&'static str> {
    match state(home, cwd, true) {
        State::Enabled => Some(ENABLED_LINE),
        State::SetUpByYou => Some(SET_UP_LINE),
        State::Unset => None,
        State::Disabled => (!can_replace_off_switch(home, claude_home, cwd, process_configured))
            .then_some(OFF_ELSEWHERE_LINE),
    }
}

/// Whether a yes may replace the off switch: telemetry is off, the process
/// environment names no telemetry variable, the user's own settings.json holds
/// only that off switch, and no other settings file names a telemetry variable.
fn can_replace_off_switch(
    home: &Path,
    claude_home: &Path,
    cwd: &Path,
    process_configured: bool,
) -> bool {
    let user_file = claude_home.join("settings.json");
    !process_configured
        && state(home, cwd, false) == State::Disabled
        && only_an_off_switch(&user_file)
        && !crate::usage::telemetry_detect::files_configured_except(home, cwd, &user_file)
}

/// Whether the only telemetry variable in `settings_path`'s env block is an
/// off `CLAUDE_CODE_ENABLE_TELEMETRY`, the one case where a yes may replace a
/// value the user set (it is the exact thing they were asked about).
fn only_an_off_switch(settings_path: &Path) -> bool {
    let Ok(root) = load(settings_path) else {
        return false;
    };
    let Some(env) = root.get("env").and_then(Value::as_object) else {
        return false;
    };
    // Read the value as the detector does: a JSON 0 or false is "0" or "false".
    let off = env.get("CLAUDE_CODE_ENABLE_TELEMETRY").is_some_and(|v| {
        let text = match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        matches!(
            text.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false"
        )
    });
    off && env
        .keys()
        .filter(|k| *k == "CLAUDE_CODE_ENABLE_TELEMETRY" || k.starts_with("OTEL_"))
        .count()
        == 1
}

/// Whether the process environment names a telemetry variable that the user's
/// own settings.json does not also name with the same value. Claude Code puts
/// the `env` block of its settings into the environment of the commands it
/// runs, so `playbook setup` run from inside a session sees a copy of the
/// settings it is about to read, and that copy is not a second setup.
pub fn process_configured_beyond_settings(claude_home: &Path) -> bool {
    let user_env = load(&claude_home.join("settings.json"))
        .ok()
        .and_then(|root| root.get("env").and_then(Value::as_object).cloned())
        .unwrap_or_default();
    std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v)))
        .filter(|(k, _)| crate::usage::telemetry_detect::is_telemetry_var(k))
        .any(|(k, v)| {
            // A value that is not valid text cannot match, so it counts as the
            // user's own.
            let mirrored = v.to_str().is_some_and(|v| {
                user_env.get(&k).is_some_and(|s| match s {
                    Value::String(s) => s == v,
                    other => {
                        let text = other.to_string();
                        text == v
                    }
                })
            });
            !mirrored
        })
}

/// The lines the wizard shows for the telemetry part: the exact keys that
/// change, and what each switch that is left alone does. `claude_home` is the
/// folder that holds the user's settings.json.
pub fn plan_lines(claude_home: &Path) -> Vec<String> {
    let env = load(&claude_home.join("settings.json"))
        .ok()
        .and_then(|root| root.get("env").and_then(Value::as_object).cloned())
        .unwrap_or_default();
    let text = |v: &Value| match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let mut lines = Vec::new();
    match env.get("CLAUDE_CODE_ENABLE_TELEMETRY") {
        Some(old) if text(old).trim() == "1" => {
            lines.push("Stays: CLAUDE_CODE_ENABLE_TELEMETRY is already 1".to_string());
        }
        Some(old) => lines.push(format!(
            "Changes: CLAUDE_CODE_ENABLE_TELEMETRY {} -> 1 (CLAUDE_CODE_ENABLE_TELEMETRY is the switch that lets Claude Code export OpenTelemetry data to your own collector)",
            text(old)
        )),
        None => lines.push(
            "Adds: CLAUDE_CODE_ENABLE_TELEMETRY=1 (the switch that lets Claude Code export OpenTelemetry data to your own collector)"
                .to_string(),
        ),
    }
    lines.push(format!(
        "Also adds: {} (where to send it and how to log in to the local collector)",
        crate::cc::telemetry::env_for(&crate::usage::collector::State::default())
            .iter()
            .map(|(k, _)| k.as_str())
            .filter(|k| *k != "CLAUDE_CODE_ENABLE_TELEMETRY")
            .collect::<Vec<_>>()
            .join(", ")
    ));
    for name in ["DISABLE_TELEMETRY", "DO_NOT_TRACK"] {
        if let Some(v) = env.get(name) {
            lines.push(format!(
                "Stays as it is: {name}={}. It turns off Anthropic's own analytics and error reporting. Claude Code documents it as separate from OpenTelemetry export, so it does not block this.",
                text(v)
            ));
        }
    }
    lines
}

/// The init step. `cwd` is where project settings are looked up.
/// `replace_off_ok` is whether a lone off switch the user set may be replaced
/// (an answer to the question or an explicit `--telemetry`, not `--yes`).
pub fn apply(
    home: &Path,
    claude_home: &Path,
    cwd: &Path,
    epoch: u64,
    process_configured: bool,
    replace_off_ok: bool,
) -> StepReport {
    let off = matches!(
        crate::config::resolve_valid("usage.telemetry", home, None),
        Ok((Value::String(v), _, _)) if v == "off"
    );
    if off {
        return StepReport::skipped("telemetry", "usage.telemetry is off, so nothing is written");
    }
    let settings_path = claude_home.join("settings.json");
    let paths = crate::usage::run::Paths::from_home(home);
    let written_before = load(&marker_path(home)).unwrap_or_default();
    let ours = !written_before.is_empty() && already_written(home, &settings_path);
    if ours {
        let current = match crate::usage::collector::ensure_state(&paths) {
            Ok(s) => s,
            Err(e) => return StepReport::failed("telemetry", e),
        };
        if holds_all(&settings_path, &crate::cc::telemetry::env_for(&current)) {
            return StepReport::already_correct(
                "telemetry",
                "the variables `playbook setup` wrote are still in place",
            );
        }
    }
    // A yes to "turn it on" may replace an off switch that lives alone in the
    // user's own settings.json. Any other existing setup is left as it is.
    let switch_off_here = can_replace_off_switch(home, claude_home, cwd, process_configured);
    if switch_off_here && !replace_off_ok && !ours {
        return StepReport::skipped(
            "telemetry",
            "CLAUDE_CODE_ENABLE_TELEMETRY is 0 in your settings and --yes does not change a value you set; to turn it on, run: playbook setup --only telemetry",
        );
    }
    if !ours
        && !switch_off_here
        && (process_configured || crate::cc::telemetry::files_configured(home, cwd))
    {
        return StepReport::skipped(
            "telemetry",
            "you already have telemetry configured, left as it is (`playbook usage status` shows how it is read)",
        );
    }
    let state = match crate::usage::collector::ensure_state(&paths) {
        Ok(s) => s,
        Err(e) => return StepReport::failed("telemetry", e),
    };
    let mut root = match load(&settings_path) {
        Ok(r) => r,
        Err(e) => return StepReport::failed("telemetry", e),
    };
    let vars = crate::cc::telemetry::env_for(&state);
    // The settings merge writes its keys sorted. A file that is sorted stays
    // sorted, so the next merge finds nothing to reorder.
    let was_sorted = root.keys().is_sorted();
    let env = root
        .entry("env")
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(env) = env.as_object_mut() else {
        return StepReport::skipped(
            "telemetry",
            "the env entry in settings.json is not an object, left as it is",
        );
    };
    let mut written = Map::new();
    // A refresh keeps what the first run replaced, so uninstall still restores it.
    let mut replaced = written_before
        .get(REPLACED)
        .and_then(Value::as_object)
        .filter(|_| ours)
        .cloned()
        .unwrap_or_default();
    for (k, v) in vars {
        let new = Value::String(v);
        if ours && env.get(&k) == written_before.get(&k) {
            env.insert(k.clone(), new.clone());
        } else if let Some(old) = env.insert(k.clone(), new.clone()) {
            replaced.insert(k.clone(), old);
        }
        written.insert(k, new);
    }
    if !replaced.is_empty() {
        written.insert(REPLACED.to_string(), Value::Object(replaced));
    }
    if let Err(e) = write_keeping_order(&settings_path, root, was_sorted, epoch) {
        return StepReport::failed("telemetry", e);
    }
    let marker = marker_path(home);
    if let Err(e) = crate::common::atomic::write_atomic_private(
        &marker,
        format!("{:#}\n", Value::Object(written)),
    ) {
        return StepReport::failed(
            "telemetry",
            format!("could not write {}: {e}", marker.display()),
        );
    }
    StepReport::wired(
        "telemetry",
        format!(
            "wrote the OpenTelemetry variables for the local collector (127.0.0.1:{}); no prompt text or tool content is sent",
            state.port
        ),
    )
}

/// Whether settings.json's env holds every `(key, value)` in `wanted`.
fn holds_all(settings_path: &Path, wanted: &[(String, String)]) -> bool {
    let Ok(root) = load(settings_path) else {
        return false;
    };
    let env = root.get("env").and_then(Value::as_object);
    wanted
        .iter()
        .all(|(k, v)| env.is_some_and(|e| e.get(k).and_then(Value::as_str) == Some(v)))
}

/// What `remove` did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Removed {
    pub keys: usize,
    pub backup: Option<PathBuf>,
}

/// Removes the keys `apply` wrote, those still holding the value written.
/// `dry_run` only counts.
pub fn remove(
    home: &Path,
    settings_path: &Path,
    epoch: u64,
    dry_run: bool,
) -> Result<Removed, String> {
    let marker = marker_path(home);
    let written = load(&marker)?;
    if written.is_empty() {
        return Ok(Removed::default());
    }
    let mut root = load(settings_path)?;
    let mut keys = 0;
    let mut emptied = false;
    if let Some(env) = root.get_mut("env").and_then(Value::as_object_mut) {
        let replaced = written
            .get(REPLACED)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for (k, v) in written.iter().filter(|(k, _)| *k != REPLACED) {
            if env.get(k) == Some(v) {
                env.remove(k);
                keys += 1;
                if let Some(old) = replaced.get(k) {
                    env.insert(k.clone(), old.clone());
                }
            }
        }
        emptied = keys > 0 && env.is_empty();
    }
    if emptied {
        root.remove("env");
    }
    let mut out = Removed { keys, backup: None };
    if dry_run {
        return Ok(out);
    }
    if keys > 0 {
        write_with_backup(settings_path, root, epoch)?;
        let name = settings_path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        out.backup = Some(settings_path.with_file_name(format!("{name}.bak.{epoch}")));
    }
    let _ = fs::remove_file(&marker);
    Ok(out)
}

/// The one-sentence purpose of each key playbook owns, for the change summary.
const PURPOSES: &[(&str, &str)] = &[
    (
        "CLAUDE_CODE_ENABLE_TELEMETRY",
        "lets Claude Code export OpenTelemetry data",
    ),
    (
        "OTEL_LOGS_EXPORTER",
        "sends Claude Code's event log over OTLP",
    ),
    (
        "OTEL_METRICS_EXPORTER",
        "sends Claude Code's counters over OTLP",
    ),
    (
        "OTEL_TRACES_EXPORTER",
        "keeps traces off, playbook does not use them",
    ),
    (
        "OTEL_EXPORTER_OTLP_PROTOCOL",
        "sends JSON over HTTP, which the collector reads",
    ),
    (
        "OTEL_EXPORTER_OTLP_ENDPOINT",
        "points the export at the collector on this machine",
    ),
    (
        "OTEL_EXPORTER_OTLP_HEADERS",
        "logs in to the local collector with its token",
    ),
    (
        "OTEL_METRICS_INCLUDE_ACCOUNT_UUID",
        "keeps your account id out of the counters",
    ),
];

/// Why OTEL_LOG_TOOL_DETAILS is not among the keys.
const TOOL_DETAILS_LINE: &str = "OTEL_LOG_TOOL_DETAILS stays off: it would send Bash commands, file paths and tool inputs to the collector";

/// The owned keys of an env block, in the order playbook writes them.
fn owned_env(env: &Map<String, Value>) -> Vec<(String, String)> {
    PURPOSES
        .iter()
        .filter_map(|(k, _)| {
            let v = env.get(*k)?;
            let text = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            Some(((*k).to_string(), text))
        })
        .collect()
}

fn env_block(settings_path: &Path) -> Result<Map<String, Value>, String> {
    let root = load(settings_path)?;
    Ok(root
        .get("env")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default())
}

/// A value for display. The token never prints.
fn shown(key: &str, value: &str) -> String {
    if key == "OTEL_EXPORTER_OTLP_HEADERS" {
        "Authorization=Bearer <token>".to_string()
    } else {
        value.to_string()
    }
}

/// Makes Claude Code's settings follow the `usage.telemetry` setting and
/// returns the lines that say what changed, where and why. `on` is whether the
/// setting is now `auto`. Only the keys playbook owns are written or removed.
pub fn sync_setting(
    home: &Path,
    claude_home: &Path,
    cwd: &Path,
    epoch: u64,
    on: bool,
) -> Result<Vec<String>, String> {
    sync_with(
        home,
        claude_home,
        cwd,
        epoch,
        on,
        process_configured_beyond_settings(claude_home),
    )
}

fn sync_with(
    home: &Path,
    claude_home: &Path,
    cwd: &Path,
    epoch: u64,
    on: bool,
    process_configured: bool,
) -> Result<Vec<String>, String> {
    let path = claude_home.join("settings.json");
    let unreadable = |e: String| {
        format!(
            "{e}; the Claude Code settings were not changed. Fix the file, then run: playbook config set usage.telemetry {}",
            if on { "auto" } else { "off" }
        )
    };
    let before = owned_env(&env_block(&path).map_err(unreadable)?);
    if on {
        let step = apply(home, claude_home, cwd, epoch, process_configured, true);
        match step.status {
            crate::init::run::StepStatus::Failed => return Err(step.detail),
            crate::init::run::StepStatus::Skipped => {
                return Ok(vec![
                    format!("telemetry: not written: {}", step.detail),
                    "telemetry: if a Claude Code session started this shell with the keys playbook wrote earlier, run the command again from a new terminal".to_string(),
                ]);
            }
            _ => {}
        }
    } else {
        remove(home, &path, epoch, false)?;
    }
    let after = owned_env(&env_block(&path)?);
    let find = |set: &[(String, String)], k: &str| {
        set.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone())
    };
    let mut diff = Vec::new();
    for (key, purpose) in PURPOSES {
        match (find(&before, key), find(&after, key)) {
            (None, Some(new)) => {
                diff.push(format!("  + {key}={}  ({purpose})", shown(key, &new)));
            }
            (Some(old), Some(new)) if old != new => diff.push(format!(
                "  ~ {key}: {} -> {}  ({purpose})",
                shown(key, &old),
                shown(key, &new)
            )),
            (Some(old), None) => {
                diff.push(format!("  - {key}={}  ({purpose})", shown(key, &old)));
            }
            _ => {}
        }
    }
    let mut lines = Vec::new();
    let left: Vec<&str> = after.iter().map(|(k, _)| k.as_str()).collect();
    let backup = path.with_file_name(format!("settings.json.bak.{epoch}"));
    if diff.is_empty() {
        lines.push(format!(
            "telemetry: no change, {} already {}",
            path.display(),
            if on {
                "holds every key playbook owns"
            } else if left.is_empty() {
                "has none of the keys playbook owns"
            } else {
                "was not changed"
            }
        ));
    } else {
        lines.push(format!(
            "telemetry: {} {}",
            if on { "changed" } else { "removed keys from" },
            path.display()
        ));
        lines.extend(diff);
        if backup.is_file() {
            lines.push(format!("  the old file is saved as {}", backup.display()));
        }
        if !on {
            lines.push("  every other key was kept".to_string());
        }
    }
    if !on && !left.is_empty() {
        lines.push(format!(
            "telemetry: left in place because playbook did not write them or you changed them: {}",
            left.join(", ")
        ));
    }
    if on {
        lines.push(format!("telemetry: {TOOL_DETAILS_LINE}"));
        lines.push("telemetry: to undo, run: playbook config set usage.telemetry off".to_string());
    } else {
        lines.push(
            "telemetry: to turn it on again, run: playbook config set usage.telemetry auto"
                .to_string(),
        );
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;
    use serde_json::json;

    fn setup(name: &str, settings: &str) -> (PathBuf, PathBuf, PathBuf) {
        crate::usage_host::install();
        let home = scratch_dir(name);
        let claude = home.join(".claude");
        fs::create_dir_all(&claude).unwrap();
        fs::write(claude.join("settings.json"), settings).unwrap();
        let cwd = home.join("project");
        fs::create_dir_all(&cwd).unwrap();
        (home, claude, cwd)
    }

    fn env_of(claude: &Path) -> Map<String, Value> {
        let doc: Value =
            serde_json::from_str(&fs::read_to_string(claude.join("settings.json")).unwrap())
                .unwrap();
        doc.get("env")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    }

    #[test]
    fn with_no_telemetry_configured_the_env_keys_are_written_and_recorded() {
        let (home, claude, cwd) = setup("init-tel-write", r#"{"model":"opus","env":{"FOO":"1"}}"#);
        let step = apply(&home, &claude, &cwd, 100, false, true);
        let env = env_of(&claude);
        assert_eq!(step.status, crate::init::run::StepStatus::Wired);
        assert_eq!(env["CLAUDE_CODE_ENABLE_TELEMETRY"], "1");
        assert_eq!(env["OTEL_METRICS_INCLUDE_ACCOUNT_UUID"], "false");
        assert_eq!(env["FOO"], "1");
        assert!(marker_path(&home).is_file());
        assert!(claude.join("settings.json.bak.100").is_file());
    }

    #[test]
    fn state_reads_each_case_from_the_settings_files() {
        let cases = [
            ("{}", State::Unset),
            (
                r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"0"}}"#,
                State::Disabled,
            ),
            (
                r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"1"}}"#,
                State::Enabled,
            ),
            (
                r#"{"env":{"OTEL_EXPORTER_OTLP_ENDPOINT":"https://x"}}"#,
                State::SetUpByYou,
            ),
        ];
        for (i, (settings, expected)) in cases.iter().enumerate() {
            let (home, _claude, cwd) = setup(&format!("init-tel-state-{i}"), settings);

            let got = state(&home, &cwd, false);

            assert_eq!(got, *expected, "{settings}");
        }
    }

    #[test]
    fn every_spelling_of_a_lone_off_switch_is_replaced_by_a_yes() {
        let spellings = [r#""""#, r#""false""#, r#"" 0 ""#, "0", "false"];
        for (i, spelling) in spellings.iter().enumerate() {
            let settings = format!(r#"{{"env":{{"CLAUDE_CODE_ENABLE_TELEMETRY":{spelling}}}}}"#);
            let (home, claude, cwd) = setup(&format!("init-tel-spelling-{i}"), &settings);

            let step = apply(&home, &claude, &cwd, 5, false, true);

            assert_eq!(
                step.status,
                crate::init::run::StepStatus::Wired,
                "{spelling}"
            );
            assert_eq!(env_of(&claude)["CLAUDE_CODE_ENABLE_TELEMETRY"], "1");
        }
    }

    #[test]
    fn a_lone_off_switch_is_kept_when_another_file_names_a_telemetry_variable() {
        let original = r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"0"}}"#;
        let (home, claude, cwd) = setup("init-tel-other-file", original);
        fs::create_dir_all(cwd.join(".claude")).unwrap();
        fs::write(
            cwd.join(".claude/settings.json"),
            r#"{"env":{"OTEL_EXPORTER_OTLP_ENDPOINT":"https://team"}}"#,
        )
        .unwrap();

        let step = apply(&home, &claude, &cwd, 5, false, true);

        assert_eq!(step.status, crate::init::run::StepStatus::Skipped);
        assert_eq!(
            fs::read_to_string(claude.join("settings.json")).unwrap(),
            original
        );
    }

    #[test]
    fn an_off_switch_in_project_settings_is_not_replaced() {
        let (home, claude, cwd) = setup("init-tel-project-off", "{}");
        fs::create_dir_all(cwd.join(".claude")).unwrap();
        fs::write(
            cwd.join(".claude/settings.json"),
            r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"0"}}"#,
        )
        .unwrap();

        let step = apply(&home, &claude, &cwd, 5, false, true);

        assert_eq!(step.status, crate::init::run::StepStatus::Skipped);
        assert!(env_of(&claude).is_empty());
    }

    #[test]
    fn an_off_switch_in_the_process_environment_is_never_replaced() {
        let (home, claude, cwd) = setup(
            "init-tel-process-off",
            r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"0"}}"#,
        );

        let step = apply(&home, &claude, &cwd, 5, true, true);

        assert_eq!(step.status, crate::init::run::StepStatus::Skipped);
        assert_eq!(env_of(&claude)["CLAUDE_CODE_ENABLE_TELEMETRY"], "0");
    }

    #[test]
    fn uninstall_puts_back_the_off_switch_a_yes_replaced() {
        let (home, claude, cwd) = setup(
            "init-tel-restore",
            r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"0","FOO":"1"}}"#,
        );
        apply(&home, &claude, &cwd, 5, false, true);

        let removed = remove(&home, &claude.join("settings.json"), 6, false).unwrap();

        let env = env_of(&claude);
        assert!(removed.keys > 0);
        assert_eq!(env["CLAUDE_CODE_ENABLE_TELEMETRY"], "0");
        assert_eq!(env["FOO"], "1");
        assert!(!env.contains_key("OTEL_EXPORTER_OTLP_ENDPOINT"));
    }

    #[test]
    fn a_second_init_after_a_replaced_switch_reports_no_change() {
        let (home, claude, cwd) = setup(
            "init-tel-replaced-twice",
            r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"0"}}"#,
        );
        apply(&home, &claude, &cwd, 5, false, true);

        let again = apply(&home, &claude, &cwd, 6, false, true);

        assert_eq!(again.status, crate::init::run::StepStatus::AlreadyCorrect);
    }

    #[test]
    fn the_question_is_skipped_for_each_state_init_cannot_act_on() {
        let on = r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"1"}}"#;
        let own = r#"{"env":{"OTEL_EXPORTER_OTLP_ENDPOINT":"https://x"}}"#;
        let beside = r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"0","OTEL_LOGS_EXPORTER":"otlp"}}"#;
        let lone = r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"0"}}"#;
        let cases = [
            (on, Some(ENABLED_LINE)),
            (own, Some(SET_UP_LINE)),
            (beside, Some(OFF_ELSEWHERE_LINE)),
            (lone, None),
            ("{}", None),
        ];
        for (i, (settings, expected)) in cases.iter().enumerate() {
            let (home, claude, cwd) = setup(&format!("init-tel-skip-{i}"), settings);

            let got = skip_line_files_only(&home, &claude, &cwd);

            assert_eq!(got, *expected, "{settings}");
        }
    }

    /// `skip_line` without the process environment, so the test machine
    /// cannot change the answer.
    fn skip_line_files_only(home: &Path, claude: &Path, cwd: &Path) -> Option<&'static str> {
        match state(home, cwd, false) {
            State::Enabled => Some(ENABLED_LINE),
            State::SetUpByYou => Some(SET_UP_LINE),
            State::Unset => None,
            State::Disabled => {
                (!can_replace_off_switch(home, claude, cwd, false)).then_some(OFF_ELSEWHERE_LINE)
            }
        }
    }

    #[test]
    fn a_yes_replaces_an_off_switch_that_stands_alone_in_user_settings() {
        let (home, claude, cwd) = setup(
            "init-tel-flip",
            r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"0","FOO":"1"}}"#,
        );
        let step = apply(&home, &claude, &cwd, 5, false, true);
        let env = env_of(&claude);
        assert_eq!(step.status, crate::init::run::StepStatus::Wired);
        assert_eq!(env["CLAUDE_CODE_ENABLE_TELEMETRY"], "1");
        assert_eq!(env["FOO"], "1");
    }

    #[test]
    fn a_second_run_reports_no_change_instead_of_calling_it_your_setup() {
        let (home, claude, cwd) = setup("init-tel-twice", "{}");
        apply(&home, &claude, &cwd, 1, false, true);
        let again = apply(&home, &claude, &cwd, 2, false, true);
        assert_eq!(again.status, crate::init::run::StepStatus::AlreadyCorrect);
    }

    #[test]
    fn the_config_key_off_writes_nothing() {
        let (home, claude, cwd) = setup("init-tel-cfgoff", "{}");
        let cfg = home.join(".config/playbook");
        fs::create_dir_all(&cfg).unwrap();
        fs::write(cfg.join("config.json"), r#"{"usage":{"telemetry":"off"}}"#).unwrap();
        let step = apply(&home, &claude, &cwd, 1, false, true);
        assert_eq!(step.status, crate::init::run::StepStatus::Skipped);
        assert!(env_of(&claude).is_empty());
    }

    #[test]
    fn content_logging_variables_are_never_written() {
        let (home, claude, cwd) = setup("init-tel-privacy", "{}");
        apply(&home, &claude, &cwd, 1, false, true);
        let env = env_of(&claude);
        for name in [
            "OTEL_LOG_USER_PROMPTS",
            "OTEL_LOG_TOOL_DETAILS",
            "OTEL_LOG_TOOL_CONTENT",
            "OTEL_LOG_RAW_API_BODIES",
        ] {
            assert!(!env.contains_key(name), "{name}");
        }
    }

    #[test]
    fn an_existing_telemetry_setting_is_never_overridden() {
        let original = r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"0","OTEL_EXPORTER_OTLP_ENDPOINT":"https://x"}}"#;
        let (home, claude, cwd) = setup("init-tel-keep", original);
        let step = apply(&home, &claude, &cwd, 1, false, true);
        assert_eq!(step.status, crate::init::run::StepStatus::Skipped);
        assert_eq!(
            fs::read_to_string(claude.join("settings.json")).unwrap(),
            original
        );
        assert!(!marker_path(&home).exists());
    }

    #[test]
    fn a_telemetry_variable_in_the_environment_counts_as_configured() {
        let (home, claude, cwd) = setup("init-tel-envvar", "{}");
        let step = apply(&home, &claude, &cwd, 1, true, true);
        assert_eq!(step.status, crate::init::run::StepStatus::Skipped);
        assert!(env_of(&claude).is_empty());
    }

    #[test]
    fn a_project_setting_counts_as_configured() {
        let (home, claude, cwd) = setup("init-tel-project", "{}");
        fs::create_dir_all(cwd.join(".claude")).unwrap();
        fs::write(
            cwd.join(".claude/settings.json"),
            r#"{"env":{"OTEL_LOGS_EXPORTER":"console"}}"#,
        )
        .unwrap();
        let step = apply(&home, &claude, &cwd, 1, false, true);
        assert_eq!(step.status, crate::init::run::StepStatus::Skipped);
        assert!(env_of(&claude).is_empty());
    }

    #[test]
    fn remove_takes_out_exactly_what_was_written_and_keeps_the_rest() {
        let (home, claude, cwd) = setup("init-tel-remove", r#"{"env":{"FOO":"1"}}"#);
        apply(&home, &claude, &cwd, 1, false, true);
        let out = remove(&home, &claude.join("settings.json"), 2, false).unwrap();
        assert_eq!(out.keys, 8);
        assert_eq!(
            env_of(&claude),
            json!({"FOO":"1"}).as_object().unwrap().clone()
        );
        assert!(!marker_path(&home).exists());
    }

    #[test]
    fn remove_keeps_a_key_the_user_changed_since() {
        let (home, claude, cwd) = setup("init-tel-changed", "{}");
        apply(&home, &claude, &cwd, 1, false, true);
        let path = claude.join("settings.json");
        let mut doc: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        doc["env"]["OTEL_LOGS_EXPORTER"] = json!("console");
        fs::write(&path, doc.to_string()).unwrap();
        let out = remove(&home, &path, 2, false).unwrap();
        assert_eq!(out.keys, 7);
        assert_eq!(env_of(&claude)["OTEL_LOGS_EXPORTER"], "console");
    }

    #[test]
    fn remove_without_a_record_changes_nothing() {
        let original = r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"1"}}"#;
        let (home, claude, _cwd) = setup("init-tel-norecord", original);
        let out = remove(&home, &claude.join("settings.json"), 2, false).unwrap();
        assert_eq!(out, Removed::default());
        assert_eq!(
            fs::read_to_string(claude.join("settings.json")).unwrap(),
            original
        );
    }

    #[test]
    fn a_dry_run_remove_counts_and_writes_nothing() {
        let (home, claude, cwd) = setup("init-tel-dry", "{}");
        apply(&home, &claude, &cwd, 1, false, true);
        let before = fs::read_to_string(claude.join("settings.json")).unwrap();
        let out = remove(&home, &claude.join("settings.json"), 2, true).unwrap();
        assert_eq!(out.keys, 8);
        assert_eq!(
            fs::read_to_string(claude.join("settings.json")).unwrap(),
            before
        );
        assert!(marker_path(&home).exists());
    }

    #[test]
    fn the_plan_lists_the_changed_switch_and_the_two_that_stay() {
        let (_home, claude, _cwd) = setup(
            "init-tel-plan",
            r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"0","DISABLE_TELEMETRY":"1","DO_NOT_TRACK":"1"}}"#,
        );

        let lines = plan_lines(&claude).join("\n");

        assert!(
            lines.contains("CLAUDE_CODE_ENABLE_TELEMETRY 0 -> 1"),
            "{lines}"
        );
        assert!(
            lines.contains("Stays as it is: DISABLE_TELEMETRY=1"),
            "{lines}"
        );
        assert!(lines.contains("Stays as it is: DO_NOT_TRACK=1"), "{lines}");
        assert!(lines.contains("OTEL_EXPORTER_OTLP_ENDPOINT"), "{lines}");
    }

    #[test]
    fn the_plan_for_a_clean_file_adds_the_switch() {
        let (_home, claude, _cwd) = setup("init-tel-plan-clean", "{}");

        let lines = plan_lines(&claude).join("\n");

        assert!(
            lines.contains("Adds: CLAUDE_CODE_ENABLE_TELEMETRY=1"),
            "{lines}"
        );
        assert!(!lines.contains("Stays as it is"), "{lines}");
    }

    #[test]
    fn the_three_off_values_keep_two_and_flip_one_and_uninstall_restores_all() {
        let original = r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"0","DISABLE_TELEMETRY":"1","DO_NOT_TRACK":"1"}}"#;
        let (home, claude, cwd) = setup("init-tel-trio", original);

        let step = apply(&home, &claude, &cwd, 3, false, true);
        let during = env_of(&claude);
        remove(&home, &claude.join("settings.json"), 4, false).unwrap();
        let after = env_of(&claude);

        assert_eq!(step.status, crate::init::run::StepStatus::Wired);
        assert_eq!(during["CLAUDE_CODE_ENABLE_TELEMETRY"], "1");
        assert_eq!(during["DISABLE_TELEMETRY"], "1");
        assert_eq!(during["DO_NOT_TRACK"], "1");
        assert_eq!(after.len(), 3);
        assert_eq!(after["CLAUDE_CODE_ENABLE_TELEMETRY"], "0");
    }

    #[test]
    fn without_consent_to_replace_a_lone_off_switch_nothing_is_written() {
        let original = r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"0"}}"#;
        let (home, claude, cwd) = setup("init-tel-no-replace", original);

        let step = apply(&home, &claude, &cwd, 3, false, false);

        assert_eq!(step.status, crate::init::run::StepStatus::Skipped);
        assert!(step.detail.contains("playbook setup --only telemetry"));
        assert_eq!(
            fs::read_to_string(claude.join("settings.json")).unwrap(),
            original
        );
    }

    #[test]
    fn a_changed_collector_port_is_rewritten_and_uninstall_still_restores_the_original() {
        let (home, claude, cwd) = setup(
            "init-tel-port-move",
            r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"0"}}"#,
        );
        apply(&home, &claude, &cwd, 1, false, true);
        let paths = crate::usage::run::Paths::from_home(&home);
        let state_file = crate::usage::collector::state_path(&paths);
        let mut state = crate::usage::collector::read_state(&state_file).unwrap();
        state.port = 51234;
        fs::write(&state_file, serde_json::to_vec(&state).unwrap()).unwrap();

        let again = apply(&home, &claude, &cwd, 2, false, false);
        let moved = env_of(&claude)["OTEL_EXPORTER_OTLP_ENDPOINT"].clone();
        remove(&home, &claude.join("settings.json"), 3, false).unwrap();

        assert_eq!(again.status, crate::init::run::StepStatus::Wired);
        assert_eq!(moved, "http://127.0.0.1:51234");
        assert_eq!(env_of(&claude)["CLAUDE_CODE_ENABLE_TELEMETRY"], "0");
        assert!(!env_of(&claude).contains_key("OTEL_EXPORTER_OTLP_ENDPOINT"));
    }

    #[test]
    fn a_machine_with_no_claude_folder_gets_the_file_created() {
        crate::usage_host::install();
        let home = crate::common::test_support::scratch_dir("init-tel-nodir");
        let claude = home.join(".claude");

        let step = apply(&home, &claude, &home, 1, false, true);

        assert_eq!(step.status, crate::init::run::StepStatus::Wired);
        assert_eq!(env_of(&claude)["CLAUDE_CODE_ENABLE_TELEMETRY"], "1");
    }

    #[test]
    fn a_stale_record_of_a_replaced_value_is_not_carried_into_a_fresh_write() {
        let (home, claude, cwd) = setup(
            "init-tel-stale-record",
            r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"0"}}"#,
        );
        apply(&home, &claude, &cwd, 1, false, true);
        let settings = claude.join("settings.json");
        fs::write(&settings, "{}").unwrap();

        apply(&home, &claude, &cwd, 2, false, true);
        remove(&home, &settings, 3, false).unwrap();

        assert!(!env_of(&claude).contains_key("CLAUDE_CODE_ENABLE_TELEMETRY"));
    }

    fn text(lines: &[String]) -> String {
        lines.join("\n")
    }

    #[test]
    fn turning_it_on_in_a_fresh_home_adds_every_key_and_explains_each() {
        let (home, claude, cwd) = setup("init-tel-sync-fresh", "{}");
        fs::remove_file(claude.join("settings.json")).unwrap();

        let out = text(&sync_with(&home, &claude, &cwd, 100, true, false).unwrap());

        let path = claude.join("settings.json").display().to_string();
        assert!(out.contains(&path), "{out}");
        assert!(
            out.contains("+ CLAUDE_CODE_ENABLE_TELEMETRY=1  (lets Claude Code export"),
            "{out}"
        );
        assert!(
            out.contains("+ OTEL_EXPORTER_OTLP_HEADERS=Authorization=Bearer <token>"),
            "{out}"
        );
        assert!(out.contains("OTEL_LOG_TOOL_DETAILS stays off"), "{out}");
        assert!(
            out.contains("playbook config set usage.telemetry off"),
            "{out}"
        );
        assert_eq!(env_of(&claude)["OTEL_METRICS_EXPORTER"], "otlp");
    }

    #[test]
    fn turning_it_on_keeps_the_other_env_keys_and_never_prints_the_token() {
        let (home, claude, cwd) = setup(
            "init-tel-sync-others",
            r#"{"model":"opus","env":{"FOO":"1","DISABLE_TELEMETRY":"1"}}"#,
        );

        let out = text(&sync_with(&home, &claude, &cwd, 100, true, false).unwrap());

        let env = env_of(&claude);
        assert_eq!(env["FOO"], "1");
        assert_eq!(env["DISABLE_TELEMETRY"], "1");
        assert!(!out.contains("FOO"), "{out}");
        assert!(out.contains("settings.json.bak.100"), "{out}");
        let header = env["OTEL_EXPORTER_OTLP_HEADERS"].as_str().unwrap();
        let token = header.replace("Authorization=Bearer ", "");
        assert!(!out.contains(&token), "the token must not print: {out}");
    }

    #[test]
    fn keys_already_in_place_report_no_change() {
        let (home, claude, cwd) = setup("init-tel-sync-same", "{}");
        sync_with(&home, &claude, &cwd, 100, true, false).unwrap();

        let out = text(&sync_with(&home, &claude, &cwd, 101, true, false).unwrap());

        assert!(out.contains("no change"), "{out}");
        assert!(!out.contains("  + "), "{out}");
        assert!(!claude.join("settings.json.bak.101").exists());
    }

    #[test]
    fn turning_it_off_removes_only_the_keys_playbook_owns() {
        let (home, claude, cwd) = setup("init-tel-sync-off", r#"{"env":{"FOO":"1"}}"#);
        sync_with(&home, &claude, &cwd, 100, true, false).unwrap();

        let out = text(&sync_with(&home, &claude, &cwd, 101, false, false).unwrap());

        let env = env_of(&claude);
        assert_eq!(env.len(), 1, "{env:?}");
        assert_eq!(env["FOO"], "1");
        assert!(out.contains("removed keys from"), "{out}");
        assert!(out.contains("  - OTEL_LOGS_EXPORTER=otlp"), "{out}");
        assert!(out.contains("every other key was kept"), "{out}");
        assert!(
            out.contains("playbook config set usage.telemetry auto"),
            "{out}"
        );
    }

    #[test]
    fn turning_it_off_with_nothing_written_says_so() {
        let (home, claude, cwd) = setup("init-tel-sync-off-none", r#"{"env":{"FOO":"1"}}"#);

        let out = text(&sync_with(&home, &claude, &cwd, 100, false, false).unwrap());

        assert!(out.contains("no change"), "{out}");
        assert_eq!(env_of(&claude)["FOO"], "1");
    }

    #[test]
    fn a_settings_file_that_is_not_json_stops_and_is_left_alone() {
        let (home, claude, cwd) = setup("init-tel-sync-bad", "{ not json");

        for on in [true, false] {
            let err = sync_with(&home, &claude, &cwd, 100, on, false).unwrap_err();
            assert!(err.contains("were not changed"), "{err}");
            assert!(err.contains("playbook config set usage.telemetry"), "{err}");
            assert!(err.contains("settings.json"), "{err}");
        }
        assert_eq!(
            fs::read_to_string(claude.join("settings.json")).unwrap(),
            "{ not json"
        );
    }

    #[test]
    fn turning_it_off_names_keys_it_did_not_write() {
        let (home, claude, cwd) = setup(
            "init-tel-sync-handmade",
            r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"1"}}"#,
        );

        let out = text(&sync_with(&home, &claude, &cwd, 100, false, false).unwrap());

        assert!(!out.contains("has none of the keys"), "{out}");
        assert!(out.contains("left in place"), "{out}");
        assert!(out.contains("CLAUDE_CODE_ENABLE_TELEMETRY"), "{out}");
        assert_eq!(env_of(&claude)["CLAUDE_CODE_ENABLE_TELEMETRY"], "1");
    }

    #[test]
    fn turning_it_on_where_telemetry_is_set_elsewhere_says_not_written() {
        let (home, claude, cwd) = setup("init-tel-sync-elsewhere", r#"{"env":{"FOO":"1"}}"#);

        let out = text(&sync_with(&home, &claude, &cwd, 100, true, true).unwrap());

        assert!(out.contains("not written"), "{out}");
        assert!(!env_of(&claude).contains_key("OTEL_LOGS_EXPORTER"));
    }

    #[test]
    fn a_replaced_value_comes_back_on_off_and_shows_as_a_change() {
        let (home, claude, cwd) = setup(
            "init-tel-sync-restore",
            r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"0"}}"#,
        );
        sync_with(&home, &claude, &cwd, 100, true, false).unwrap();

        let out = text(&sync_with(&home, &claude, &cwd, 101, false, false).unwrap());

        assert!(
            out.contains("~ CLAUDE_CODE_ENABLE_TELEMETRY: 1 -> 0"),
            "{out}"
        );
        assert_eq!(env_of(&claude)["CLAUDE_CODE_ENABLE_TELEMETRY"], "0");
    }
}
