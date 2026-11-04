// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook doctor check`: the seven layer checks and the three
//! informational ones that `commands/doctor.md` used to run as bash blocks.
//! Every probe reads from an `Env`, so tests point it at a scratch home.

use crate::config;
use crate::doctor::field;
use crate::update::{pathcheck, resolve};
use serde_json::{json, Value};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// How a row reads in the status table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Pass,
    Info,
    Warn,
    Fail,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::Pass => "PASS",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Fail => "FAIL",
        }
    }
}

/// One line of the status table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// `1` to `7` for a layer, `-` for an informational row.
    pub layer: &'static str,
    pub level: Level,
    pub message: String,
    pub hint: Option<String>,
}

impl Row {
    pub(super) fn new(layer: &'static str, level: Level, message: impl Into<String>) -> Row {
        Row {
            layer,
            level,
            message: message.into(),
            hint: None,
        }
    }

    pub(super) fn hint(mut self, hint: impl Into<String>) -> Row {
        self.hint = Some(hint.into());
        self
    }
}

/// Everything a probe reads from the machine.
pub struct Env {
    pub home: PathBuf,
    pub claude_home: PathBuf,
    pub plugin_root: Option<PathBuf>,
    pub shell: String,
    pub path_var: OsString,
    pub repo_root: Option<PathBuf>,
    pub repo_slug: Option<String>,
}

impl Env {
    fn settings(&self) -> PathBuf {
        self.claude_home.join("settings.json")
    }
}

const PRE_GUARDS: [&str; 5] = [
    "rm-workspace-guard",
    "bg-await-guard",
    "precommit-check",
    "commit-message-sanitizer",
    "policy-guard",
];

/// The row for the document converter, from the facade's verdict.
#[cfg(unix)]
fn doc_converter_row(health: &crate::doc::converter::Health) -> Row {
    use crate::doc::converter::Health;
    match health {
        Health::Ready => Row::new("-", Level::Pass, health.message()),
        Health::VersionSkew => {
            Row::new("-", Level::Warn, health.message()).hint("run: playbook update")
        }
        Health::External => Row::new("-", Level::Warn, health.message()),
        Health::Missing => Row::new("-", Level::Info, health.message()),
    }
}

#[cfg(unix)]
fn doc_rows(env: &Env) -> Vec<Row> {
    use crate::doc::converter::{configured_path, Search};
    let configured = configured_path(&env.home, env.repo_slug.as_deref());
    let search = Search::process(&configured).with_path(env.path_var.as_os_str());
    vec![doc_converter_row(&search.health())]
}

/// Document conversion needs process groups and signals, so it is not offered
/// here. The row says so instead of leaving the table silent.
#[cfg(not(unix))]
fn doc_rows(_env: &Env) -> Vec<Row> {
    vec![Row::new("-", Level::Info, DOCTOR_ROW_UNSUPPORTED)]
}

#[cfg(not(unix))]
const DOCTOR_ROW_UNSUPPORTED: &str = "doc converter: not available on this platform";

/// The guards that must also be wired on PostToolUse.
const POST_GUARDS: [&str; 1] = ["commit-message-sanitizer"];

const INSTALL_HINT: &str = "run: curl -fsSL https://raw.githubusercontent.com/pragmatic-engineer/playbook/main/install.sh | bash, or brew install pragmatic-engineer/tap/playbook, and make sure its directory is on PATH";

/// One probe: reads the machine and returns its rows.
type Probe = fn(&Env) -> Vec<Row>;

/// Every probe, in table order.
const PROBES: [Probe; 22] = [
    |env| vec![layer1(env)],
    |env| vec![layer2(env)],
    |env| vec![layer3(env)],
    |env| vec![layer4(env)],
    |env| layer5(env).into_iter().collect(),
    layer6,
    layer7,
    config_rows,
    effort_rows,
    worktree_rows,
    migration_rows,
    data_rows,
    hooks_rows,
    atlassian_rows,
    old_launcher_rows,
    |env| {
        leftover_service_row(&env.home)
            .into_iter()
            .chain(telemetry_rows(env))
            .collect()
    },
    |env| vec![attribution_row(env)],
    update_rows,
    |env| vec![policy_guard_row(env)],
    doc_rows,
    completion_rows,
    super::build::rows,
];

/// Run every check. The probes only read, and several wait on child processes
/// (`claude plugin list`, `git`, the binary's own `--version`), so they run
/// side by side. Rows are joined in table order, so the output is unchanged.
pub fn run(env: &Env) -> Vec<Row> {
    let rows = crate::common::par::map(&PROBES, crate::common::par::MAX_CONCURRENT, |probe| {
        probe(env)
    })
    .into_iter()
    .flatten()
    .collect();
    if is_paused(env) {
        mark_paused(rows)
    } else {
        rows
    }
}

/// Whether the plugin is switched off in the settings file, which makes every
/// hook entry and the status line do nothing. `PLAYBOOK_IGNORE_PLUGIN_DISABLED=1`
/// runs them anyway, so then nothing is paused.
fn is_paused(env: &Env) -> bool {
    !std::env::var(crate::common::plugin_state::IGNORE_ENV).is_ok_and(|v| v == "1")
        && crate::common::plugin_state::disabled_in_file(&env.settings())
}

/// Rows that claim hooks or the status line work cannot say PASS while the
/// plugin is paused. The plugin row says why.
fn mark_paused(mut rows: Vec<Row>) -> Vec<Row> {
    let hint = "run: claude plugin enable playbook@pragmatic-engineer";
    let message =
        "plugin paused (enabledPlugins sets it to false), so hooks and the status line do nothing";
    match rows.iter().position(|r| r.layer == "1") {
        Some(at) if rows[at].level == Level::Fail => {}
        Some(at) if rows[at].level == Level::Pass => {
            rows[at] = Row::new("1", Level::Warn, message).hint(hint);
        }
        // An INFO or WARN row states another fact (not installed, claude
        // missing); keep it and add the paused row in front.
        Some(at) => rows.insert(at, Row::new("1", Level::Warn, message).hint(hint)),
        None => rows.insert(0, Row::new("1", Level::Warn, message).hint(hint)),
    }
    for row in rows.iter_mut().filter(|r| {
        matches!(r.layer, "2" | "5")
            || r.message
                .starts_with("playbook hook entries in settings.json")
    }) {
        if row.level == Level::Pass {
            row.level = Level::Warn;
            let tail = if row.layer == "5" {
                "so it does not run"
            } else {
                "so they do not run"
            };
            row.message = format!("{}, but the plugin is paused {tail}", row.message);
            row.hint = Some(hint.to_string());
        }
    }
    rows
}

