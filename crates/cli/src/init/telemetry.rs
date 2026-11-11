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
use crate::usage::telemetry_detect::{is_on, ENABLE_VAR};
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

/// Whether the record names at least one key and settings.json still holds
/// every value in it. A record that holds only its meta entries names no key,
/// so it counts as no record.
fn already_written(home: &Path, settings_path: &Path) -> bool {
    let Ok(written) = load(&marker_path(home)) else {
        return false;
    };
    if recorded_keys(&written).next().is_none() {
        return false;
    }
    let Ok(root) = load(settings_path) else {
        return false;
    };
    let env = root.get("env").and_then(Value::as_object);
    let all_held = recorded_keys(&written).all(|(k, v)| env.is_some_and(|e| e.get(k) == Some(v)));
    all_held
}

/// The marker key that holds the values init replaced, by variable name.
const REPLACED: &str = "_replaced";

/// The entries of a record that name a written key, not its meta entries.
fn recorded_keys(record: &Map<String, Value>) -> impl Iterator<Item = (&String, &Value)> {
    record.iter().filter(|(k, _)| *k != REPLACED)
}

/// Whether at least one recorded key still holds its recorded value.
fn record_is_live(user_env: &Map<String, Value>, record: &Map<String, Value>) -> bool {
    recorded_keys(record).any(|(k, v)| user_env.get(k) == Some(v))
}

/// What `apply` does with keys that are missing or stale beside telemetry
/// settings it did not write.
#[derive(Debug, PartialEq, Eq)]
enum Repair {
    /// Every wanted key already holds a wanted value.
    NotNeeded,
    /// Write these keys, in `env_for` order.
    Keys(Vec<String>),
    /// Someone else's setup: leave it alone.
    Foreign,
}

/// The two local endpoint forms setup writes, up to the port.
const LOCAL_ENDPOINT_PREFIXES: [&str; 2] = ["http://127.0.0.1:", "http://localhost:"];

/// The start of the headers value setup writes, up to the token.
const BEARER_PREFIX: &str = "Authorization=Bearer ";

/// The lowest and highest valid port.
const PORT_MIN: u32 = 1;
const PORT_MAX: u32 = 65535;

const ENDPOINT_KEY: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";
const HEADERS_KEY: &str = "OTEL_EXPORTER_OTLP_HEADERS";

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Whether `endpoint` is a local collector address: `http://127.0.0.1:<port>`
/// or `http://localhost:<port>`, a port from 1 to 65535, and nothing after it
/// but an optional `/`.
fn is_local_endpoint(endpoint: &str) -> bool {
    LOCAL_ENDPOINT_PREFIXES.iter().any(|prefix| {
        let Some(rest) = endpoint.strip_prefix(prefix) else {
            return false;
        };
        let port = rest.strip_suffix('/').unwrap_or(rest);
        !port.is_empty()
            && port.bytes().all(|b| b.is_ascii_digit())
            && port
                .parse::<u32>()
                .is_ok_and(|n| (PORT_MIN..=PORT_MAX).contains(&n))
    })
}

/// Whether `headers` is `Authorization=Bearer <token>` with a token.
fn is_bearer_header(headers: &str) -> bool {
    headers
        .strip_prefix(BEARER_PREFIX)
        .is_some_and(|token| !token.trim().is_empty())
}

/// Decides whether the playbook keys in the user's settings.json env can be
/// repaired. `wanted` is `env_for` for the collector state. Pure.
fn repair_needed(
    user_env: &Map<String, Value>,
    record: &Map<String, Value>,
    wanted: &[(String, String)],
    elsewhere_configured: bool,
) -> Repair {
    let current = |k: &str| user_env.get(k).map(value_text);
    let switch = current(ENABLE_VAR);
    if elsewhere_configured || (switch.is_some() && !is_on(switch.as_deref())) {
        return Repair::Foreign;
    }
    let differs = |k: &str, want: &str| match current(k) {
        None => true,
        Some(cur) => cur != want && !(k == ENABLE_VAR && is_on(Some(&cur))),
    };
    let keys_that_differ = |only: &dyn Fn(&str) -> bool| {
        let keys: Vec<String> = wanted
            .iter()
            .filter(|(k, want)| only(k) && differs(k, want))
            .map(|(k, _)| k.clone())
            .collect();
        if keys.is_empty() {
            Repair::NotNeeded
        } else {
            Repair::Keys(keys)
        }
    };
    if record_is_live(user_env, record) {
        let user_changed_one = wanted.iter().any(|(k, want)| {
            current(k).is_some_and(|cur| {
                cur != *want
                    && record.get(k).map(value_text).as_deref() != Some(cur.as_str())
                    && !(k == ENABLE_VAR && is_on(Some(&cur)))
            })
        });
        if user_changed_one {
            return Repair::Foreign;
        }
        return keys_that_differ(&|_| true);
    }
    // No record, or a dead one: only two shapes count as playbook's.
    let telemetry_keys: Vec<&String> = user_env
        .keys()
        .filter(|k| crate::usage::telemetry_detect::is_telemetry_var(k))
        .collect();
    if telemetry_keys.len() == 1 && switch.is_some() {
        return keys_that_differ(&|k| k != ENABLE_VAR);
    }
    let full_set = wanted.iter().all(|(k, _)| user_env.contains_key(k));
    let fixed_hold = wanted
        .iter()
        .filter(|(k, _)| k != ENABLE_VAR && k != ENDPOINT_KEY && k != HEADERS_KEY)
        .all(|(k, want)| current(k).as_deref() == Some(want.as_str()));
    let shape_ok = full_set
        && fixed_hold
        && current(ENDPOINT_KEY).is_some_and(|e| is_local_endpoint(&e))
        && current(HEADERS_KEY).is_some_and(|h| is_bearer_header(&h));
    if shape_ok {
        keys_that_differ(&|k| k == ENDPOINT_KEY || k == HEADERS_KEY)
    } else {
        Repair::Foreign
    }
}

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
    let found = state(home, cwd, true);
    let can_replace = found == State::Disabled
        && can_replace_off_switch(home, claude_home, cwd, process_configured);
    skip_line_from(
        found,
        needs_action(home, claude_home, cwd, process_configured),
        can_replace,
    )
}

