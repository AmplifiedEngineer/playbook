// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Points a `cc` session at the local usage receiver, for that session only.
//!
//! The variables go in the launcher's own environment, which the child
//! inherits, the same way `memory.source` sets its switch. Nothing is written:
//! Claude Code's settings files are only read, to find out whether the user
//! already configured telemetry, and no `--settings` flag is added.
//!
//! It acts only when all of these hold, else it sets nothing:
//!
//! - `usage.telemetry` is `auto` (the default; `off` turns it off),
//! - no telemetry variable is set in the process environment or in the `env`
//!   block of a Claude Code settings file (user, project, local, managed),
//! - a live `playbook usage collect` receiver is recorded in `collector.json`.
//!
//! The collector itself is started by `ensure_for`: the `session-init` hook,
//! the two prompt-only hooks and most playbook commands (`cc` included) ask
//! for one, when telemetry points at it. It is a daemon that holds a lock for
//! its whole life (see `playbook_telemetry::daemon`), so a running one costs a
//! starter one open and one `try_lock`. Nothing outside playbook's own data
//! directory is written: no launchd or systemd file and no Claude Code setting.
//!
//! Prompt, tool detail, tool content and raw body logging are never turned on
//! and the account uuid is left out of the metrics.

use crate::usage::collector::{read_state, running, state_path, State};
pub use crate::usage::daemon::Ensured;
use crate::usage::daemon::{self, EnsureOpts, LockState};
use crate::usage::run::Paths;
pub use crate::usage::telemetry_detect::{
    configured, files_configured, process_configured, Configured,
};
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

/// The variables for a session pointed at `state`.
pub fn env_for(state: &State) -> Vec<(String, String)> {
    let pairs = [
        ("CLAUDE_CODE_ENABLE_TELEMETRY", "1".to_string()),
        ("OTEL_LOGS_EXPORTER", "otlp".to_string()),
        ("OTEL_METRICS_EXPORTER", "otlp".to_string()),
        ("OTEL_TRACES_EXPORTER", "none".to_string()),
        ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/json".to_string()),
        (
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            format!("http://127.0.0.1:{}", state.port),
        ),
        (
            "OTEL_EXPORTER_OTLP_HEADERS",
            format!("Authorization=Bearer {}", state.token),
        ),
        ("OTEL_METRICS_INCLUDE_ACCOUNT_UUID", "false".to_string()),
    ];
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// The decision, with its inputs given so it can be tested.
pub fn decide(
    mode_auto: bool,
    process_configured: bool,
    files_configured: bool,
    collector: Option<&State>,
) -> Vec<(String, String)> {
    if !mode_auto || process_configured || files_configured {
        return Vec::new();
    }
    match collector {
        Some(state) if running(state) => env_for(state),
        _ => Vec::new(),
    }
}

/// The variables to set for a session started in `cwd`, empty when none.
/// It only reads the recorded state: the command trigger (`on_start`) has
/// already asked for a collector before `cc` gets here.
pub fn session_env(home: &Path, cwd: &Path) -> Vec<(String, String)> {
    if !telemetry_auto(home) {
        return Vec::new();
    }
    let collector = read_state(&state_path(&Paths::from_home(home)));
    let kind = configured(home, cwd, collector.as_ref().map(|s| s.port));
    decide(true, kind != Configured::Nothing, false, collector.as_ref())
}

/// What asks for a collector. Each has its own wait and stop budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// The `session-init` hook: may wait, probe and kill a hung collector.
    SessionStart,
    /// A hook wired only on `UserPromptSubmit`: no wait, no network call
    /// while a collector at this version runs.
    Prompt,
    /// A playbook command (see `on_start`): like `Prompt`.
    Command,
}

