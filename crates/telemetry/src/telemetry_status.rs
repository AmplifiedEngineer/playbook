// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! One plain answer to "is telemetry on?", shown by `playbook usage status`
//! and in the usage view's footer. It only reads: the `usage.telemetry`
//! config key, the Claude Code settings files and environment, and the
//! collector state. Totals always come from transcripts, so an `off` here
//! changes what the cost-per-agent and latency views can show, never a total.

use super::collector::{read_state, running, state_path, State};
use super::run::Paths;
use super::telemetry_detect::{configured, Configured};
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Telemetry rows arrived in the last day, whatever the settings say.
    OnReceiving,
    /// Configured to reach the local collector, and it is running.
    OnRunning,
    /// Configured to reach the local collector, which is not running.
    OnCollectorDown,
    /// `usage.telemetry` is `off`.
    OffByConfig,
    /// Your Claude Code settings turn telemetry off.
    OffDisabled,
    /// Your Claude Code settings send telemetry somewhere else.
    OffOwnSetup,
    /// Telemetry is switched on but no `OTEL_EXPORTER_OTLP_ENDPOINT` is set,
    /// so plain `claude` exports nothing.
    OffNoEndpoint,
    /// Nothing sets telemetry for plain `claude`. `cc` still does for its sessions.
    OffNotConfigured,
}

impl Mode {
    /// The status text, without the fix.
    pub fn text(self) -> &'static str {
        match self {
            Mode::OnReceiving => "telemetry: on, data arriving",
            Mode::OnRunning => "telemetry: on, collector running",
            Mode::OnCollectorDown => "telemetry: on, collector not running",
            Mode::OffNoEndpoint => {
                "telemetry: off (OTEL_EXPORTER_OTLP_ENDPOINT is missing, using transcripts)"
            }
            Mode::OffByConfig => "telemetry: off (usage.telemetry is off, using transcripts)",
            Mode::OffDisabled => {
                "telemetry: off (disabled in your Claude Code settings, using transcripts)"
            }
            Mode::OffOwnSetup => {
                "telemetry: off (set up by you for another endpoint, using transcripts)"
            }
            Mode::OffNotConfigured => "telemetry: off (not configured)",
        }
    }

    /// The one-line fix, when there is one.
    pub fn fix(self) -> Option<&'static str> {
        match self {
            Mode::OnReceiving | Mode::OnRunning | Mode::OffOwnSetup => None,
            Mode::OffNoEndpoint => Some(
                "run `playbook setup --telemetry` (older versions: `playbook init --telemetry`) to add the OTEL_* variables to ~/.claude/settings.json, and keep `playbook usage collect` running",
            ),
            Mode::OnCollectorDown => Some("start it with `playbook usage collect`; `cc` starts it for you"),
            Mode::OffByConfig => Some("run `playbook config set usage.telemetry auto`"),
            Mode::OffDisabled => Some(
                "to turn it on, run `playbook setup --telemetry` (older versions: `playbook init --telemetry`), which replaces an off switch that stands alone in ~/.claude/settings.json, or remove CLAUDE_CODE_ENABLE_TELEMETRY from the env block of your Claude Code settings yourself",
            ),
            Mode::OffNotConfigured => Some("run `playbook setup --telemetry` (older versions: `playbook init --telemetry`), or start sessions with `cc`"),
        }
    }

    /// The short form for the footer.
    pub fn footer(self) -> &'static str {
        match self {
            Mode::OnReceiving => "telemetry: on, data arriving",
            Mode::OnRunning => "telemetry: on, collector running",
            Mode::OnCollectorDown => "telemetry: on, collector down",
            Mode::OffNoEndpoint => "telemetry: off (no OTLP endpoint)",
            Mode::OffByConfig | Mode::OffDisabled | Mode::OffOwnSetup => {
                "telemetry: off, using transcripts"
            }
            Mode::OffNotConfigured => "telemetry: off (not configured)",
        }
    }
}

/// The empty state of a telemetry panel in short lines: the cause first
/// (telemetry off, collector down, or no rows yet), then each command on a
/// line of its own, so a short panel still shows the first fix. `None` is a
/// status not read yet.
pub fn empty_panel_lines(mode: Option<Mode>) -> Vec<String> {
    empty_panel_lines_for(mode, super::host::current())
}