fn on_path(env: &Env, name: &str) -> Option<PathBuf> {
    std::env::split_paths(&env.path_var)
        .filter(|d| d.is_absolute())
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// Longest one doctor probe may run.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

fn output_of(mut cmd: Command) -> Option<String> {
    // A hung `claude` or `playbook` must not hang the doctor: a probe that
    // does not answer in time reads as one that could not run.
    let out = crate::common::proc::run_with_timeout(&mut cmd, PROBE_TIMEOUT)?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn layer1(env: &Env) -> Row {
    let miss = "run: claude plugin marketplace add pragmatic-engineer/marketplace && claude plugin install playbook@pragmatic-engineer";
    let Some(claude) = on_path(env, "claude") else {
        return Row::new(
            "1",
            Level::Info,
            "claude CLI not found, plugin check skipped",
        );
    };
    let mut cmd = Command::new(claude);
    cmd.args(["plugin", "list"]);
    let Some(text) = output_of(cmd) else {
        return Row::new("1", Level::Info, "could not run `claude plugin list`");
    };
    let lower = text.to_lowercase();
    // Match the plugin's own id.
    let Some(at) = lower.find("playbook@") else {
        // The skills plugin is optional: a binary-only install is a valid state.
        return Row::new("1", Level::Info, "skills plugin not installed (optional)").hint(miss);
    };
    let block_end = lower[at..].find("\n\n").map_or(lower.len(), |n| at + n);
    if lower[at..block_end].contains("disabled") {
        return Row::new("1", Level::Fail, "plugin installed but disabled")
            .hint("run: claude plugin enable playbook@pragmatic-engineer");
    }
    Row::new("1", Level::Pass, "plugin enabled")
}

fn layer2(env: &Env) -> Row {
    let settings = env.settings();
    let mut total = 0;
    let mut wired = 0;
    let mut problems = String::new();
    for (event, guards, suffix) in [
        ("PreToolUse", &PRE_GUARDS[..], ""),
        ("PostToolUse", &POST_GUARDS[..], "(PostToolUse)"),
    ] {
        for (guard, count) in field::hook_commands_for_event(&settings, event, guards) {
            total += 1;
            if count > 0 {
                wired += 1;
            } else {
                problems.push_str(&format!(" {guard}{suffix}:NOT_WIRED"));
            }
        }
    }
    if problems.is_empty() {
        Row::new(
            "2",
            Level::Pass,
            format!("safety guards wired ({wired} of {total})"),
        )
    } else {
        Row::new(
            "2",
            Level::Fail,
            format!("safety guards wired {wired}/{total}, not wired:{problems}"),
        )
        .hint("run `playbook setup`, which rewrites every guard to its bare form")
    }
}

/// Reports the shell-launcher lines that releases up to 0.36.0 wrote into the
/// rc files (ADR 0044). It stays because those lines are still on disk for any
/// user who upgrades from such a release, and each one now prints an error or
/// does nothing in every new shell. Remove it in 1.0.0, together with
/// migration `retired-launcher-rc-line`.
fn layer3(env: &Env) -> Row {
    let (writable, stuck): (Vec<_>, Vec<_>) = crate::init::rcfile::retired_rc_entries(&env.home)
        .into_iter()
        .partition(|r| r.writable);
    let names = |list: &[crate::init::rcfile::RetiredRc]| {
        list.iter()
            .map(|r| r.rc_file.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let hint = match (writable.is_empty(), stuck.is_empty()) {
        (true, true) => {
            let row = Row::new("3", Level::Pass, "no shell line needed for `playbook cc`");
            return if pb_runs_this_binary(env) {
                row
            } else {
                row.hint("no `pb` on PATH runs playbook; run: playbook setup --only short-name")
            };
        }
        (false, true) => "run `playbook setup` to remove it after a backup".to_string(),
        (true, false) => {
            "the file is read-only; delete the playbook launcher lines by hand".to_string()
        }
        (false, false) => format!(
            "run `playbook setup` for {}; delete the lines by hand in {}, which is read-only",
            names(&writable),
            names(&stuck)
        ),
    };
    let all: Vec<_> = writable.into_iter().chain(stuck).collect();
    Row::new(
        "3",
        Level::Warn,
        format!("retired launcher line in {}", names(&all)),
    )
    .hint(hint)
}

/// Whether a `pb` on PATH resolves to the running binary, as the short-name
/// part checks it.
fn pb_runs_this_binary(env: &Env) -> bool {
    match (on_path(env, "pb"), std::env::current_exe()) {
        (Some(pb), Ok(exe)) => crate::init::pb_link::same_file(&pb, &exe),
        _ => false,
    }
}

fn layer4(env: &Env) -> Row {
    let file = crate::init::system_prompt::path(&env.home);
    if !crate::init::system_prompt::enabled(&env.home) {
        Row::new(
            "4",
            Level::Info,
            "`pb cc` starts with Claude Code's own prompt (systemPrompt.enabled is false)",
        )
        .hint("to load the playbook prompt, run: playbook setup --only system-prompt")
    } else if file.is_file() && std::fs::File::open(&file).is_err() {
        Row::new("4", Level::Warn, "system prompt file is not readable")
            .hint("fix the file permissions on ~/.config/playbook/prompts/SYSTEM_PROMPT.md")
    } else if file.is_file() {
        Row::new("4", Level::Pass, "`pb cc` loads the playbook system prompt")
    } else {
        Row::new(
            "4",
            Level::Info,
            "system prompt not installed, so `pb cc` starts with Claude Code's own",
        )
        .hint("run: playbook setup --only system-prompt")
    }
}

fn expand(path: &str, home: &Path) -> String {
    let home = home.to_string_lossy();
    let path = match path.strip_prefix('~') {
        Some(rest) => format!("{home}{rest}"),
        None => path.to_string(),
    };
    path.replace("$HOME", &home)
}

fn layer5(env: &Env) -> Option<Row> {
    let cmd = field::statusline_command(&env.settings());
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return Some(Row::new(
            "5",
            Level::Info,
            "no status line configured (opt-in)",
        ));
    }
    if cmd == "playbook statusline" {
        return Some(Row::new(
            "5",
            Level::Pass,
            "status line runs `playbook statusline`",
        ));
    }
    if [
        "bash $HOME/.config/playbook/statusline.sh",
        "bash ~/.config/playbook/statusline.sh",
    ]
    .contains(&cmd)
    {
        return Some(
            Row::new(
                "5",
                Level::Fail,
                format!("status line runs the retired statusline.sh: {cmd}"),
            )
            .hint("run `playbook setup`, which rewrites it to `playbook statusline`"),
        );
    }
    let last = cmd.split_whitespace().next_back().unwrap_or("");
    let path = expand(last, &env.home);
    if !Path::new(&path).is_file() {
        return Some(
            Row::new(
                "5",
                Level::Fail,
                format!("status line file is missing: {path}"),
            )
            .hint("point statusLine.command at `playbook statusline`, or restore your script"),
        );
    }
    Some(Row::new(
        "5",
        Level::Info,
        format!("status line runs your own command: {cmd}"),
    ))
}

fn version_of(bin: &Path) -> String {
    let mut cmd = Command::new(bin);
    cmd.arg("--version");
    output_of(cmd)
        .and_then(|t| t.split_whitespace().next_back().map(str::to_string))
        .unwrap_or_default()
}

/// The plugin the resolver picks, the same one `playbook path plugin-root`
/// and the session hook use, `PLAYBOOK_PLUGIN_DIR` included.
fn plugin_dir(env: &Env) -> Option<PathBuf> {
    let override_env = std::env::var(crate::plugin_root::OVERRIDE_ENV).ok();
    let cwd = std::env::current_dir().unwrap_or_default();
    crate::plugin_root::resolve(&crate::plugin_root::Inputs {
        claude_home: &env.claude_home,
        override_env: override_env.as_deref(),
        env: env.plugin_root.as_ref().and_then(|p| p.to_str()),
        cwd: &cwd,
    })
    .ok()
    .map(|r| r.root)
}

/// The binary against the plugin (ADR 0038 decision 7): a pass while the
/// plugin is in the minor line of the binary on PATH and that binary is not
/// older than the plugin's `minBinaryVersion`. Different patch versions are
/// normal.
fn version_row(env: &Env, bin: &Path, bin_ver: &str) -> Row {
    use crate::plugin_root::{binary_too_old, plugin_out_of_range, plugin_version};
    let plugin = plugin_dir(env);
    let Some(version) = plugin.as_deref().and_then(plugin_version) else {
        return Row::new(
            "6",
            Level::Info,
            format!("playbook binary on PATH, version {bin_ver}, no plugin manifest to compare"),
        );
    };
    // A Homebrew binary is often reached through a symlink in /usr/local/bin.
    let homebrew =
        crate::update::is_homebrew(&fs::canonicalize(bin).unwrap_or_else(|_| bin.to_path_buf()));
    plugin
        .as_deref()
        .and_then(|root| binary_too_old(root, bin_ver, homebrew))
        .or_else(|| plugin_out_of_range(&version, bin_ver, homebrew))
        .map_or_else(
            || {
                Row::new(
                    "6",
                    Level::Pass,
                    if bin_ver == env!("CARGO_PKG_VERSION") {
                        format!("playbook binary {bin_ver} works with plugin {version}")
                    } else {
                        format!(
                            "playbook binary {bin_ver} works with plugin {version} (range checked by playbook {}, which runs this check)",
                            env!("CARGO_PKG_VERSION")
                        )
                    },
                )
            },
            |line| Row::new("6", Level::Warn, line),
        )
}

/// The version record (ADR 0034): whether the last heal ran clean and what
/// still needs the user. The hooks run `bin_ver`, the first `playbook` on
/// PATH, so the record is compared with that one. Reads through a read-only
/// connection, so the check changes nothing on disk.
fn heal_row(env: &Env, bin_ver: &str) -> Row {
    let root = crate::common::paths::playbook_root_from(&env.home);
    let fix = "run: playbook migrate";
    let record = match crate::heal::Record::read_only(&root) {
        Ok(record) => record,
        Err(err) => {
            return Row::new(
                "6",
                Level::Warn,
                format!("the upgrade record could not be read: {err}"),
            )
            .hint(fix)
        }
    };
    if record.failed_at.is_some() {
        return Row::new(
            "6",
            Level::Warn,
            "the last upgrade heal failed, so playbook's own files may be out of date",
        )
        .hint(fix);
    }
    // The list `playbook migrate` prints, under this one row. The plugin
    // items are left to the version row above, which checks the binary on
    // PATH.
    let items = crate::pending::migration_items_read_only(&env.home).unwrap_or_default();
    if !items.is_empty() {
        return Row::new(
            "6",
            Level::Warn,
            format!(
                "upgrade check: {}:\n{}",
                crate::pending::count(items.len()),
                crate::pending::bullets(&items).trim_end()
            ),
        )
        .hint(format!("run: {}", crate::pending::closing_command(&items)));
    }
    let Some(b) = record.binary else {
        return Row::new(
            "6",
            Level::Info,
            "no upgrade record yet; the next playbook command or session start writes it",
        )
        .hint(fix);
    };
    match resolve::compare(&b, bin_ver) {
        std::cmp::Ordering::Equal => Row::new(
            "6",
            Level::Pass,
            format!("upgrade record: playbook {b}"),
        ),
        std::cmp::Ordering::Greater => Row::new(
            "6",
            Level::Info,
            format!("the upgrade record names playbook {b}, newer than {bin_ver}; nothing runs after a downgrade"),
        ),
        std::cmp::Ordering::Less => Row::new(
            "6",
            Level::Info,
            format!("the upgrade record names playbook {b}; the next playbook command or session start updates it"),
        )
        .hint(fix),
    }
}

fn layer6(env: &Env) -> Vec<Row> {
    let entries = pathcheck::scan(&env.path_var);
    let Some(first) = entries.first() else {
        return vec![Row::new("6", Level::Fail, "playbook binary not on PATH")
            .hint(format!("every ported hook is dead. {INSTALL_HINT}"))];
    };
    let bin_ver = version_of(first);
    let mut rows = Vec::new();
    if bin_ver.is_empty() {
        rows.push(
            Row::new(
                "6",
                Level::Fail,
                format!(
                    "{} printed no version, so it is not the binary this plugin expects",
                    first.display()
                ),
            )
            .hint(
                "a stale shim or a name collision with another tool: check `command -v playbook`",
            ),
        );
    } else {
        rows.push(version_row(env, first, &bin_ver));
        rows.push(heal_row(env, &bin_ver));
    }
    let mut gate = Command::new(first);
    gate.args(["gate", "record", "--help"]);
    if !output_of(gate).is_some_and(|t| t.contains("--source")) {
        rows.push(
            Row::new("6", Level::Fail, "gate record has no --source flag, so every quality gate call fails")
                .hint("run `playbook update`, or re-run install.sh if the binary is too old to have `update`"),
        );
    }
    if entries.len() > 1 {
        let versions: Vec<String> = entries.iter().map(|e| version_of(e)).collect();
        let list: Vec<String> = entries
            .iter()
            .zip(&versions)
            .map(|(e, v)| format!("{} {}", e.display(), if v.is_empty() { "?" } else { v }))
            .collect();
        let first_v = &versions[0];
        let stale = !first_v.is_empty()
            && versions[1..]
                .iter()
                .any(|v| !v.is_empty() && resolve::compare(first_v, v) == std::cmp::Ordering::Less);
        rows.push(if stale {
            Row::new("6", Level::Warn, format!("the first playbook on PATH is older than a later one: {}", list.join(", ")))
                .hint("remove the stale one (Homebrew: `brew uninstall playbook`) or put the newer directory first on PATH")
        } else {
            Row::new("6", Level::Info, format!("several playbook binaries on PATH: {}", list.join(", ")))
        });
    }
    rows
}

/// The Atlassian command line tools the `playbook:atlassian` skill can use.
/// Absent tools are informational: the skill falls back to the next route.
fn atlassian_rows(env: &Env) -> Vec<Row> {
    use crate::atlassian::{acli_line, acli_status, twg_line, twg_status, Status, UPGRADE_HINT};
    let acli = acli_status(&env.path_var);
    let acli_row = match &acli {
        Status::Ok(_) => Row::new("-", Level::Pass, acli_line(&acli)),
        Status::Missing => Row::new("-", Level::Info, acli_line(&acli)),
        Status::TooOld(_) | Status::Unreadable => {
            Row::new("-", Level::Warn, acli_line(&acli)).hint(format!("run: {UPGRADE_HINT}"))
        }
    };
    let twg = twg_status(&env.path_var);
    vec![acli_row, Row::new("-", Level::Info, twg_line(&twg))]
}

/// Reports the old `ccc` and `ccd` launchers, which `playbook cc` and
/// `playbook ccd` replace. Informational: it never changes a file.
fn old_launcher_rows(env: &Env) -> Vec<Row> {
    super::launchers::find(&env.home, &env.path_var)
        .iter()
        .map(|f| {
            Row::new("-", Level::Info, super::launchers::describe(f))
                .hint("`pb cc` and `pb ccd` work too when `pb` is installed")
        })
        .collect()
}

/// Reports whether tab completion is wired for `$SHELL`. It only reads files:
/// the stub or loader asks the binary for the script, so it is never stale.
fn completion_rows(env: &Env) -> Vec<Row> {
    use crate::init::completion::{self, Status};
    use crate::init::path::PathShell;
    let shell = PathShell::detect(&env.shell);
    if shell == PathShell::Other {
        return Vec::new();
    }
    let row = match completion::status(&env.home, shell) {
        Status::Ok => Row::new("-", Level::Pass, "tab completion is set up"),
        Status::Missing => {
            Row::new("-", Level::Info, "tab completion is not set up").hint(COMPLETION_SETUP_HINT)
        }
        Status::StubNotOurs => Row::new(
            "-",
            Level::Warn,
            "a completion file is not written by playbook, so setup leaves it alone",
        )
        .hint(COMPLETION_REMOVE_HINT),
        Status::LineEdited => Row::new(
            "-",
            Level::Warn,
            "the completion line in ~/.zshrc was edited by hand",
        )
        .hint(COMPLETION_REMOVE_HINT),
        Status::LineDuplicated => Row::new(
            "-",
            Level::Warn,
            "the completion line in ~/.zshrc is duplicated",
        )
        .hint(COMPLETION_REMOVE_HINT),
    };
    vec![row]
}

const COMPLETION_SETUP_HINT: &str = "run playbook setup";
const COMPLETION_REMOVE_HINT: &str = "remove the file or line by hand, then run playbook setup";

fn layer7(env: &Env) -> Vec<Row> {
    let settings = env.settings();
    if !settings.is_file() {
        return vec![Row::new(
            "7",
            Level::Info,
            "no ~/.claude/settings.json, hook paths not checked",
        )];
    }
    let mut checked = 0;
    let mut dangling: Vec<String> = Vec::new();
    for cmd in field::hook_commands(&settings) {
        if cmd.starts_with(crate::init::wire::HOOK_COMMAND_PREFIX) {
            continue;
        }
        let last = cmd.split_whitespace().next_back().unwrap_or("");
        if !last.contains('/') {
            continue;
        }
        let path = expand(last, &env.home);
        if path.contains('$') {
            continue;
        }
        checked += 1;
        if !Path::new(&path).exists() {
            dangling.push(cmd);
        }
    }
    dangling.sort();
    dangling.dedup();
    if dangling.is_empty() {
        return vec![Row::new(
            "7",
            Level::Pass,
            format!("no hook command points at a missing file ({checked} checked)"),
        )];
    }
    dangling
        .into_iter()
        .map(|cmd| {
            let hint = if crate::init::dead_hooks::is_dead(&cmd, &env.home, &env.claude_home) {
                "this hook does nothing every time it fires; run: playbook setup --hooks (it removes the entry after a backup)"
            } else {
                "this hook does nothing every time it fires; `playbook setup` will not remove it, so delete the entry from ~/.claude/settings.json by hand"
            };
            Row::new("7", Level::Fail, format!("hook command points at a missing file: {cmd}")).hint(hint)
        })
        .collect()
}

fn effort_rows(env: &Env) -> Vec<Row> {
    let cwd = env.repo_root.clone().unwrap_or_else(|| env.home.clone());
    let c = crate::effort::ceiling(&env.home, &env.claude_home, &cwd);
    vec![
        Row::new(
            "-",
            Level::Info,
            format!(
                "effort ceiling: playbook {}, Claude Code {} (read only, never written), effective {}",
                c.playbook,
                c.claude.as_deref().unwrap_or("none"),
                c.effective.as_deref().unwrap_or("none"),
            ),
        ),
        Row::new("-", Level::Info, format!("effort ceiling limit: {}", crate::effort::LIMIT)),
    ]
}

fn config_rows(env: &Env) -> Vec<Row> {
    let slug = env.repo_slug.as_deref();
    [
        "autoReview.enabled",
        "autoReview.type",
        "autoReview.fix",
        "autoMerge.enabled",
        "commit.signOff",
        "pr.draft",
    ]
    .iter()
    .map(|key| match config::resolve_valid(key, &env.home, slug) {
        Ok((value, source, ignored)) => {
            let mut origin = source.label().to_string();
            if let Some(ig) = ignored {
                origin = format!("{origin}, {} value ignored", ig.tier.label());
            }
            Row::new(
                "-",
                Level::Info,
                format!(
                    "{key}: {} (source: {origin})",
                    crate::common::mode::value_text(&value)
                ),
            )
        }
        Err(err) => Row::new("-", Level::Info, format!("{key}: could not check: {err}")),
    })
    .collect()
}

fn worktree_rows(env: &Env) -> Vec<Row> {
    let Some(root) = &env.repo_root else {
        return vec![Row::new(
            "-",
            Level::Info,
            "not in a git repo, worktrees not checked",
        )];
    };
    let now = crate::common::time::now_secs();
    match crate::worktree::sweep(root, &env.home, env.repo_slug.as_deref(), true, now) {
        Ok(lines) if lines.is_empty() => {
            vec![Row::new("-", Level::Info, "no stale worktrees found")]
        }
        Ok(lines) => lines
            .into_iter()
            .map(|l| Row::new("-", Level::Info, l))
            .collect(),
        Err(err) => vec![Row::new(
            "-",
            Level::Info,
            format!("worktree check failed: {err}"),
        )],
    }
}

/// The endpoint `playbook setup` recorded in its marker file, if any.
fn marker_endpoint(home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(
        crate::common::paths::playbook_root_from(home).join("telemetry-env.json"),
    )
    .ok()?;
    let doc: serde_json::Value = serde_json::from_str(&text).ok()?;
    doc.get("OTEL_EXPORTER_OTLP_ENDPOINT")?
        .as_str()
        .map(|e| e.trim_end_matches('/').to_string())
}

const TELEMETRY_SETUP_FIX: &str = "run: playbook setup --only telemetry";

/// The policy guard is optional and off by default, so off is an Info row.
fn policy_guard_row(env: &Env) -> Row {
    use crate::hooks::policy_guard::{enabled_for, env_override, ENABLED_KEY};
    if let Some(on) = env_override() {
        let (level, state) = if on {
            (Level::Pass, "on")
        } else {
            (Level::Info, "off")
        };
        return Row::new(
            "-",
            level,
            format!("policy guard is {state}: POLICY_GUARD in the environment decides, whatever {ENABLED_KEY} holds"),
        )
        .hint("unset POLICY_GUARD to follow the stored key");
    }
    if enabled_for(&env.home) {
        Row::new(
            "-",
            Level::Pass,
            "policy guard is on: it denies skipped git hooks, a hand-run gh pr create, Claude Code memory writes, settings.json edits and protected config changes",
        )
        .hint(format!(
            "to turn it off, run: playbook config set --global {ENABLED_KEY} false"
        ))
    } else {
        Row::new(
            "-",
            Level::Info,
            "policy guard is off (optional, off by default)",
        )
        .hint(format!(
            "to turn it on, run: playbook config set --global {ENABLED_KEY} true"
        ))
    }
}

/// The `attribution` key in the user's settings.json (ADR 0042). With
/// attribution OFF, the default: ok when every key holds the off value, a
/// warning with the fix when a key is missing. With ON chosen
/// (`attribution.enabled` true): ok when no key holds the off value. A key
/// with the user's own value is info.
fn attribution_row(env: &Env) -> Row {
    use crate::init::attribution::{self, State};
    let settings = env.settings();
    match attribution::state(&settings) {
        State::Unreadable(why) => Row::new("-", Level::Warn, format!("attribution: {why}")),
        State::OtherShape => Row::new("-", Level::Info, attribution::OTHER_SHAPE),
        state if attribution::enabled(&env.home) => {
            attribution_on_row(&state, &attribution::held_off(&settings))
        }
        State::Open { missing, custom } if !missing.is_empty() => Row::new(
            "-",
            Level::Warn,
            format!(
                "attribution OFF is the default, and {} is missing in settings.json, so Claude Code adds its attribution",
                attribution::names(&missing)
            ),
        )
        .hint(format!(
            "run: {} (or playbook doctor --fix){}; for attribution ON, run: {}",
            attribution::COMMAND,
            if custom.is_empty() { "" } else { "; your own values stay" },
            attribution::ON_COMMAND
        )),
        State::Open { custom, .. } => Row::new(
            "-",
            Level::Info,
            format!(
                "attribution has your own value for {}",
                attribution::names(&custom)
            ),
        )
        .hint(format!("for attribution OFF, run: {}", attribution::OFF_COMMAND)),
        State::Off => Row::new(
            "-",
            Level::Pass,
            "attribution OFF: commits and PRs get no Claude trailer, generated-by line or session link",
        ),
    }
}

/// The attribution row when the user chose ON. `held` names the keys that
/// still hold the off value.
fn attribution_on_row(state: &crate::init::attribution::State, held: &[&str]) -> Row {
    use crate::init::attribution::{self, State};
    if !held.is_empty() {
        return Row::new(
            "-",
            Level::Warn,
            format!(
                "attribution ON is your choice ({} is true), and {} still holds the off value",
                attribution::KEY,
                attribution::names(held)
            ),
        )
        .hint(format!("run: {}", attribution::COMMAND));
    }
    match state {
        State::Off => Row::new(
            "-",
            Level::Info,
            "attribution is false in settings.json, your own value, so Claude Code adds no attribution",
        ),
        State::Open { custom, .. } if !custom.is_empty() => Row::new(
            "-",
            Level::Info,
            format!(
                "attribution ON is your choice; your own value for {}",
                attribution::names(custom)
            ),
        ),
        State::Open { .. } => Row::new(
            "-",
            Level::Pass,
            "attribution ON, your choice: Claude Code adds its trailer, generated-by line and session link",
        ),
        State::OtherShape => Row::new("-", Level::Info, attribution::OTHER_SHAPE),
        State::Unreadable(why) => Row::new("-", Level::Warn, format!("attribution: {why}")),
    }
}

/// What doctor says while a file of the retired collector service remains.
const LEFTOVER_SERVICE: &str = "a collector service file is still installed";

/// A service file of the retired collector service (ADR 0043) under either
/// label, or the systemd unit. Shown whatever telemetry says, since the
/// service manager loads it at the next login; `playbook migrate` removes
/// playbook's and names the commands for a file playbook did not write.
/// Such a file is named the same way, with that said.
fn leftover_service_row(home: &Path) -> Option<Row> {
    let ours = crate::init::collector_service::installed_names(home);
    let foreign = crate::init::collector_service::foreign_files(home);
    let names: Vec<String> = ours
        .iter()
        .cloned()
        .chain(foreign.iter().map(|f| f.sentence()))
        .collect();
    // Migrate removes only playbook's files; for a file it did not write, the
    // hint is the commands that remove it.
    let hint = match (&ours, foreign.is_empty()) {
        (None, false) => format!(
            "run: {}",
            foreign
                .iter()
                .map(|f| f.fix.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        ),
        _ => "run: playbook migrate".to_string(),
    };
    (!names.is_empty()).then(|| {
        Row::new(
            "-",
            Level::Warn,
            format!("{LEFTOVER_SERVICE}: {}", names.join("; ")),
        )
        .hint(hint)
    })
}

/// The telemetry rows: Claude Code's settings point at the playbook
/// collector, the collector runs, and rows arrived in the last day. Each
/// failing row names the one command that fixes it.
fn telemetry_rows(env: &Env) -> Vec<Row> {
    use crate::usage::collector::{probe, read_state, running, state_path, Probe as Reply};
    use crate::usage::telemetry_detect::{merged_vars, Configured};
    let off = matches!(
        crate::config::resolve_valid("usage.telemetry", &env.home, None),
        Ok((serde_json::Value::String(v), _, _)) if v == "off"
    );
    if off {
        return vec![Row::new(
            "-",
            Level::Info,
            "telemetry is off by the usage.telemetry setting",
        )
        .hint("run: playbook config set usage.telemetry auto")];
    }
    let paths = crate::usage::run::Paths::from_home(&env.home);
    let state = read_state(&state_path(&paths)).filter(|s| s.port != 0);
    let cwd = env.repo_root.clone().unwrap_or_else(|| env.home.clone());
    let port = state.as_ref().map(|s| s.port);
    let kind = crate::usage::telemetry_detect::configured_from_files(&env.home, &cwd, port);
    let mut rows = Vec::new();
    match kind {
        Configured::Playbook => rows.push(Row::new(
            "-",
            Level::Pass,
            format!(
                "telemetry env points at the playbook collector (127.0.0.1:{})",
                port.unwrap_or_default()
            ),
        )),
        Configured::Nothing | Configured::Disabled => {
            return vec![Row::new(
                "-",
                Level::Warn,
                if kind == Configured::Disabled {
                    "telemetry is switched off in Claude Code settings"
                } else {
                    "telemetry is not set up in Claude Code settings"
                },
            )
            .hint(TELEMETRY_SETUP_FIX)];
        }
        Configured::Other => {
            let endpoint = merged_vars(&env.home, &cwd, false)
                .and_then(|v| v.get("OTEL_EXPORTER_OTLP_ENDPOINT").cloned());
            let ours = port.map(|p| format!("http://127.0.0.1:{p}"));
            let theirs = endpoint.as_deref().map(|e| e.trim_end_matches('/'));
            let written = marker_endpoint(&env.home);
            if theirs.is_some() && theirs == ours.as_deref() {
                return vec![Row::new(
                    "-",
                    Level::Warn,
                    "telemetry points at the playbook collector but CLAUDE_CODE_ENABLE_TELEMETRY is not 1",
                )
                .hint(TELEMETRY_SETUP_FIX)];
            }
            // The endpoint init wrote earlier, now out of step with the port.
            if theirs.is_some() && theirs == written.as_deref() && ours.is_some() {
                return vec![Row::new(
                    "-",
                    Level::Warn,
                    format!(
                        "Claude Code settings send telemetry to {} but the collector port is {}",
                        theirs.unwrap_or_default(),
                        port.unwrap_or_default()
                    ),
                )
                .hint(TELEMETRY_SETUP_FIX)];
            }
            return vec![Row::new(
                "-",
                Level::Info,
                "telemetry is set up by you and does not point at the playbook collector",
            )
            .hint("run: playbook usage status")];
        }
    }
    let Some(state) = state else {
        return rows;
    };
    rows.push(daemon_row(&paths, state.port));
    let up = running(&state) && probe(state.port) == Reply::Collector;
    if up {
        rows.push(Row::new(
            "-",
            Level::Pass,
            format!("collector running (pid {})", state.pid),
        ));
    } else if crate::usage::collector::port_answers(state.port)
        && probe(state.port) == Reply::Silent
    {
        rows.push(
            Row::new(
                "-",
                Level::Warn,
                format!(
                    "the collector port {} is not answering right now",
                    state.port
                ),
            )
            .hint("run: playbook usage status"),
        );
    } else if crate::usage::collector::port_answers(state.port) {
        rows.push(
            Row::new(
                "-",
                Level::Fail,
                format!(
                    "another program owns the collector port {}, so telemetry is lost",
                    state.port
                ),
            )
            .hint("free the port, or run: playbook usage collect --port <N>, then playbook setup --only telemetry"),
        );
    } else {
        rows.push(
            Row::new(
                "-",
                Level::Warn,
                "collector not running, so telemetry sent now is lost",
            )
            .hint(crate::usage::daemon::START_HINT),
        );
    }
    let last =
        crate::usage::collector::status(&paths, crate::usage::collector::configured_retention())
            .ok()
            .and_then(|s| s.last_event);
    let now = i64::try_from(crate::common::time::now_epoch_secs()).unwrap_or(i64::MAX);
    rows.push(match last {
        Some(t) if now.saturating_sub(t) <= 86_400 => {
            Row::new("-", Level::Pass, "telemetry rows arrived in the last day")
        }
        Some(_) => Row::new("-", Level::Warn, "no telemetry rows in the last day")
            .hint("restart Claude Code so it reads the new settings, then start a session"),
        None => Row::new("-", Level::Warn, "no telemetry rows yet")
            .hint("restart Claude Code so it reads the new settings, then start a session"),
    });
    rows
}

/// Who holds the collector's lifetime lock, or why none does. Read only: it
/// opens the lock file without creating it and connects nowhere.
fn daemon_row(paths: &crate::usage::run::Paths, port: u16) -> Row {
    use crate::usage::daemon::{lock_path, lock_state, stamp_reason, LockState};
    let ours = env!("CARGO_PKG_VERSION");
    match lock_state(paths) {
        LockState::Held(holder) => match (holder.pid, holder.version) {
            (Some(pid), Some(version)) if version == ours => Row::new(
                "-",
                Level::Pass,
                format!("collector daemon: pid {pid}, version {version}, lock held"),
            ),
            (Some(pid), Some(version)) => Row::new(
                "-",
                Level::Warn,
                format!(
                    "collector daemon: version {version} (pid {pid}) differs from this binary ({ours})"
                ),
            )
            .hint("run: playbook usage stop, then any playbook command"),
            // No whole holder line yet: it is still starting.
            _ => Row::new("-", Level::Info, "collector daemon: starting, lock held"),
        },
        LockState::Free => match stamp_reason(paths) {
            Some(reason) => Row::new(
                "-",
                Level::Warn,
                format!("collector daemon: lock free, {reason}"),
            )
            .hint(format!(
                "free port {port}, or move with playbook usage collect --port <N> and playbook setup --only telemetry"
            )),
            None => Row::new("-", Level::Warn, "collector daemon: lock free")
                .hint(crate::usage::daemon::START_HINT),
        },
        LockState::Unavailable => Row::new(
            "-",
            Level::Warn,
            format!(
                "collector daemon: cannot read {}",
                lock_path(paths).display()
            ),
        )
        .hint("check the permissions of the playbook data folder"),
    }
}

/// One row for the data folder: path, files, sizes and any `*.migrated`
/// leftovers from a finished migration.
fn data_rows(env: &Env) -> Vec<Row> {
    let dir = crate::common::data::data_dir_from(&env.home);
    let files = crate::common::data::migrate::list_files(&dir);
    if files.is_empty() {
        return vec![Row::new(
            "-",
            Level::Info,
            format!("data folder {} is empty or missing", dir.display()),
        )];
    }
    let total: u64 = files.iter().map(|f| f.bytes).sum();
    let left: Vec<String> = files
        .iter()
        .filter(|f| crate::common::data::migrate::is_migrated_leftover(&f.path))
        .map(|f| f.path.display().to_string())
        .collect();
    let mut msg = format!(
        "data folder {}: {} files, {} KB",
        dir.display(),
        files.len(),
        total / 1024
    );
    if !left.is_empty() {
        msg.push_str(&format!(", migrated leftovers: {}", left.join(", ")));
    }
    let mut rows = vec![Row::new("-", Level::Info, msg)];
    rows.extend(orphan_rows(env, &dir));
    rows
}

/// Databases in the data folder with no worktree of this clone. They belong
/// to removed worktrees, or to another clone of the same repo, so the row
/// only reports them.
fn orphan_rows(env: &Env, data: &Path) -> Vec<Row> {
    let (Some(root), Some(slug)) = (&env.repo_root, &env.repo_slug) else {
        return Vec::new();
    };
    let Some(live) = crate::worktree::live_worktree_ids(root) else {
        return Vec::new();
    };
    let orphans = crate::common::data::orphan_dbs(data, slug, &live);
    if orphans.is_empty() {
        return Vec::new();
    }
    let list: Vec<String> = orphans
        .iter()
        .map(|(kind, p)| format!("{kind}: {}", p.display()))
        .collect();
    vec![Row::new(
        "-",
        Level::Info,
        format!(
            "{} database file(s) of {slug} match no worktree of this clone (removed, or another clone): {}",
            orphans.len(),
            list.join(", ")
        ),
    )
    .hint("the code index is a cache and safe to delete; check other clones before deleting gate records")]
}

/// The release check (`update.mode`): the mode and the last check, and a
/// warning with the fix when the check found a newer release.
fn update_rows(env: &Env) -> Vec<Row> {
    let status = crate::update::check::status_at(&env.home);
    let mut rows = vec![Row::new("-", Level::Info, status.line)];
    if let Some(latest) = status.newer {
        let current = env!("CARGO_PKG_VERSION");
        rows.push(
            Row::new(
                "-",
                Level::Warn,
                format!("playbook {latest} is available (you have {current})"),
            )
            .hint(format!("run: {}", status.fix)),
        );
    }
    rows
}

fn migration_rows(env: &Env) -> Vec<Row> {
    let ctx = crate::init::migrate::Ctx {
        home: env.home.clone(),
        claude_home: env.claude_home.clone(),
        self_root: crate::plugin_root::find(
            env.plugin_root.as_ref().and_then(|p| p.to_str()),
            &env.claude_home,
        ),
        repo: None,
        epoch: crate::common::time::now_epoch_secs(),
    };
    crate::init::migrate::pending_manual(&ctx)
        .into_iter()
        .map(|l| Row::new("-", Level::Info, format!("migration {l}")))
        .collect()
}

/// Longest the login shell probe may run. A slow profile reads as an answer we
/// could not get, not as a missing binary.
const LOGIN_SHELL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The hooks `playbook setup` writes into `settings.json`: present, current,
/// known to the binary that runs them, and reachable from a shell that starts
/// with a minimal PATH. Hook commands are the bare `playbook hook <name>`, so
/// the last probe starts a login shell with only the system directories on
/// PATH, the way a launcher or an IDE starts Claude Code, and asks it where
/// `playbook` is. The result is a hint, not proof: `init/path.rs` documents
/// how hook commands get their PATH. None of these rows fails: the binary and
/// its hooks are optional.
fn hooks_rows(env: &Env) -> Vec<Row> {
    use crate::common::plugin_state::{other_playbook_plugins, HOOKS_PLUGIN_KEY, PLUGIN_KEY};
    use crate::init::wire::{audit, wired_hooks};
    let total = wired_hooks().count();
    let found = audit(&env.settings());
    let mut rows = Vec::new();
    if found.present == 0 {
        rows.push(
            Row::new(
                "-",
                Level::Warn,
                "playbook hooks are not in settings.json, so the guards and worktree hooks do not run",
            )
            .hint("run: playbook setup --yes"),
        );
    } else if !found.missing.is_empty() {
        rows.push(
            Row::new(
                "-",
                Level::Warn,
                format!(
                    "{} of {total} playbook hook entries are missing from settings.json: {}",
                    total - found.present,
                    found.missing.join(", ")
                ),
            )
            .hint("run: playbook setup --yes"),
        );
    } else {
        rows.push(Row::new(
            "-",
            Level::Pass,
            format!("playbook hook entries in settings.json ({total} of {total}), including session-init, which loads the handoff after /clear when Claude Code runs it"),
        ));
    }
    for cmd in &found.stale {
        let hint = if crate::init::retired_hooks::is_retired_command(cmd) {
            "run: playbook setup --hooks"
        } else {
            "delete the entry from ~/.claude/settings.json by hand"
        };
        rows.push(
            Row::new(
                "-",
                Level::Warn,
                format!("settings.json has a hook the binary no longer writes: {cmd}"),
            )
            .hint(hint),
        );
    }
    let cwd = env.repo_root.clone().unwrap_or_else(|| env.home.clone());
    for key in other_playbook_plugins(&env.claude_home, &cwd) {
        let what = if key == HOOKS_PLUGIN_KEY {
            "the playbook-hooks plugin is still installed, so its hooks run next to the ones in settings.json".to_string()
        } else {
            format!("{key} is installed next to {PLUGIN_KEY}, so its skills and hooks run twice")
        };
        rows.push(
            Row::new("-", Level::Warn, what).hint(format!("run: claude plugin uninstall {key}")),
        );
    }
    if found.present > 0 {
        rows.extend(hook_binary_rows(env));
    }
    rows
}

/// What a login shell with a minimal PATH finds for `playbook`: its absolute
/// path, `Some(None)` when it found nothing, and `None` when the shell did not
/// answer (no shell set, it failed to start, or it took too long).
fn login_shell_playbook(env: &Env) -> Option<Option<PathBuf>> {
    if env.shell.is_empty() {
        return None;
    }
    let mut cmd = Command::new(&env.shell);
    cmd.args(["-lc", "printf 'PBPATH:%s\\n' \"$(command -v playbook)\""])
        .env_clear()
        .env("HOME", &env.home)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    let out = crate::common::proc::run_with_timeout(&mut cmd, LOGIN_SHELL_TIMEOUT)?;
    let text = String::from_utf8_lossy(&out.stdout);
    let found = text
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("PBPATH:"))
        .map(str::trim)?;
    Some(Path::new(found).is_absolute().then(|| PathBuf::from(found)))
}

/// The two ways a present hook entry can fail to run: the `playbook` a hook
/// reaches does not know the hook name, or a minimal-PATH login shell cannot
/// find `playbook` at all. The name check runs against the binary the login
/// shell found, and falls back to the first one on doctor's own PATH.
fn hook_binary_rows(env: &Env) -> Vec<Row> {
    use crate::init::wire::wired_hooks;
    let mut rows = Vec::new();
    let login = login_shell_playbook(env);
    match &login {
        Some(None) => rows.push(
            Row::new(
                "-",
                Level::Warn,
                "a login shell with a minimal PATH cannot find playbook, so hooks fail when Claude Code starts from a launcher or an IDE",
            )
            .hint("run: playbook setup, which puts the binary's directory on PATH for every shell start"),
        ),
        None if !env.shell.is_empty() => rows.push(Row::new(
            "-",
            Level::Info,
            "the login shell probe did not answer in time, so the minimal PATH check was skipped",
        )),
        _ => {}
    }
    let binary = match login {
        Some(Some(path)) => Some(path),
        _ => pathcheck::scan(&env.path_var).into_iter().next(),
    };
    if let Some(binary) = binary {
        let mut cmd = Command::new(&binary);
        cmd.args(["hook", "--help"]);
        if let Some(help) = output_of(cmd) {
            let mut unknown: Vec<&str> = wired_hooks()
                .map(|s| s.name)
                .filter(|n| !help.contains(n))
                .collect();
            unknown.sort_unstable();
            unknown.dedup();
            if !unknown.is_empty() {
                rows.push(
                    Row::new(
                        "-",
                        Level::Warn,
                        format!(
                            "{} does not know these hooks, so they fail every time: {}",
                            binary.display(),
                            unknown.join(", ")
                        ),
                    )
                    .hint("run: playbook update"),
                );
            }
        }
    }
    rows
}

/// The lines under the table: what to run for each failed layer, or that all
/// required layers pass.
pub fn closing(rows: &[Row]) -> Vec<&'static str> {
    let mut out = Vec::new();
    let failed: Vec<&str> = rows
        .iter()
        .filter(|r| r.level == Level::Fail)
        .map(|r| r.layer)
        .collect();
    if failed.iter().any(|l| ["1", "2", "3", "4", "5"].contains(l)) {
        out.push("Run /playbook:setup to fix layers 1 to 5.");
    }
    if failed.contains(&"6") {
        out.push("/playbook:setup cannot fix layer 6, it does not install the binary. Install it with install.sh or Homebrew and put its directory on PATH.");
    }
    if failed.contains(&"7") {
        out.push(
            "Layer 7 needs a hand edit: remove or fix the named entry in ~/.claude/settings.json.",
        );
    }
    let paused = rows
        .iter()
        .any(|r| r.layer == "1" && r.message.starts_with("plugin paused"));
    if paused {
        out.push("The plugin is paused, so hooks and the status line do nothing. Run: claude plugin enable playbook@pragmatic-engineer");
    }
    if failed.is_empty() && !paused {
        out.push("All required layers pass.");
    }
    out
}

/// The table as text: `LEVEL  message -- hint`, one row per line, then the
/// closing remediation line when a required layer failed.
pub fn render(rows: &[Row]) -> String {
    let mut out = String::new();
    for r in rows {
        // A row with a list keeps its hint on its first line, so the hint
        // never runs into the last line of the list.
        let (first, rest) = r.message.split_once('\n').unwrap_or((&r.message, ""));
        out.push_str(&format!("{}  {first}", r.level.label()));
        if let Some(h) = &r.hint {
            out.push_str(&format!(" -- {h}"));
        }
        out.push('\n');
        if !rest.is_empty() {
            out.push_str(rest);
            out.push('\n');
        }
    }
    for line in closing(rows) {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The table as JSON.
pub fn to_json(rows: &[Row]) -> Value {
    json!(rows
        .iter()
        .map(|r| json!({
            "layer": r.layer,
            "level": r.level.label(),
            "message": r.message,
            "hint": r.hint,
        }))
        .collect::<Vec<_>>())
}

/// Whether any required layer failed.
pub fn failed(rows: &[Row]) -> bool {
    rows.iter().any(|r| r.level == Level::Fail)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROBE_COUNT: usize = 21;
    const COMPLETION_PROBE_AT: usize = PROBE_COUNT - 1;

    fn env_for(home: &Path) -> Env {
        Env {
            home: home.to_path_buf(),
            claude_home: home.join(".claude"),
            plugin_root: None,
            shell: "/bin/zsh".to_string(),
            path_var: OsString::new(),
            repo_root: None,
            repo_slug: None,
        }
    }

    #[test]
    fn the_probes_table_has_21_entries_in_order() {
        // Arrange
        let home = crate::common::test_support::scratch_dir("doctor-probes");
        std::fs::create_dir_all(&home).unwrap();

        // Act
        let last = PROBES[COMPLETION_PROBE_AT](&env_for(&home));

        // Assert
        assert_eq!(PROBES.len(), PROBE_COUNT);
        assert_eq!(last.len(), 1, "{last:?}");
        assert_eq!(last[0].level, Level::Info);
        assert!(last[0].message.contains("completion"), "{last:?}");
    }

    #[test]
    fn a_paused_plugin_downgrades_only_the_hook_and_status_line_passes() {
        let rows = vec![
            Row::new("1", Level::Pass, "plugin enabled"),
            Row::new("2", Level::Pass, "safety guards wired (7 of 7)"),
            Row::new("3", Level::Pass, "launcher installed"),
            Row::new("5", Level::Pass, "status line ok"),
            Row::new(
                "-",
                Level::Pass,
                "playbook hook entries in settings.json (9 of 9)",
            ),
            Row::new("-", Level::Pass, "acli found"),
        ];

        let marked = mark_paused(rows);

        assert_eq!(marked[0].level, Level::Warn);
        assert!(marked[0].message.contains("paused"));
        assert_eq!(marked[1].level, Level::Warn);
        assert_eq!(marked[2].level, Level::Pass);
        assert_eq!(marked[3].level, Level::Warn);
        assert_eq!(marked[4].level, Level::Warn);
        assert_eq!(marked[5].level, Level::Pass);
        assert!(marked[4].message.ends_with("so they do not run"));
        assert!(marked[3].message.ends_with("so it does not run"));
        assert!(closing(&marked)[0].contains("paused"));
    }

    #[test]
    fn an_info_plugin_row_survives_with_the_paused_warning_in_front() {
        let marked = mark_paused(vec![Row::new(
            "1",
            Level::Info,
            "skills plugin not installed (optional)",
        )]);

        assert_eq!(marked.len(), 2);
        assert_eq!(marked[0].level, Level::Warn);
        assert_eq!(marked[1].level, Level::Info);
    }

    #[test]
    fn a_missing_plugin_row_is_added_and_a_failing_one_is_kept() {
        let without = mark_paused(vec![Row::new("2", Level::Fail, "not wired")]);
        let failing = mark_paused(vec![Row::new(
            "1",
            Level::Fail,
            "plugin installed but disabled",
        )]);

        assert_eq!((without[0].layer, without[0].level), ("1", Level::Warn));
        assert_eq!(without[1].level, Level::Fail);
        assert_eq!(failing.len(), 1);
        assert_eq!(failing[0].level, Level::Fail);
    }

    #[test]
    #[cfg(unix)]
    fn a_ready_converter_is_a_pass_row() {
        use crate::doc::converter::{Health, DOCTOR_ROW_BUNDLED};
        // Arrange and Act
        let row = doc_converter_row(&Health::Ready);

        // Assert
        assert_eq!(row.level, Level::Pass);
        assert_eq!(row.message, DOCTOR_ROW_BUNDLED);
    }

    #[test]
    #[cfg(unix)]
    fn a_bundled_converter_at_another_version_is_a_warn_row() {
        use crate::doc::converter::{Health, DOCTOR_ROW_SKEW};
        // Arrange and Act
        let row = doc_converter_row(&Health::VersionSkew);

        // Assert
        assert_eq!(row.level, Level::Warn);
        assert_eq!(row.message, DOCTOR_ROW_SKEW);
    }

    #[test]
    #[cfg(unix)]
    fn an_external_converter_is_a_warn_row() {
        use crate::doc::converter::{Health, DOCTOR_ROW_EXTERNAL};
        // Arrange and Act
        let row = doc_converter_row(&Health::External);

        // Assert
        assert_eq!(row.level, Level::Warn);
        assert_eq!(row.message, DOCTOR_ROW_EXTERNAL);
    }

    #[test]
    #[cfg(unix)]
    fn no_converter_is_an_info_row() {
        use crate::doc::converter::{Health, DOCTOR_ROW_NONE};
        // Arrange and Act
        let row = doc_converter_row(&Health::Missing);

        // Assert
        assert_eq!(row.level, Level::Info);
        assert_eq!(row.message, DOCTOR_ROW_NONE);
    }
}