/// The skip table. `repair` is whether a yes has something to put right in
/// the user's settings.json: playbook keys that are missing or stale. It turns the "nothing to ask" and "set up by you" lines
/// into a question. `can_replace` is whether a yes may replace an off switch.
fn skip_line_from(state: State, repair: bool, can_replace: bool) -> Option<&'static str> {
    match state {
        State::Enabled => (!repair).then_some(ENABLED_LINE),
        State::SetUpByYou => (!repair).then_some(SET_UP_LINE),
        State::Unset => None,
        State::Disabled => (!can_replace).then_some(OFF_ELSEWHERE_LINE),
    }
}

/// Whether setup would repair keys now. It
/// reads the user file, the record and the collector state, and never
/// creates any of them: with no collector state it compares against the
/// placeholder `plan_lines` uses.
fn needs_action(home: &Path, claude_home: &Path, cwd: &Path, process_configured: bool) -> bool {
    let settings_path = claude_home.join("settings.json");
    let Ok(root) = load(&settings_path) else {
        return false;
    };
    let user_env = root
        .get("env")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let record = load(&marker_path(home)).unwrap_or_default();
    let paths = crate::usage::run::Paths::from_home(home);
    let collector =
        crate::usage::collector::read_state(&crate::usage::collector::state_path(&paths))
            .unwrap_or_default();
    let elsewhere = process_configured
        || crate::usage::telemetry_detect::files_configured_except(home, cwd, &settings_path);
    matches!(
        repair_needed(
            &user_env,
            &record,
            &crate::cc::telemetry::env_for(&collector),
            elsewhere
        ),
        Repair::Keys(_)
    )
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
    let ours = already_written(home, &settings_path);
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
    let mut repair_keys: Option<Vec<String>> = None;
    let mut ensured = None;
    if !ours
        && !switch_off_here
        && (process_configured || crate::cc::telemetry::files_configured(home, cwd))
    {
        let skip = || {
            StepReport::skipped(
                "telemetry",
                "you already have telemetry configured, left as it is (`playbook usage status` shows how it is read)",
            )
        };
        // An unparsable settings file counts as configured: leave it alone.
        let Ok(user_root) = load(&settings_path) else {
            return skip();
        };
        let elsewhere = process_configured
            || crate::usage::telemetry_detect::files_configured_except(home, cwd, &settings_path);
        if elsewhere {
            return skip();
        }
        let current = match crate::usage::collector::ensure_state(&paths) {
            Ok(s) => s,
            Err(e) => return StepReport::failed("telemetry", e),
        };
        let user_env = user_root
            .get("env")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let wanted = crate::cc::telemetry::env_for(&current);
        match repair_needed(&user_env, &written_before, &wanted, false) {
            Repair::NotNeeded => {
                return StepReport::already_correct("telemetry", ALREADY_POINTS_LINE);
            }
            Repair::Foreign => return skip(),
            Repair::Keys(keys) => repair_keys = Some(keys),
        }
        ensured = Some(current);
    }
    let state = match ensured {
        Some(s) => s,
        None => match crate::usage::collector::ensure_state(&paths) {
            Ok(s) => s,
            Err(e) => return StepReport::failed("telemetry", e),
        },
    };
    let mut root = match load(&settings_path) {
        Ok(r) => r,
        Err(e) => return StepReport::failed("telemetry", e),
    };
    let mut vars = crate::cc::telemetry::env_for(&state);
    if let Some(keys) = &repair_keys {
        vars.retain(|(k, _)| keys.contains(k));
    }
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
    // A live record is kept whole: unrepaired keys stay in it, so uninstall
    // still removes them, and so does what the first run replaced. A dead or
    // absent record starts over with only this run's keys.
    let live = record_is_live(env, &written_before);
    let mut written = if live {
        written_before.clone()
    } else {
        Map::new()
    };
    let mut replaced = written_before
        .get(REPLACED)
        .and_then(Value::as_object)
        .filter(|_| live)
        .cloned()
        .unwrap_or_default();
    let mut repaired = Vec::new();
    for (k, v) in vars {
        // A user's own spelling of an on switch stays, and stays unrecorded.
        if k == ENABLE_VAR && is_on(env.get(&k).map(value_text).as_deref()) {
            continue;
        }
        let new = Value::String(v);
        if live && env.get(&k) == written_before.get(&k) {
            env.insert(k.clone(), new.clone());
        } else if let Some(old) = env.insert(k.clone(), new.clone()) {
            replaced.insert(k.clone(), old);
        }
        repaired.push(k.clone());
        written.insert(k, new);
    }
    if replaced.is_empty() {
        written.remove(REPLACED);
    } else {
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
    if repair_keys.is_some() {
        return StepReport::wired(
            "telemetry",
            format!(
                "put the OpenTelemetry variables for the local collector (127.0.0.1:{}) right: {}",
                state.port,
                repaired.join(", ")
            ),
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

/// The detail when every key already holds a wanted value and no record says
/// playbook wrote them.
const ALREADY_POINTS_LINE: &str = "the OpenTelemetry keys already point at the local collector";

/// Whether settings.json's env holds every `(key, value)` in `wanted`.
fn holds_all(settings_path: &Path, wanted: &[(String, String)]) -> bool {
    let Ok(root) = load(settings_path) else {
        return false;
    };
    let env = root.get("env").and_then(Value::as_object);
    wanted.iter().all(|(k, v)| {
        let held = env.and_then(|e| e.get(k)).and_then(Value::as_str);
        held == Some(v) || (k == ENABLE_VAR && is_on(held))
    })
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
        for (k, v) in recorded_keys(&written) {
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
        let all_eight = {
            let env: Map<String, Value> =
                crate::cc::telemetry::env_for(&crate::usage::collector::State {
                    port: 47318,
                    token: "tok".into(),
                    ..Default::default()
                })
                .into_iter()
                .map(|(k, v)| (k, json!(v)))
                .collect();
            json!({ "env": env }).to_string()
        };
        // A switch alone now has keys to add, so the question is asked.
        let cases = [
            (on, None),
            (all_eight.as_str(), Some(ENABLED_LINE)),
            (own, Some(SET_UP_LINE)),
            (beside, Some(OFF_ELSEWHERE_LINE)),
            (lone, None),
            ("{}", None),
        ];
        for (i, (settings, expected)) in cases.iter().enumerate() {
            let (home, claude, cwd) = setup(&format!("init-tel-skip-{i}"), settings);
            write_collector(&home, 47318, "tok");

            let got = skip_line_files_only(&home, &claude, &cwd);

            assert_eq!(got, *expected, "{settings}");
        }
    }

    /// `skip_line` without the process environment, so the test machine
    /// cannot change the answer. It shares `skip_line_from` and
    /// `needs_action` with the real one.
    fn skip_line_files_only(home: &Path, claude: &Path, cwd: &Path) -> Option<&'static str> {
        let found = state(home, cwd, false);
        let can_replace =
            found == State::Disabled && can_replace_off_switch(home, claude, cwd, false);
        skip_line_from(found, needs_action(home, claude, cwd, false), can_replace)
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

    // ---- repair_needed ----

    fn wanted_set() -> Vec<(String, String)> {
        crate::cc::telemetry::env_for(&crate::usage::collector::State {
            port: 47318,
            token: "tok".into(),
            ..crate::usage::collector::State::default()
        })
    }

    fn map(pairs: &[(&str, &str)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), json!(v)))
            .collect()
    }

    /// The eight wanted keys with `edits` applied: `Some` sets, `None` removes.
    fn full_env(edits: &[(&str, Option<&str>)]) -> Map<String, Value> {
        let mut env: Map<String, Value> = wanted_set()
            .into_iter()
            .map(|(k, v)| (k, json!(v)))
            .collect();
        for (k, v) in edits {
            match v {
                Some(v) => env.insert((*k).to_string(), json!(v)),
                None => env.remove(*k),
            };
        }
        env
    }

    fn names(list: &[&str]) -> Repair {
        Repair::Keys(list.iter().map(|s| (*s).to_string()).collect())
    }

    const SEVEN: [&str; 7] = [
        "OTEL_LOGS_EXPORTER",
        "OTEL_METRICS_EXPORTER",
        "OTEL_TRACES_EXPORTER",
        "OTEL_EXPORTER_OTLP_PROTOCOL",
        "OTEL_EXPORTER_OTLP_ENDPOINT",
        "OTEL_EXPORTER_OTLP_HEADERS",
        "OTEL_METRICS_INCLUDE_ACCOUNT_UUID",
    ];

    fn decide(env: &Map<String, Value>, record: &Map<String, Value>) -> Repair {
        repair_needed(env, record, &wanted_set(), false)
    }

    #[test]
    fn repair_needed_classifies_each_switch_only_and_record_case() {
        let on = map(&[("CLAUDE_CODE_ENABLE_TELEMETRY", "1")]);
        let on_true = map(&[("CLAUDE_CODE_ENABLE_TELEMETRY", "true")]);
        let none = Map::new();
        let only_replaced = {
            let mut m = Map::new();
            m.insert(
                REPLACED.to_string(),
                json!({"CLAUDE_CODE_ENABLE_TELEMETRY": "0"}),
            );
            m
        };
        assert_eq!(decide(&on, &none), names(&SEVEN));
        assert_eq!(decide(&on_true, &none), names(&SEVEN));
        assert_eq!(
            decide(&on, &only_replaced),
            names(&SEVEN),
            "meta only is no record"
        );
        // A dead record: nothing it names holds its value.
        let dead = map(&[("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:1")]);
        assert_eq!(decide(&on, &dead), names(&SEVEN));
    }

    #[test]
    fn repair_needed_with_a_live_record() {
        let record = full_env(&[]);
        // a deleted key comes back
        let env = full_env(&[("OTEL_EXPORTER_OTLP_PROTOCOL", None)]);
        assert_eq!(
            decide(&env, &record),
            names(&["OTEL_EXPORTER_OTLP_PROTOCOL"])
        );
        // a moved port: the endpoint still holds the recorded value
        let old = full_env(&[("OTEL_EXPORTER_OTLP_ENDPOINT", Some("http://127.0.0.1:1111"))]);
        assert_eq!(
            repair_needed(&old, &old, &wanted_set(), false),
            names(&["OTEL_EXPORTER_OTLP_ENDPOINT"])
        );
        // the switch recorded as 1, now true, everything else wanted
        let spelled = full_env(&[("CLAUDE_CODE_ENABLE_TELEMETRY", Some("true"))]);
        assert_eq!(decide(&spelled, &record), Repair::NotNeeded);
        // the user changed a recorded key
        let own = full_env(&[("OTEL_EXPORTER_OTLP_ENDPOINT", Some("https://team"))]);
        assert_eq!(decide(&own, &record), Repair::Foreign);
        let console = full_env(&[("OTEL_LOGS_EXPORTER", Some("console"))]);
        assert_eq!(decide(&console, &record), Repair::Foreign);
        // an extra OTEL key of the user's is ignored
        let extra = full_env(&[
            ("OTEL_RESOURCE_ATTRIBUTES", Some("team=a")),
            ("OTEL_EXPORTER_OTLP_PROTOCOL", None),
        ]);
        assert_eq!(
            decide(&extra, &record),
            names(&["OTEL_EXPORTER_OTLP_PROTOCOL"])
        );
        // all eight hold the wanted values
        assert_eq!(decide(&full_env(&[]), &record), Repair::NotNeeded);
        // the switch was deleted from the file under a live record
        let no_switch = full_env(&[("CLAUDE_CODE_ENABLE_TELEMETRY", None)]);
        assert_eq!(
            decide(&no_switch, &record),
            names(&["CLAUDE_CODE_ENABLE_TELEMETRY"])
        );
    }

    #[test]
    fn repair_needed_with_a_dead_record_or_none_and_a_full_stale_set() {
        let none = Map::new();
        let stale = full_env(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", Some("http://127.0.0.1:1")),
            (
                "OTEL_EXPORTER_OTLP_HEADERS",
                Some("Authorization=Bearer old"),
            ),
        ]);
        let both = names(&["OTEL_EXPORTER_OTLP_ENDPOINT", "OTEL_EXPORTER_OTLP_HEADERS"]);
        assert_eq!(decide(&stale, &none), both);
        // a dead record names values that are all gone
        let dead = map(&[("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:9")]);
        assert_eq!(decide(&stale, &dead), both);
        // localhost with another port and the wanted token: the endpoint only
        let local = full_env(&[("OTEL_EXPORTER_OTLP_ENDPOINT", Some("http://localhost:1234"))]);
        assert_eq!(
            decide(&local, &none),
            names(&["OTEL_EXPORTER_OTLP_ENDPOINT"])
        );
        // the port bounds and the optional slash are accepted
        for endpoint in ["http://127.0.0.1:1/", "http://127.0.0.1:65535"] {
            let env = full_env(&[
                ("OTEL_EXPORTER_OTLP_ENDPOINT", Some(endpoint)),
                (
                    "OTEL_EXPORTER_OTLP_HEADERS",
                    Some("Authorization=Bearer old"),
                ),
            ]);
            assert_eq!(decide(&env, &none), both, "{endpoint}");
        }
        // all eight hold the wanted values with no record
        assert_eq!(decide(&full_env(&[]), &none), Repair::NotNeeded);
    }

    #[test]
    fn repair_needed_leaves_every_other_setup_alone() {
        let none = Map::new();
        type Edits<'a> = &'a [(&'a str, Option<&'a str>)];
        let foreign_edits: &[Edits] = &[
            &[(
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                Some("https://127.0.0.1:4318"),
            )],
            &[(
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                Some("http://localhost.example.test:4318"),
            )],
            &[("OTEL_EXPORTER_OTLP_ENDPOINT", Some("http://127.0.0.1:"))],
            &[("OTEL_EXPORTER_OTLP_ENDPOINT", Some("http://127.0.0.1:0"))],
            &[(
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                Some("http://127.0.0.1:70000"),
            )],
            &[(
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                Some("http://127.0.0.1:4318x"),
            )],
            &[(
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                Some("http://127.0.0.1:4318/v1"),
            )],
            &[("OTEL_EXPORTER_OTLP_HEADERS", Some("Authorization=Bearer "))],
            &[("OTEL_EXPORTER_OTLP_HEADERS", Some("Authorization=Basic x"))],
            &[("OTEL_EXPORTER_OTLP_PROTOCOL", Some("grpc"))],
            &[(
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                Some("https://collector.example.test"),
            )],
            &[("CLAUDE_CODE_ENABLE_TELEMETRY", Some("0"))],
            &[("CLAUDE_CODE_ENABLE_TELEMETRY", Some(""))],
            &[("CLAUDE_CODE_ENABLE_TELEMETRY", None)],
        ];
        for edits in foreign_edits {
            assert_eq!(
                decide(&full_env(edits), &none),
                Repair::Foreign,
                "{edits:?}"
            );
        }
        let own_collector = map(&[
            ("CLAUDE_CODE_ENABLE_TELEMETRY", "1"),
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://localhost:4318"),
            ("OTEL_EXPORTER_OTLP_HEADERS", "Authorization=Bearer x"),
            ("OTEL_LOGS_EXPORTER", "otlp"),
            ("OTEL_METRICS_EXPORTER", "otlp"),
        ]);
        assert_eq!(decide(&own_collector, &none), Repair::Foreign);
        let dead = map(&[("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:9")]);
        assert_eq!(decide(&own_collector, &dead), Repair::Foreign);
        let minimal = map(&[
            ("CLAUDE_CODE_ENABLE_TELEMETRY", "1"),
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://localhost:4318"),
            ("OTEL_LOGS_EXPORTER", "otlp"),
            ("OTEL_METRICS_EXPORTER", "otlp"),
        ]);
        assert_eq!(decide(&minimal, &none), Repair::Foreign);
        let no_endpoint = map(&[
            ("CLAUDE_CODE_ENABLE_TELEMETRY", "1"),
            ("OTEL_LOGS_EXPORTER", "otlp"),
            ("OTEL_METRICS_EXPORTER", "otlp"),
        ]);
        assert_eq!(decide(&no_endpoint, &none), Repair::Foreign);
        // another source names a telemetry value
        let on = map(&[("CLAUDE_CODE_ENABLE_TELEMETRY", "1")]);
        assert_eq!(
            repair_needed(&on, &none, &wanted_set(), true),
            Repair::Foreign
        );
    }

    // ---- repair in `apply` ----

    fn settings_bytes(claude: &Path) -> Vec<u8> {
        fs::read(claude.join("settings.json")).unwrap()
    }

    fn collector_state(home: &Path) -> crate::usage::collector::State {
        let paths = crate::usage::run::Paths::from_home(home);
        crate::usage::collector::read_state(&crate::usage::collector::state_path(&paths)).unwrap()
    }

    fn write_collector(home: &Path, port: u16, token: &str) {
        let paths = crate::usage::run::Paths::from_home(home);
        let file = crate::usage::collector::state_path(&paths);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let state = crate::usage::collector::State {
            port,
            token: token.into(),
            ..crate::usage::collector::State::default()
        };
        fs::write(file, serde_json::to_vec(&state).unwrap()).unwrap();
    }

    fn collector_file(home: &Path) -> PathBuf {
        crate::usage::collector::state_path(&crate::usage::run::Paths::from_home(home))
    }

    fn record_of(home: &Path) -> Map<String, Value> {
        load(&marker_path(home)).unwrap()
    }

    fn set_env_key(claude: &Path, key: &str, value: Option<&str>) {
        let path = claude.join("settings.json");
        let mut doc: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let env = doc["env"].as_object_mut().unwrap();
        match value {
            Some(v) => env.insert(key.to_string(), json!(v)),
            None => env.remove(key),
        };
        fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    }

    const WIRED: crate::init::run::StepStatus = crate::init::run::StepStatus::Wired;
    const OK: crate::init::run::StepStatus = crate::init::run::StepStatus::AlreadyCorrect;
    const SKIPPED: crate::init::run::StepStatus = crate::init::run::StepStatus::Skipped;

    fn keys_in(claude: &Path) -> Vec<String> {
        let mut keys: Vec<String> = env_of(claude)
            .keys()
            .filter(|k| crate::usage::telemetry_detect::is_telemetry_var(k))
            .cloned()
            .collect();
        keys.sort();
        keys
    }

    fn all_eight() -> Vec<String> {
        let mut keys: Vec<String> = crate::cc::telemetry::env_for(&Default::default())
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        keys.sort();
        keys
    }

    #[test]
    fn a_lone_on_switch_gets_the_seven_keys_once_and_uninstall_keeps_the_switch() {
        let (home, claude, cwd) = setup(
            "init-tel-switch-only",
            r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"1","FOO":"1"}}"#,
        );

        let first = apply(&home, &claude, &cwd, 1, false, true);
        let after_first = settings_bytes(&claude);
        let second = apply(&home, &claude, &cwd, 2, false, true);

        assert_eq!(first.status, WIRED);
        assert!(
            first.detail.contains("OTEL_LOGS_EXPORTER"),
            "{}",
            first.detail
        );
        assert_eq!(keys_in(&claude), all_eight());
        assert_eq!(second.status, OK);
        assert_eq!(settings_bytes(&claude), after_first, "byte for byte");
        let record = record_of(&home);
        assert!(!record.contains_key("CLAUDE_CODE_ENABLE_TELEMETRY"));
        assert!(!record.contains_key(REPLACED));

        remove(&home, &claude.join("settings.json"), 3, false).unwrap();

        let env = env_of(&claude);
        assert_eq!(env["CLAUDE_CODE_ENABLE_TELEMETRY"], "1");
        assert_eq!(env["FOO"], "1");
        assert_eq!(keys_in(&claude), vec!["CLAUDE_CODE_ENABLE_TELEMETRY"]);
    }

    #[test]
    fn a_lone_true_switch_stays_true_through_repair_and_a_moved_port() {
        let (home, claude, cwd) = setup(
            "init-tel-true",
            r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"true"}}"#,
        );

        let first = apply(&home, &claude, &cwd, 1, false, true);
        let after_first = settings_bytes(&claude);
        let second = apply(&home, &claude, &cwd, 2, false, true);

        assert_eq!(first.status, WIRED);
        assert_eq!(second.status, OK);
        assert_eq!(settings_bytes(&claude), after_first);

        let mut state = collector_state(&home);
        state.port = 51234;
        fs::write(collector_file(&home), serde_json::to_vec(&state).unwrap()).unwrap();
        let third = apply(&home, &claude, &cwd, 3, false, true);

        let env = env_of(&claude);
        assert_eq!(third.status, WIRED);
        assert_eq!(env["OTEL_EXPORTER_OTLP_ENDPOINT"], "http://127.0.0.1:51234");
        assert_eq!(env["CLAUDE_CODE_ENABLE_TELEMETRY"], "true");
        let record = record_of(&home);
        assert!(!record.contains_key("CLAUDE_CODE_ENABLE_TELEMETRY"));
        assert!(!record.contains_key(REPLACED));
    }

    #[test]
    fn a_deleted_key_under_a_live_record_comes_back_and_uninstall_restores_the_off_switch() {
        let (home, claude, cwd) = setup(
            "init-tel-deleted-key",
            r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"0","FOO":"1"}}"#,
        );
        apply(&home, &claude, &cwd, 1, false, true);
        set_env_key(&claude, "OTEL_EXPORTER_OTLP_PROTOCOL", None);
        let before = env_of(&claude);

        let step = apply(&home, &claude, &cwd, 2, false, false);

        let after = env_of(&claude);
        assert_eq!(step.status, WIRED);
        assert!(step.detail.contains("OTEL_EXPORTER_OTLP_PROTOCOL"));
        assert_eq!(after["OTEL_EXPORTER_OTLP_PROTOCOL"], "http/json");
        let changed: Vec<&String> = after
            .keys()
            .filter(|k| before.get(*k) != after.get(*k))
            .collect();
        assert_eq!(changed, vec!["OTEL_EXPORTER_OTLP_PROTOCOL"]);

        remove(&home, &claude.join("settings.json"), 3, false).unwrap();

        let env = env_of(&claude);
        assert_eq!(env["CLAUDE_CODE_ENABLE_TELEMETRY"], "0");
        assert_eq!(env["FOO"], "1");
        assert_eq!(keys_in(&claude), vec!["CLAUDE_CODE_ENABLE_TELEMETRY"]);
    }

    #[test]
    fn a_live_record_with_a_true_switch_and_a_deleted_key_writes_only_that_key() {
        let (home, claude, cwd) = setup("init-tel-live-true", r#"{"env":{}}"#);
        apply(&home, &claude, &cwd, 1, false, true);
        set_env_key(&claude, "CLAUDE_CODE_ENABLE_TELEMETRY", Some("true"));
        set_env_key(&claude, "OTEL_EXPORTER_OTLP_PROTOCOL", None);

        let step = apply(&home, &claude, &cwd, 2, false, true);

        let env = env_of(&claude);
        assert_eq!(step.status, WIRED);
        assert_eq!(env["CLAUDE_CODE_ENABLE_TELEMETRY"], "true");
        assert_eq!(env["OTEL_EXPORTER_OTLP_PROTOCOL"], "http/json");
    }

    #[test]
    fn a_switch_the_user_respelled_since_the_record_needs_nothing() {
        let (home, claude, cwd) = setup("init-tel-respelled", r#"{"env":{}}"#);
        apply(&home, &claude, &cwd, 1, false, true);
        set_env_key(&claude, "CLAUDE_CODE_ENABLE_TELEMETRY", Some("true"));
        let before = settings_bytes(&claude);

        let step = apply(&home, &claude, &cwd, 2, false, true);

        assert_eq!(step.status, OK);
        assert_eq!(step.detail, ALREADY_POINTS_LINE);
        assert_eq!(settings_bytes(&claude), before);
    }

    #[test]
    fn a_full_stale_set_with_no_record_is_repaired_and_uninstall_puts_the_stale_values_back() {
        let (home, claude, cwd) = setup("init-tel-stale-set", r#"{}"#);
        write_collector(&home, 47318, "newtok");
        let mut env = Map::new();
        for (k, v) in crate::cc::telemetry::env_for(&collector_state(&home)) {
            env.insert(k, json!(v));
        }
        env.insert(
            "OTEL_EXPORTER_OTLP_ENDPOINT".into(),
            json!("http://127.0.0.1:1111"),
        );
        env.insert(
            "OTEL_EXPORTER_OTLP_HEADERS".into(),
            json!("Authorization=Bearer oldtok"),
        );
        let doc = json!({ "env": env });
        fs::write(claude.join("settings.json"), doc.to_string()).unwrap();

        let step = apply(&home, &claude, &cwd, 1, false, true);

        let fixed = env_of(&claude);
        assert_eq!(step.status, WIRED);
        assert_eq!(
            fixed["OTEL_EXPORTER_OTLP_ENDPOINT"],
            "http://127.0.0.1:47318"
        );
        assert_eq!(
            fixed["OTEL_EXPORTER_OTLP_HEADERS"],
            "Authorization=Bearer newtok"
        );

        remove(&home, &claude.join("settings.json"), 2, false).unwrap();

        let back = env_of(&claude);
        assert_eq!(back["OTEL_EXPORTER_OTLP_ENDPOINT"], "http://127.0.0.1:1111");
        assert_eq!(
            back["OTEL_EXPORTER_OTLP_HEADERS"],
            "Authorization=Bearer oldtok"
        );
    }

    #[test]
    fn no_record_and_all_eight_wanted_keys_is_already_correct_and_writes_nothing() {
        let (home, claude, cwd) = setup("init-tel-all-eight", r#"{}"#);
        write_collector(&home, 47318, "tok");
        let mut env = Map::new();
        for (k, v) in crate::cc::telemetry::env_for(&collector_state(&home)) {
            env.insert(k, json!(v));
        }
        fs::write(
            claude.join("settings.json"),
            json!({ "env": env }).to_string(),
        )
        .unwrap();
        let before = settings_bytes(&claude);

        let step = apply(&home, &claude, &cwd, 1, false, true);

        assert_eq!(step.status, OK);
        assert_eq!(step.detail, ALREADY_POINTS_LINE);
        assert!(!marker_path(&home).exists(), "no record is written");
        assert_eq!(settings_bytes(&claude), before);
    }

    #[test]
    fn a_token_change_rewrites_only_the_headers() {
        let (home, claude, cwd) = setup("init-tel-token", r#"{}"#);
        apply(&home, &claude, &cwd, 1, false, true);
        let mut state = collector_state(&home);
        state.token = "rotated".into();
        fs::write(collector_file(&home), serde_json::to_vec(&state).unwrap()).unwrap();

        let step = apply(&home, &claude, &cwd, 2, false, true);

        assert_eq!(step.status, WIRED);
        assert_eq!(
            env_of(&claude)["OTEL_EXPORTER_OTLP_HEADERS"],
            "Authorization=Bearer rotated"
        );
    }

    #[test]
    fn a_foreign_setup_is_skipped_and_left_byte_for_byte() {
        let (home, claude, cwd) = setup(
            "init-tel-foreign",
            r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"1","OTEL_EXPORTER_OTLP_ENDPOINT":"https://team"}}"#,
        );
        let before = settings_bytes(&claude);

        let step = apply(&home, &claude, &cwd, 1, false, true);

        assert_eq!(step.status, SKIPPED);
        assert!(step
            .detail
            .contains("you already have telemetry configured"));
        assert_eq!(settings_bytes(&claude), before);
    }

    #[test]
    fn an_unparsable_settings_file_is_skipped_and_never_asks_for_state() {
        let (home, claude, cwd) = setup("init-tel-broken", "{ not json");

        let step = apply(&home, &claude, &cwd, 1, false, true);

        assert_eq!(step.status, SKIPPED);
        assert_eq!(settings_bytes(&claude), b"{ not json");
        assert!(!collector_file(&home).exists());
    }

    #[test]
    fn a_home_with_no_collector_state_gets_it_made_for_a_repair() {
        let (home, claude, cwd) = setup(
            "init-tel-no-state",
            r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"1"}}"#,
        );
        assert!(!collector_file(&home).exists());

        let step = apply(&home, &claude, &cwd, 1, false, true);

        let state = collector_state(&home);
        assert_eq!(step.status, WIRED);
        assert_eq!(
            env_of(&claude)["OTEL_EXPORTER_OTLP_ENDPOINT"],
            format!("http://127.0.0.1:{}", state.port)
        );
    }

    #[test]
    fn another_source_means_no_state_is_made_and_nothing_is_written() {
        let (home, claude, cwd) = setup(
            "init-tel-elsewhere",
            r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"1"}}"#,
        );
        let before = settings_bytes(&claude);

        let step = apply(&home, &claude, &cwd, 1, true, true);

        assert_eq!(step.status, SKIPPED);
        assert!(!collector_file(&home).exists(), "ensure_state is skipped");
        assert_eq!(settings_bytes(&claude), before);
    }

    #[test]
    fn a_record_that_holds_only_replaced_is_no_record_and_the_switch_stays() {
        let (home, claude, cwd) = setup(
            "init-tel-only-replaced",
            r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"1"}}"#,
        );
        fs::create_dir_all(marker_path(&home).parent().unwrap()).unwrap();
        fs::write(
            marker_path(&home),
            r#"{"_replaced":{"CLAUDE_CODE_ENABLE_TELEMETRY":"0"}}"#,
        )
        .unwrap();

        let step = apply(&home, &claude, &cwd, 1, false, true);

        let record = record_of(&home);
        assert_eq!(step.status, WIRED);
        assert_eq!(keys_in(&claude), all_eight());
        assert_eq!(env_of(&claude)["CLAUDE_CODE_ENABLE_TELEMETRY"], "1");
        assert!(
            !record.contains_key(REPLACED),
            "the user's switch is not recorded as replaced: {record:?}"
        );
    }

    #[test]
    fn the_skip_table_covers_state_repair_and_can_replace() {
        let cases = [
            (State::Enabled, true, false, None),
            (State::Enabled, false, false, Some(ENABLED_LINE)),
            (State::SetUpByYou, true, false, None),
            (State::SetUpByYou, false, false, Some(SET_UP_LINE)),
            (State::Unset, false, false, None),
            (State::Unset, true, true, None),
            (State::Disabled, false, true, None),
            (State::Disabled, true, true, None),
            (State::Disabled, false, false, Some(OFF_ELSEWHERE_LINE)),
            (State::Disabled, true, false, Some(OFF_ELSEWHERE_LINE)),
        ];
        for (state, repair, can_replace, expected) in cases {
            assert_eq!(
                skip_line_from(state, repair, can_replace),
                expected,
                "{state:?} repair={repair} can_replace={can_replace}"
            );
        }
    }

    #[test]
    fn the_wizard_asks_when_a_yes_can_repair_and_keeps_the_line_for_a_foreign_setup() {
        // (settings, collector.json first, expected)
        let full_stale = r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"1","OTEL_LOGS_EXPORTER":"otlp","OTEL_METRICS_EXPORTER":"otlp","OTEL_TRACES_EXPORTER":"none","OTEL_EXPORTER_OTLP_PROTOCOL":"http/json","OTEL_EXPORTER_OTLP_ENDPOINT":"http://127.0.0.1:1111","OTEL_EXPORTER_OTLP_HEADERS":"Authorization=Bearer old","OTEL_METRICS_INCLUDE_ACCOUNT_UUID":"false"}}"#;
        let own = r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"1","OTEL_EXPORTER_OTLP_ENDPOINT":"https://team"}}"#;
        let lone = r#"{"env":{"CLAUDE_CODE_ENABLE_TELEMETRY":"1"}}"#;
        let cases = [
            (lone, true, None),
            (lone, false, None),
            (full_stale, false, None),
            (own, true, Some(ENABLED_LINE)),
        ];
        for (i, (settings, with_state, expected)) in cases.iter().enumerate() {
            let (home, claude, cwd) = setup(&format!("init-tel-wizard-{i}"), settings);
            if *with_state {
                write_collector(&home, 47318, "tok");
            }
            assert_eq!(
                skip_line_files_only(&home, &claude, &cwd),
                *expected,
                "{settings} state={with_state}"
            );
        }
    }

    #[test]
    fn a_full_correct_set_asks_nothing_but_a_deleted_switch_under_a_live_record_is_asked() {
        let (home, claude, cwd) = setup("init-tel-wizard-live", r#"{}"#);
        apply(&home, &claude, &cwd, 1, false, true);
        assert_eq!(
            skip_line_files_only(&home, &claude, &cwd),
            Some(ENABLED_LINE),
            "all eight keys in place"
        );

        set_env_key(&claude, "CLAUDE_CODE_ENABLE_TELEMETRY", None);

        assert_eq!(
            skip_line_files_only(&home, &claude, &cwd),
            None,
            "the repair puts the switch back, so the wizard asks"
        );
        let step = apply(&home, &claude, &cwd, 2, false, true);
        assert_eq!(step.status, WIRED);
        assert_eq!(env_of(&claude)["CLAUDE_CODE_ENABLE_TELEMETRY"], "1");
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