/// [`empty_panel_lines`] with the commands of `host`.
pub fn empty_panel_lines_for(mode: Option<Mode>, host: &dyn super::host::Host) -> Vec<String> {
    let setup = host.setup_command("telemetry");
    match mode {
        None | Some(Mode::OffNotConfigured) => vec![
            "No rows: telemetry is off.".into(),
            "Turn it on with:".into(),
            setup,
            "Or start sessions with `playbook cc`.".into(),
        ],
        Some(Mode::OffNoEndpoint) => vec![
            "No rows: telemetry has no OTLP endpoint.".into(),
            "Add OTEL_EXPORTER_OTLP_ENDPOINT with:".into(),
            setup,
        ],
        Some(Mode::OffDisabled) => vec![
            "No rows: Claude Code settings turn it off.".into(),
            "Turn telemetry on with:".into(),
            setup,
        ],
        Some(Mode::OffByConfig) => vec![
            "No rows: usage.telemetry is off.".into(),
            "Turn it on with:".into(),
            "playbook config set usage.telemetry auto".into(),
        ],
        Some(Mode::OffOwnSetup) => vec![
            "No rows: telemetry goes to your own endpoint.".into(),
            "playbook reads transcripts instead.".into(),
        ],
        Some(Mode::OnCollectorDown) => vec![
            "No rows: the collector is not running.".into(),
            "To start it:".into(),
            host.start_hint(),
        ],
        Some(Mode::OnRunning) => vec![
            "No rows in this range yet.".into(),
            "Telemetry is on and the collector runs.".into(),
            "Rows appear while sessions run.".into(),
        ],
        Some(Mode::OnReceiving) => vec![
            "No rows in this range yet.".into(),
            "Telemetry rows arrived in the last day.".into(),
            "Check the collector with:".into(),
            host.command("usage status"),
        ],
    }
}

/// Facts beside the settings that change the answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Signals {
    /// A telemetry request was stored in the last day.
    pub rows_recent: bool,
    /// No `OTEL_EXPORTER_OTLP_ENDPOINT` in the process environment or settings.
    pub endpoint_missing: bool,
}

/// Telemetry is switched on but no OTLP endpoint (generic or per signal) is set.
fn endpoint_is_missing(vars: &std::collections::BTreeMap<String, String>) -> bool {
    let on = vars.contains_key("CLAUDE_CODE_ENABLE_TELEMETRY");
    let endpoint = [
        "OTEL_EXPORTER_OTLP_ENDPOINT",
        "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
        "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
    ]
    .iter()
    .any(|k| vars.contains_key(*k));
    on && !endpoint
}

/// Whether the usage database holds a telemetry request from the last day.
fn rows_recent(paths: &Paths) -> bool {
    let Ok(conn) = playbook_store::Connection::open_with_flags(
        &paths.db,
        playbook_store::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) else {
        return false;
    };
    let _ = conn.busy_timeout(std::time::Duration::from_millis(2000));
    let since = super::util::time::now_secs() - 86_400;
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM telemetry_requests WHERE timestamp >= ?1)",
        [since],
        |r| r.get::<_, bool>(0),
    )
    .unwrap_or(false)
}

/// The decision with its inputs given, so it can be tested.
pub fn decide(
    config_auto: bool,
    kind: Configured,
    collector_running: bool,
    signals: Signals,
) -> Mode {
    if !config_auto {
        return Mode::OffByConfig;
    }
    if signals.rows_recent {
        return Mode::OnReceiving;
    }
    match kind {
        Configured::Disabled => Mode::OffDisabled,
        Configured::Other if signals.endpoint_missing => Mode::OffNoEndpoint,
        Configured::Other => Mode::OffOwnSetup,
        Configured::Nothing => Mode::OffNotConfigured,
        Configured::Playbook if collector_running => Mode::OnRunning,
        Configured::Playbook => Mode::OnCollectorDown,
    }
}