/// How long the session hook waits for a new collector to listen. Short,
/// because the session start must not feel it.
const SESSION_START_WAIT: Duration = Duration::from_millis(1200);
/// How long the session hook gives a collector to stop before a `SIGKILL`.
const SESSION_STOP_BUDGET: Duration = Duration::from_secs(2);
/// How long a prompt or a command waits for an old collector to hand over.
const QUICK_STOP_BUDGET: Duration = Duration::from_millis(400);

impl Trigger {
    fn opts(self) -> EnsureOpts {
        match self {
            Trigger::SessionStart => EnsureOpts {
                wait: SESSION_START_WAIT,
                probe: true,
                stop_budget: SESSION_STOP_BUDGET,
                may_kill: true,
            },
            Trigger::Prompt | Trigger::Command => EnsureOpts {
                wait: Duration::ZERO,
                probe: false,
                stop_budget: QUICK_STOP_BUDGET,
                may_kill: false,
            },
        }
    }
}

/// Test-only opt-in. A debug build that cargo or nextest started spawns no
/// collector without it: a spawned collector never idles out, so a test home
/// that sets telemetry up would leave one running on a free port for good.
pub const TEST_SPAWN_VAR: &str = "PLAYBOOK_COLLECTOR_TEST_SPAWN";

/// Whether this process may spawn or replace a collector.
fn spawn_allowed() -> bool {
    let test_run = cfg!(debug_assertions) && crate::common::real_home::started_by_cargo();
    !test_run || std::env::var_os(TEST_SPAWN_VAR).is_some_and(|v| v == "1")
}

/// Whether `usage.telemetry` is `auto`.
fn telemetry_auto(home: &Path) -> bool {
    matches!(
        crate::config::resolve_valid("usage.telemetry", home, None),
        Ok((Value::String(v), _, _)) if v == "auto"
    )
}

/// Whether the Claude Code user settings point telemetry at the playbook
/// collector, the state the `telemetry` setup part writes. This is the
/// consent for starting one.
pub fn telemetry_points_here(home: &Path) -> bool {
    use crate::usage::telemetry_detect::configured_from_files;
    let port = read_state(&state_path(&Paths::from_home(home)))
        .map(|s| s.port)
        .filter(|p| *p != 0);
    configured_from_files(home, home, port) == Configured::Playbook
}

/// The line the session-init hook prints when this session started without
/// the OpenTelemetry keys that Claude Code settings now hold. Claude Code
/// reads `env` once at start, so a session begun before `setup` wrote the
/// keys exports nothing until it restarts. It reads the `usage.telemetry`
/// setting, the collector state, the user and managed settings files (with
/// the home as the working directory, so a repo's own settings are not read)
/// and one variable of the hook's own environment, which is the session's.
pub fn missing_keys_line(home: &Path) -> Option<&'static str> {
    if !telemetry_auto(home) || !telemetry_points_here(home) {
        return None;
    }
    let endpoint = std::env::var_os("OTEL_EXPORTER_OTLP_ENDPOINT");
    keys_line(endpoint.is_some_and(|v| !v.is_empty()))
}

const MISSING_KEYS: &str = "playbook: this session started without the OpenTelemetry keys, so it will not send telemetry. Restart it to load them.";

/// The caller has already found that the settings hold the playbook keys.
/// `session_has_endpoint` says the session's environment holds them too.
fn keys_line(session_has_endpoint: bool) -> Option<&'static str> {
    (!session_has_endpoint).then_some(MISSING_KEYS)
}

/// Makes sure a collector at this version runs for the user's home, as
/// `trigger` allows. Never panics on its own and never prints.
pub fn ensure_for(trigger: Trigger) -> Ensured {
    let Ok(exe) = std::env::current_exe() else {
        return Ensured::NotConfigured;
    };
    ensure_at(&crate::common::home_dir(), &exe, trigger, false)
}