/// Reads the real inputs for `home` and `cwd`.
pub fn detect(home: &Path, cwd: &Path, paths: &Paths) -> Mode {
    let config_auto = matches!(
        super::host::config_value("usage.telemetry", home),
        Some(Value::String(v)) if v == "auto"
    );
    let state: Option<State> = read_state(&state_path(paths));
    let kind = configured(home, cwd, state.as_ref().map(|s| s.port));
    let endpoint_missing = super::telemetry_detect::merged_vars(home, cwd, true)
        .is_some_and(|v| endpoint_is_missing(&v));
    let signals = Signals {
        rows_recent: rows_recent(paths),
        endpoint_missing,
    };
    decide(
        config_auto,
        kind,
        state.as_ref().is_some_and(running),
        signals,
    )
}

/// `detect` for the real home and the current directory.
pub fn detect_here(paths: &Paths) -> Mode {
    let cwd = std::env::current_dir().unwrap_or_default();
    detect(&super::host::home_dir(), &cwd, paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_empty_panel_names_one_cause_and_puts_each_command_on_its_own_line() {
        let off = empty_panel_lines(Some(Mode::OffNotConfigured));
        let down = empty_panel_lines(Some(Mode::OnCollectorDown));
        let quiet = empty_panel_lines(Some(Mode::OnRunning));

        assert!(off[0].contains("telemetry is off"), "{off:?}");
        assert!(
            off.contains(&crate::host::setup_command("telemetry")),
            "{off:?}"
        );
        assert!(down[0].contains("collector is not running"), "{down:?}");
        assert!(down.contains(&crate::daemon::START_HINT.to_string()));
        assert!(
            !down.iter().any(|l| l.contains("collector-service")),
            "{down:?}"
        );
        assert!(quiet[1].contains("collector runs"), "{quiet:?}");
        assert!(
            !quiet.iter().any(|l| l.starts_with("playbook")),
            "{quiet:?}"
        );
        // The cause and the fix fit the four lines of a short panel.
        assert_eq!(down[2], crate::daemon::START_HINT);
        for line in off.iter().chain(&down).chain(&quiet) {
            assert!(line.chars().count() <= 46, "too long: {line}");
        }
    }

    /// Splits `line` into rows of at most `width` characters at spaces, as
    /// the panel wraps it.
    fn wrapped_rows(line: &str, width: usize) -> usize {
        let mut rows = 1;
        let mut used = 0;
        for word in line.split(' ') {
            let len = word.chars().count();
            if used == 0 {
                used = len;
            } else if used + 1 + len <= width {
                used += 1 + len;
            } else {
                rows += 1;
                used = len;
            }
        }
        rows
    }

    #[test]
    fn under_pbtop_the_cause_and_first_command_fit_a_four_row_panel() {
        let host = crate::standalone::Standalone;
        let modes = [
            Mode::OffNotConfigured,
            Mode::OffNoEndpoint,
            Mode::OffDisabled,
            Mode::OffByConfig,
            Mode::OffOwnSetup,
            Mode::OnCollectorDown,
            Mode::OnRunning,
            Mode::OnReceiving,
        ];

        for mode in modes {
            let lines = empty_panel_lines_for(Some(mode), &host);
            let first_command = lines
                .iter()
                .position(|l| l.starts_with("playbook ") || l.starts_with("pbtop"))
                .map_or(lines.len(), |i| i + 1);
            let rows: usize = lines[..first_command]
                .iter()
                .map(|l| wrapped_rows(l, 46))
                .sum();

            assert!(rows <= 4, "{mode:?} needs {rows} rows: {lines:?}");
        }
    }

    #[test]
    fn data_arriving_does_not_claim_the_collector_runs() {
        let lines = empty_panel_lines(Some(Mode::OnReceiving));

        assert!(lines[1].contains("arrived in the last day"), "{lines:?}");
        assert!(!lines.iter().any(|l| l.contains("collector runs")));
        assert_eq!(lines[3], crate::host::command("usage status"));
    }

    #[test]
    fn the_config_key_off_wins_over_everything() {
        for kind in [
            Configured::Nothing,
            Configured::Playbook,
            Configured::Disabled,
        ] {
            assert_eq!(
                decide(false, kind, true, Signals::default()),
                Mode::OffByConfig
            );
        }
    }

    #[test]
    fn user_settings_that_disable_or_redirect_are_reported_and_never_overridden() {
        assert_eq!(
            decide(true, Configured::Disabled, true, Signals::default()),
            Mode::OffDisabled
        );
        assert_eq!(
            decide(true, Configured::Other, true, Signals::default()),
            Mode::OffOwnSetup
        );
    }

    #[test]
    fn nothing_configured_says_off_not_configured_with_the_fix() {
        let mode = decide(true, Configured::Nothing, false, Signals::default());
        assert_eq!(mode.text(), "telemetry: off (not configured)");
        assert!(mode.fix().unwrap().contains("playbook setup --telemetry"));
    }

    #[test]
    fn the_collector_state_separates_the_two_on_modes() {
        assert_eq!(
            decide(true, Configured::Playbook, true, Signals::default()),
            Mode::OnRunning
        );
        assert_eq!(
            decide(true, Configured::Playbook, false, Signals::default()),
            Mode::OnCollectorDown
        );
        assert_eq!(Mode::OnRunning.text(), "telemetry: on, collector running");
    }

    #[test]
    fn the_disabled_text_names_your_settings_and_transcripts() {
        assert_eq!(
            Mode::OffDisabled.text(),
            "telemetry: off (disabled in your Claude Code settings, using transcripts)"
        );
        assert!(Mode::OffDisabled.fix().is_some());
    }

    #[test]
    fn recent_telemetry_rows_mean_on_even_when_the_settings_say_off() {
        let signals = Signals {
            rows_recent: true,
            endpoint_missing: true,
        };

        let mode = decide(true, Configured::Disabled, false, signals);

        assert_eq!(mode, Mode::OnReceiving);
        assert!(mode.fix().is_none());
    }

    #[test]
    fn rows_do_not_override_the_off_config_key() {
        let signals = Signals {
            rows_recent: true,
            endpoint_missing: false,
        };

        assert_eq!(
            decide(false, Configured::Playbook, true, signals),
            Mode::OffByConfig
        );
    }

    #[test]
    fn telemetry_on_without_an_endpoint_names_the_missing_variable_and_the_fix() {
        let signals = Signals {
            rows_recent: false,
            endpoint_missing: true,
        };

        let mode = decide(true, Configured::Other, false, signals);

        assert_eq!(mode, Mode::OffNoEndpoint);
        assert!(mode.text().contains("OTEL_EXPORTER_OTLP_ENDPOINT"));
        assert!(mode.fix().unwrap().contains("playbook setup --telemetry"));
    }

    #[test]
    fn a_missing_database_reports_no_recent_rows() {
        let paths = Paths::from_home(Path::new("/nonexistent-home-for-test"));

        assert!(!rows_recent(&paths));
    }

    fn vars(keys: &[&str]) -> std::collections::BTreeMap<String, String> {
        keys.iter()
            .map(|k| (k.to_string(), "1".to_string()))
            .collect()
    }

    #[test]
    fn the_endpoint_is_missing_only_when_telemetry_is_on_and_no_endpoint_is_set() {
        let on = vars(&["CLAUDE_CODE_ENABLE_TELEMETRY"]);
        let per_signal = vars(&[
            "CLAUDE_CODE_ENABLE_TELEMETRY",
            "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
        ]);
        let unrelated = vars(&["OTEL_SERVICE_NAME"]);

        assert!(endpoint_is_missing(&on));
        assert!(!endpoint_is_missing(&per_signal));
        assert!(!endpoint_is_missing(&unrelated));
    }

    #[test]
    fn rows_from_the_last_day_count_and_older_rows_do_not() {
        let dir = std::env::temp_dir().join(format!("pbtop-rows-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths::from_home(&dir);
        std::fs::create_dir_all(paths.db.parent().unwrap()).unwrap();
        let conn = crate::db::open_db(&paths.db).unwrap();
        let now = crate::util::time::now_secs();
        let insert = |id: &str, ts: i64| {
            conn.execute(
                "INSERT INTO telemetry_requests (session_id, request_id, timestamp, model, input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens, cost_usd) VALUES ('s', ?1, ?2, 'm', 0, 0, 0, 0, 0)",
                (id, ts),
            )
            .unwrap();
        };
        insert("old", now - 2 * 86_400);
        let before = rows_recent(&paths);
        insert("new", now - 3_600);
        let after = rows_recent(&paths);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(!before);
        assert!(after);
    }
}