/// Like `ensure_for(Command)`, but also for a headless process. The service
/// removal migration stops the collector the service ran, and open sessions
/// of any kind still send to it, so the migration restarts one even when the
/// heal that runs it is headless.
pub fn ensure_after_removal() -> Ensured {
    let Ok(exe) = std::env::current_exe() else {
        return Ensured::NotConfigured;
    };
    ensure_at(&crate::common::home_dir(), &exe, Trigger::Command, true)
}

/// The gates, cheapest first, then `daemon::ensure`. A collector at this
/// version costs the file gates plus one open and one `try_lock`. The
/// `usage.telemetry` lookup can open `config.db`, so it runs after the lock
/// check, with the consent check, whenever `daemon::ensure` could spawn: a
/// missing or older holder, or a probe, which replaces a silent one. Only a
/// current holder with no probe skips them.
fn ensure_at(home: &Path, exe: &Path, trigger: Trigger, allow_headless: bool) -> Ensured {
    if crate::common::home_error(home).is_some() {
        return Ensured::NotConfigured;
    }
    let paths = Paths::from_home(home);
    let recorded =
        read_state(&state_path(&paths)).is_some_and(|s| s.port != 0 && !s.token.is_empty());
    if !recorded
        || (!allow_headless && crate::common::headless::is_headless())
        || crate::common::real_home::refused(home)
    {
        return Ensured::NotConfigured;
    }
    let opts = trigger.opts();
    // A holder still starting (no version yet) counts as current.
    let current = matches!(
        daemon::lock_state(&paths),
        LockState::Held(h) if h.version.as_deref().is_none_or(|v| v == env!("CARGO_PKG_VERSION"))
    );
    if current && !opts.probe {
        return Ensured::Running;
    }
    // Past this point `ensure` may spawn: a missing or older holder, or a
    // probe that finds a current one silent and replaces it.
    if !telemetry_auto(home) || !spawn_allowed() || !telemetry_points_here(home) {
        return Ensured::NotConfigured;
    }
    daemon::ensure(&paths, exe, opts)
}

/// The two hooks wired only on `UserPromptSubmit`. The others that also
/// run there (`memory-anchors`, `auto-guard`) are PreToolUse hooks too, and
/// no tool hook may start anything.
pub fn is_prompt_trigger(name: crate::HookName) -> bool {
    matches!(
        name,
        crate::HookName::AutoModelDetect | crate::HookName::ContextHygiene
    )
}

/// The prompt trigger for hook `name`: asks for a collector when it is one
/// of the prompt-only hooks. A panic is dropped, so the hook never fails.
pub fn on_hook(name: crate::HookName) {
    if is_prompt_trigger(name) {
        let _ = std::panic::catch_unwind(|| ensure_for(Trigger::Prompt));
    }
}

/// Commands that never start a collector: the hot paths, the ones that
/// inspect or change the install, and the collector's own commands.
fn skips_start(command: &crate::Command) -> bool {
    use crate::Command as C;
    use crate::UsageCommand as U;
    command.is_hot_path()
        || matches!(
            command,
            C::Version
                | C::Doctor { .. }
                | C::Update { .. }
                | C::Uninstall { .. }
                | C::Migrate { .. }
                | C::Setup { .. }
                | C::Init { .. }
                | C::State { .. }
                | C::Usage {
                    sub: Some(U::Collect { .. } | U::Warm | U::Prune { .. } | U::Stop),
                    ..
                }
        )
}

/// The command trigger: before `command` runs, asks for a collector unless
/// the command is skipped. A panic is dropped, so the command still runs.
pub fn on_start(command: &crate::Command) {
    if !skips_start(command) {
        let _ = std::panic::catch_unwind(|| ensure_for(Trigger::Command));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_warning_shows_only_when_the_session_lacks_the_endpoint() {
        assert!(keys_line(false).is_some_and(|l| l.contains("Restart")));
        assert_eq!(keys_line(true), None, "all fine stays quiet");
    }

    #[test]
    fn the_warning_is_one_plain_line() {
        let line = keys_line(false).unwrap();
        assert!(!line.contains('\n') && !line.contains('\u{2014}') && !line.contains('\u{2013}'));
    }

    fn live() -> State {
        // Our own pid is alive; a bound listener makes the port answer.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::mem::forget(listener);
        State {
            pid: std::process::id(),
            port,
            token: "tok".into(),
            ..State::default()
        }
    }

    fn get<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
        env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    #[test]
    fn a_live_collector_and_no_user_config_sets_the_session_env() {
        let state = live();
        let env = decide(true, false, false, Some(&state));
        assert_eq!(get(&env, "CLAUDE_CODE_ENABLE_TELEMETRY"), Some("1"));
        assert_eq!(get(&env, "OTEL_EXPORTER_OTLP_PROTOCOL"), Some("http/json"));
        assert_eq!(
            get(&env, "OTEL_EXPORTER_OTLP_ENDPOINT"),
            Some(format!("http://127.0.0.1:{}", state.port).as_str())
        );
        assert_eq!(
            get(&env, "OTEL_EXPORTER_OTLP_HEADERS"),
            Some("Authorization=Bearer tok")
        );
        assert_eq!(
            get(&env, "OTEL_METRICS_INCLUDE_ACCOUNT_UUID"),
            Some("false")
        );
    }

    #[test]
    fn content_logging_is_never_turned_on() {
        let env = env_for(&live());
        for name in [
            "OTEL_LOG_USER_PROMPTS",
            "OTEL_LOG_TOOL_DETAILS",
            "OTEL_LOG_TOOL_CONTENT",
            "OTEL_LOG_RAW_API_BODIES",
        ] {
            assert!(get(&env, name).is_none(), "{name}");
        }
    }

    #[test]
    fn off_user_config_or_no_live_collector_sets_nothing() {
        let state = live();
        assert!(decide(false, false, false, Some(&state)).is_empty());
        assert!(decide(true, true, false, Some(&state)).is_empty());
        assert!(decide(true, false, true, Some(&state)).is_empty());
        assert!(decide(true, false, false, None).is_empty());
        let dead = State {
            pid: 0,
            port: state.port,
            token: "tok".into(),
            ..State::default()
        };
        assert!(decide(true, false, false, Some(&dead)).is_empty());
    }

    /// The prompt trigger is exactly the hooks wired only on
    /// `UserPromptSubmit`, so no tool hook ever starts a collector.
    #[test]
    fn only_hooks_wired_on_the_prompt_alone_are_prompt_triggers() {
        use clap::ValueEnum;
        for name in crate::HookName::value_variants() {
            let id = name.to_possible_value().unwrap();
            let events: Vec<&str> = crate::init::wire::wired_hooks()
                .filter(|h| h.name == id.get_name())
                .map(|h| h.event)
                .collect();
            let prompt_only = !events.is_empty() && events.iter().all(|e| *e == "UserPromptSubmit");
            assert_eq!(is_prompt_trigger(*name), prompt_only, "{}", id.get_name());
        }
    }

    #[test]
    fn the_skip_list_keeps_install_and_collector_commands_from_starting_one() {
        use clap::Parser;
        let parse = |args: &[&str]| {
            crate::Cli::try_parse_from(std::iter::once("playbook").chain(args.iter().copied()))
                .unwrap()
                .command
        };
        for args in [
            &["version"][..],
            &["doctor"],
            &["migrate"],
            &["setup", "--yes"],
            &["uninstall"],
            &["state", "list"],
            &["usage", "collect"],
            &["usage", "stop"],
            &["usage", "warm"],
            &["usage", "prune"],
            &["hook", "context-hygiene"],
        ] {
            assert!(skips_start(&parse(args)), "{args:?}");
        }
        for args in [
            &["sessions"][..],
            &["cc"],
            &["usage", "status"],
            &["config", "list"],
        ] {
            assert!(!skips_start(&parse(args)), "{args:?}");
        }
    }
}
