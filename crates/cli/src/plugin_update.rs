// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Updates the installed playbook plugin through the `claude` CLI after
//! `playbook update` and `playbook migrate` (ADR 0034, 2026-10-19
//! amendment). It runs `claude plugin update <id>` for the playbook plugin
//! only, never for another plugin. A missing CLI, a failed update, a plugin
//! that is off in Claude Code or `update.plugin` set to off never fails
//! the command: the result names the command to run by hand.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// The config key that turns the plugin update off.
pub const CONFIG_KEY: &str = "update.plugin";

/// Set by `playbook update` for the new binary's `migrate`, which then
/// leaves the plugin to the update itself, so it runs once.
pub const SKIP_ENV: &str = "PLAYBOOK_SKIP_PLUGIN_UPDATE";

/// The value of [`SKIP_ENV`] that turns the update off.
const SKIP_VALUE: &str = "1";

/// The id prefix of the playbook plugin in `installed_plugins.json`.
const ID_PREFIX: &str = "playbook@";

/// The most characters of `claude`'s last error line shown to the user.
const WHY_MAX_CHARS: usize = 200;

/// How long `claude plugin update` may take.
const TIMEOUT: Duration = Duration::from_secs(120);

/// What a plugin update did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// `claude plugin update` ran and the installed version moved.
    Updated {
        id: String,
        from: Option<String>,
        to: Option<String>,
    },
    /// `claude plugin update` ran and the version stayed.
    Current { id: String, version: Option<String> },
    /// Nothing ran, for this reason.
    Skipped(String),
    /// The playbook plugin is not installed, so there is nothing to say.
    NotInstalled,
    /// `claude` is not on PATH.
    NoCli { id: String },
    /// `claude plugin update` failed or timed out.
    Failed { id: String, why: String },
}

impl Outcome {
    /// One line for the user, with the command to run when it did not run.
    /// `None` when the plugin is not installed.
    pub fn line(&self) -> Option<String> {
        Some(match self {
            Outcome::NotInstalled => return None,
            Outcome::Updated { id, from, to } => format!(
                "plugin: updated {id}{}{}. Restart Claude Code to load it",
                from.as_deref()
                    .map(|v| format!(" from {v}"))
                    .unwrap_or_default(),
                to.as_deref()
                    .map(|v| format!(" to {v}"))
                    .unwrap_or_default(),
            ),
            Outcome::Current { id, version } => format!(
                "plugin: {id} is up to date{}",
                version
                    .as_deref()
                    .map(|v| format!(" ({v})"))
                    .unwrap_or_default()
            ),
            Outcome::Skipped(why) => format!("plugin: not updated, {why}"),
            Outcome::NoCli { id } => format!(
                "plugin: the claude command is not on PATH, so the plugin was not updated; run: {}",
                command(id)
            ),
            Outcome::Failed { id, why } => {
                format!("plugin: the update failed ({why}); run: {}", command(id))
            }
        })
    }
}

/// The start of every [`command`], for code that tells a plugin fix apart.
pub const COMMAND_PREFIX: &str = "claude plugin update ";

/// The command that updates plugin `id`.
pub fn command(id: &str) -> String {
    format!("{COMMAND_PREFIX}{id}")
}

/// What `update.plugin` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Auto,
    Ask,
    Off,
}

const MODE_AUTO: &str = "auto";
const MODE_ASK: &str = "ask";
const MODE_OFF: &str = "off";

/// The mode a stored value names, or `None` when it names none. The old
/// boolean values still count, so a store written before the key took words
/// keeps its meaning.
fn parse_mode(value: &serde_json::Value) -> Option<Mode> {
    match value {
        serde_json::Value::String(s) if s == MODE_AUTO => Some(Mode::Auto),
        serde_json::Value::String(s) if s == MODE_ASK => Some(Mode::Ask),
        serde_json::Value::String(s) if s == MODE_OFF => Some(Mode::Off),
        serde_json::Value::Bool(true) => Some(Mode::Auto),
        serde_json::Value::Bool(false) => Some(Mode::Off),
        _ => None,
    }
}

/// The mode for a stored value; a missing or unknown one is `Auto`, the
/// default.
fn mode_from(value: Option<&serde_json::Value>) -> Mode {
    value.and_then(parse_mode).unwrap_or(Mode::Auto)
}

/// The `update.plugin` mode for `home`. The raw stored value is read, not
/// the enum-checked one, because a `true` or `false` from before the key
/// took words is still valid here.
pub fn mode(home: &Path) -> Mode {
    let Ok((value, _)) = crate::config::resolve(CONFIG_KEY, home, None) else {
        return Mode::Auto;
    };
    if parse_mode(&value).is_none() {
        eprintln!(
            "warning: ignoring invalid value {value} for config key {CONFIG_KEY}, valid values are: {MODE_AUTO}, {MODE_ASK}, {MODE_OFF}"
        );
    }
    mode_from(Some(&value))
}

/// The id Claude Code lists for the playbook plugin in
/// `installed_plugins.json`: the first of `plugin_keys()` that is listed
/// (`playbook@amplifiedengineer`, then the old names), else the one `playbook@<marketplace>` entry. `None` when the plugin is
/// not installed, or more than one other marketplace lists it.
pub fn plugin_id(claude_home: &Path) -> Option<String> {
    use crate::common::plugin_state::{installed_entry_at, plugin_keys};
    for key in plugin_keys() {
        if installed_entry_at(claude_home, &key).is_some() {
            return Some(key);
        }
    }
    let text = std::fs::read_to_string(claude_home.join("plugins/installed_plugins.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let map = value.get("plugins").unwrap_or(&value).as_object()?;
    let mut ids = map
        .keys()
        .filter(|k| k.starts_with(ID_PREFIX) && !k.chars().any(char::is_control));
    let id = ids.next()?.clone();
    ids.next().is_none().then_some(id)
}

/// Whether `settings.json` turns plugin `id` off. Unlike the hook pause,
/// this follows the id Claude Code lists, so a fork id counts too.
fn is_off_in_claude_code(claude_home: &Path, id: &str) -> bool {
    std::fs::read_to_string(claude_home.join("settings.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.pointer("/enabledPlugins")?.get(id).cloned())
        == Some(serde_json::Value::Bool(false))
}

/// The folder Claude Code lists for plugin `id`.
fn installed_path(claude_home: &Path, id: &str) -> Option<PathBuf> {
    let entry = crate::common::plugin_state::installed_entry_at(claude_home, id)?;
    let first = match entry {
        serde_json::Value::Array(list) => list.into_iter().next()?,
        other => other,
    };
    Some(PathBuf::from(first.get("installPath")?.as_str()?))
}

/// The version Claude Code lists for plugin `id`, from its `installPath`.
fn installed_version(claude_home: &Path, id: &str) -> Option<String> {
    crate::plugin_root::plugin_version(&installed_path(claude_home, id)?)
        .filter(|v| crate::plugin_root::is_plain_version(v))
}

/// Copies the new plugin's system prompt over the placed one, so a user who
/// opted in gets new prompt text without running `playbook setup`. A copy the
/// user edited, or a prompt that was never placed, stays as it is.
fn refresh_system_prompt(home: &Path, plugin_dir: &Path) {
    use crate::init::system_prompt::{place_system_prompt, Placement};
    if let Ok(Placement::Placed(_)) = place_system_prompt(plugin_dir, home, false) {
        crate::init::migrate::record_system_prompt(home);
    }
}

/// What the update reads, passed in so tests use a scratch home and a fake
/// `claude`.
#[derive(Debug, Clone)]
pub struct Probe {
    pub home: PathBuf,
    /// The `claude` executable, or `None` when it is not on PATH.
    pub claude: Option<PathBuf>,
}

impl Probe {
    /// The probe for this process: `claude` from PATH.
    pub fn for_home(home: &Path) -> Probe {
        let path = std::env::var("PATH").unwrap_or_default();
        Probe {
            home: home.to_path_buf(),
            claude: crate::statusline::gh::find_in_path("claude", &path),
        }
    }
}

/// Updates the playbook plugin, unless `update.plugin` is off, the plugin
/// is not installed, or it is off in Claude Code.
pub fn run(probe: &Probe) -> Outcome {
    let claude_home = probe.home.join(".claude");
    if mode(&probe.home) == Mode::Off {
        return Outcome::Skipped(format!(
            "{CONFIG_KEY} is off; to turn it on, run: playbook config set {CONFIG_KEY} {MODE_AUTO}"
        ));
    }
    let Some(id) = plugin_id(&claude_home) else {
        return Outcome::NotInstalled;
    };
    if is_off_in_claude_code(&claude_home, &id) {
        return Outcome::Skipped(format!(
            "{id} is turned off in Claude Code; turn it on there, then run: {}",
            command(&id)
        ));
    }
    let Some(claude) = &probe.claude else {
        return Outcome::NoCli { id };
    };
    let before = installed_version(&claude_home, &id);
    let mut cmd = std::process::Command::new(claude);
    cmd.args(["plugin", "update", &id])
        .env("HOME", &probe.home)
        .env(SKIP_ENV, SKIP_VALUE)
        .stdin(std::process::Stdio::null());
    let Some(out) = crate::common::proc::run_with_timeout(&mut cmd, TIMEOUT) else {
        return Outcome::Failed {
            id,
            why: format!("it did not finish within {} s", TIMEOUT.as_secs()),
        };
    };
    if !out.status.success() {
        let text = String::from_utf8_lossy(&out.stderr);
        let last = text
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .map(|l| {
                crate::ui::render::sanitize(&l.chars().take(WHY_MAX_CHARS).collect::<String>())
            })
            .unwrap_or_default();
        let why = if last.is_empty() {
            format!("exit {}", out.status.code().unwrap_or(-1))
        } else {
            last
        };
        return Outcome::Failed { id, why };
    }
    if let Some(dir) = installed_path(&claude_home, &id) {
        refresh_system_prompt(&probe.home, &dir);
    }
    let after = installed_version(&claude_home, &id);
    if after.is_some() && after != before {
        Outcome::Updated {
            id,
            from: before,
            to: after,
        }
    } else {
        Outcome::Current {
            id,
            version: after.or(before),
        }
    }
}

/// [`run`] for a command, unless `--no-plugin` was passed or the caller set
/// [`SKIP_ENV`]. `None` when it was not asked to run.
pub fn run_for_command(home: &Path, no_plugin: bool) -> Option<Outcome> {
    let skip = no_plugin || std::env::var_os(SKIP_ENV).is_some_and(|v| v == SKIP_VALUE);
    if skip {
        return None;
    }
    let root = crate::common::paths::playbook_root_from(home);
    if in_flight(&root, env!("CARGO_PKG_VERSION"), now()) {
        return Some(Outcome::Skipped(
            "another playbook process is already updating it".to_string(),
        ));
    }
    Some(run(&Probe::for_home(home)))
}

/// The state key of the once-per-binary-version marker.
const MARKER_KEY: &str = "versions/plugin-update";

/// A failed or unchanged update is tried again after this long.
pub(crate) const FAILED_RETRY: Duration = Duration::from_secs(24 * 60 * 60);

/// A `running` marker older than this belongs to a run that never finished
/// (Ctrl-C, a crash), so the update is due again.
pub(crate) const RUNNING_STALE: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Running,
    Done,
    Failed,
}

impl State {
    const fn word(self) -> &'static str {
        match self {
            State::Running => "running",
            State::Done => "done",
            State::Failed => "failed",
        }
    }
}

/// What the last attempt did: `<binary version> <state> <epoch secs>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub version: String,
    pub state: State,
    pub at: u64,
}

impl Marker {
    /// `None` for text that is not a marker; callers treat that as another
    /// version, so the update is due.
    pub fn parse(text: &str) -> Option<Marker> {
        let mut parts = text.split_whitespace();
        let version = parts.next()?.to_string();
        let state = match parts.next()? {
            "running" => State::Running,
            "done" => State::Done,
            "failed" => State::Failed,
            _ => return None,
        };
        let at = parts.next()?.parse().ok()?;
        parts
            .next()
            .is_none()
            .then_some(Marker { version, state, at })
    }

    pub fn format(&self) -> String {
        format!("{} {} {}", self.version, self.state.word(), self.at)
    }
}

/// Everything the due check reads, so the rules run without a process
/// environment.
#[derive(Debug, Clone)]
pub struct DueInputs<'a> {
    /// `None` when there is no marker or it cannot be parsed.
    pub marker: Option<Marker>,
    pub running: &'a str,
    pub installed: Option<&'a str>,
    pub mode: Mode,
    pub skip_env: Option<&'a str>,
    pub headless: bool,
    pub ci_vendor: bool,
    pub disabled_in_claude: bool,
    pub plugin_id: Option<&'a str>,
}

/// Whether the plugin update should run now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Due {
    Run(Mode),
    /// The plugin is already at the running version and the marker does not
    /// say so yet: record `done`, run nothing.
    Current,
    NotDue(&'static str),
}

/// The rules of the due check, in order. Skips come first so a skipped run
/// never writes the marker.
pub fn due_from(i: &DueInputs<'_>, now: u64) -> Due {
    if i.mode == Mode::Off {
        return Due::NotDue("update.plugin is off");
    }
    if i.skip_env == Some(SKIP_VALUE) {
        return Due::NotDue("the skip variable is set");
    }
    if i.headless {
        return Due::NotDue("headless run");
    }
    if i.ci_vendor {
        return Due::NotDue("CI run");
    }
    if i.plugin_id.is_none() {
        return Due::NotDue("the plugin is not installed");
    }
    if i.disabled_in_claude {
        return Due::NotDue("the plugin is turned off in Claude Code");
    }
    let Some(installed) = i.installed else {
        return Due::NotDue("the installed plugin version is unknown");
    };
    let this_version = i.marker.as_ref().filter(|m| m.version == i.running);
    if crate::update::resolve::compare(installed, i.running).is_ge() {
        return match this_version {
            Some(m) if m.state == State::Done => Due::NotDue("already recorded as current"),
            _ => Due::Current,
        };
    }
    let age = |m: &Marker| Duration::from_secs(now.saturating_sub(m.at));
    match this_version {
        None => Due::Run(i.mode),
        Some(m) => match m.state {
            State::Done => Due::NotDue("already done for this version"),
            State::Failed if age(m) > FAILED_RETRY => Due::Run(i.mode),
            State::Failed => Due::NotDue("failed recently"),
            State::Running if age(m) > RUNNING_STALE => Due::Run(i.mode),
            State::Running => Due::NotDue("another process is running it"),
        },
    }
}

/// The raw marker text, the `seen` value [`claim`] compares against.
pub fn seen(root: &Path) -> Option<String> {
    crate::state::list_read_only(root, MARKER_KEY)
        .ok()?
        .into_iter()
        .find_map(|(k, v)| (k == MARKER_KEY && !v.is_empty()).then_some(v))
}

pub(crate) fn now() -> u64 {
    u64::try_from(crate::common::time::now_secs()).unwrap_or(0)
}

/// [`due_from`] with the inputs read from `home` and the process
/// environment. `seen_marker` is the marker text the caller already read, so
/// the store is opened once. Reads only.
pub fn due(home: &Path, seen_marker: Option<&str>) -> Due {
    let claude_home = home.join(".claude");
    let id = plugin_id(&claude_home);
    let installed = id
        .as_deref()
        .and_then(|id| installed_version(&claude_home, id));
    let skip_env = std::env::var(SKIP_ENV).ok();
    due_from(
        &DueInputs {
            marker: seen_marker.and_then(Marker::parse),
            running: env!("CARGO_PKG_VERSION"),
            installed: installed.as_deref(),
            mode: mode(home),
            skip_env: skip_env.as_deref(),
            headless: crate::common::headless::is_headless(),
            ci_vendor: crate::update::check::ci_vendor_env(|k| std::env::var(k).ok()),
            disabled_in_claude: id
                .as_deref()
                .is_some_and(|id| is_off_in_claude_code(&claude_home, id)),
            plugin_id: id.as_deref(),
        },
        now(),
    )
}

/// Moves the marker from `seen` to `running` in one transaction. Only the
/// caller that moved it runs the update, so two sessions that start
/// together run it once.
pub fn claim(root: &Path, seen: Option<&str>, running: &str, now: u64) -> bool {
    let marker = Marker {
        version: running.to_string(),
        state: State::Running,
        at: now,
    };
    crate::state::transaction(root, |tx| {
        if tx.get(MARKER_KEY)?.filter(|v| !v.is_empty()).as_deref() == seen {
            tx.set(MARKER_KEY, &marker.format())?;
            return Ok(true);
        }
        Ok(false)
    })
    .unwrap_or(false)
}

/// The state an attempt ends in. An update that left the plugin older than
/// the binary, changed or not, counts as failed, so it retries after
/// [`FAILED_RETRY`].
fn end_state(outcome: &Outcome, running: &str, installed_after: Option<&str>) -> State {
    let still_older =
        installed_after.is_none_or(|v| crate::update::resolve::compare(v, running).is_lt());
    match outcome {
        Outcome::Failed { .. } | Outcome::NoCli { .. } => State::Failed,
        Outcome::Current { .. } | Outcome::Updated { .. } if still_older => State::Failed,
        _ => State::Done,
    }
}

/// Records how the attempt ended and returns the state it wrote. Best
/// effort: a store that cannot be written leaves the `running` marker, which
/// goes stale on its own.
pub fn finish(
    root: &Path,
    outcome: &Outcome,
    running: &str,
    installed_after: Option<&str>,
    now: u64,
) -> State {
    let state = end_state(outcome, running, installed_after);
    write(root, running, state, now);
    state
}

/// The line to keep for the next session. A run that ended `failed` with the
/// plugin changed or unchanged but still older must not say "up to date".
fn notice_line(outcome: &Outcome, state: State) -> Option<String> {
    match outcome {
        Outcome::Current { id, .. } | Outcome::Updated { id, .. } if state == State::Failed => {
            Some(format!(
                "plugin: the playbook plugin is still older than this playbook binary; run: {}",
                command(id)
            ))
        }
        // The next session may already run the new plugin, so no restart advice.
        Outcome::Updated { id, from, to } => Some(format!(
            "plugin: updated {id}{}{}",
            from.as_deref()
                .map(|v| format!(" from {v}"))
                .unwrap_or_default(),
            to.as_deref()
                .map(|v| format!(" to {v}"))
                .unwrap_or_default(),
        )),
        _ => outcome.line(),
    }
}

/// Whether an update for `running` is in progress: its marker is `running`
/// and not yet stale.
pub fn in_flight(root: &Path, running: &str, now: u64) -> bool {
    seen(root).and_then(|t| Marker::parse(&t)).is_some_and(|m| {
        m.version == running
            && m.state == State::Running
            && Duration::from_secs(now.saturating_sub(m.at)) <= RUNNING_STALE
    })
}

/// Records `done` for the running version: the plugin is already current
/// ([`Due::Current`]), or the user declined the update.
pub fn record_done(root: &Path, running: &str, now: u64) {
    write(root, running, State::Done, now);
}

fn write(root: &Path, running: &str, state: State, now: u64) {
    let marker = Marker {
        version: running.to_string(),
        state,
        at: now,
    };
    let _ = crate::state::set(root, MARKER_KEY, &marker.format());
}

/// The detached body behind `update --refresh-plugin`: updates the plugin,
/// records the marker and leaves the outcome line for the next session. It
/// prints nothing. [`run`] does not read [`SKIP_ENV`], so the child the
/// hook started cannot be stopped by its own variable.
pub fn refresh_plugin(home: &Path) {
    let root = crate::common::paths::playbook_root_from(home);
    // Only a run the hook claimed proceeds, so a hand-started flag cannot
    // run a second update beside it.
    if !in_flight(&root, env!("CARGO_PKG_VERSION"), now()) {
        return;
    }
    let (outcome, state) = attempt(home, &root);
    if let Some(line) = notice_line(&outcome, state) {
        crate::notices::store_notice(&root, crate::notices::PLUGIN_UPDATE_KEY, &line);
    }
    // The plugin may have moved, so synced agents follow it now.
    let refreshed = crate::skills_sync::refresh_report(home, false);
    if let Some(line) = crate::skills_sync::notice_line(&refreshed) {
        crate::notices::store_notice(&root, crate::notices::SKILLS_SYNC_KEY, &line);
    }
}

/// Test seam that keeps the hook from starting the detached refresh. Read
/// only in debug builds.
#[cfg(debug_assertions)]
const NO_SPAWN_ENV: &str = "PLAYBOOK_TEST_NO_SPAWN";

#[cfg(debug_assertions)]
const NO_SPAWN_VALUE: &str = "1";

/// Starts `<exe> update --refresh-plugin` detached, like the release check
/// refresh: own process group, no stdio, and the skip variable removed so
/// the child is not mistaken for a nested run. Returns whether the child
/// started; the no-spawn seam counts as started, since nothing failed.
pub(crate) fn spawn_refresh_plugin() -> bool {
    spawn_detached(&["update", "--refresh-plugin"])
}

/// Starts `<exe> skills sync --refresh` detached, the same way.
pub(crate) fn spawn_refresh_skills() -> bool {
    spawn_detached(&["skills", "sync", "--refresh"])
}

fn spawn_detached(args: &[&str]) -> bool {
    #[cfg(debug_assertions)]
    if std::env::var_os(NO_SPAWN_ENV).is_some_and(|v| v == NO_SPAWN_VALUE) {
        return true;
    }
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args)
        .env_remove(SKIP_ENV)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn().is_ok()
}

/// Settles a claim after the spawn attempt. A child that never started
/// leaves no one to end the `running` marker, so it is set to `failed` and
/// retries after [`FAILED_RETRY`] instead of blocking for [`RUNNING_STALE`].
pub(crate) fn settle_spawn(root: &Path, running: &str, spawned: bool, now: u64) -> bool {
    if !spawned {
        write(root, running, State::Failed, now);
    }
    spawned
}

/// The test seam that stands in for a terminal. Read only in debug builds,
/// so a release binary cannot be fooled by it.
#[cfg(debug_assertions)]
const ASSUME_TTY_ENV: &str = "PLAYBOOK_TEST_ASSUME_TTY";

/// Seam value for a terminal on stderr and stdin.
#[cfg(debug_assertions)]
const ASSUME_BOTH_TTY: &str = "1";

/// Seam value for a terminal on stderr only, with stdin left as it is.
#[cfg(debug_assertions)]
const ASSUME_STDERR_TTY: &str = "stderr";

fn assumed_tty(for_stdin: bool) -> bool {
    #[cfg(debug_assertions)]
    if let Some(v) = std::env::var_os(ASSUME_TTY_ENV) {
        return v == ASSUME_BOTH_TTY || (!for_stdin && v == ASSUME_STDERR_TTY);
    }
    let _ = for_stdin;
    false
}

fn is_terminal(stream: &impl std::io::IsTerminal, for_stdin: bool) -> bool {
    assumed_tty(for_stdin) || stream.is_terminal()
}

/// Printed when a yes loses the claim to another running process.
const ALREADY_UPDATING: &str = "playbook: another playbook process is already updating the plugin";

/// The question `Ask` mode puts on stderr.
const QUESTION: &str = "Update the playbook plugin now? [Y/n] ";

/// Updates the plugin at the first command after an upgrade, when the
/// command is at a terminal (ADR 0034, 2026-10-19 amendment). `Auto` runs
/// the update; `Ask` asks first and needs a terminal on stdin too. Output
/// goes to stderr, so the command's own stdout stays as it was, and nothing
/// here changes the exit code.
pub fn on_start(command: &crate::Command, args: &[std::ffi::OsString]) {
    if !crate::heal::runs_on_start(command)
        || crate::update::check::has_json_arg(args)
        || !is_terminal(&std::io::stderr(), false)
    {
        return;
    }
    let home = crate::common::home_dir();
    if crate::common::home_error(&home).is_some() {
        return;
    }
    let stdin_is_terminal = is_terminal(&std::io::stdin(), true);
    let _ = std::panic::catch_unwind(|| start(&home, stdin_is_terminal));
}

/// The steps of one trigger: due check, mode, claim, update, report.
fn start(home: &Path, stdin_is_terminal: bool) {
    let root = crate::common::paths::playbook_root_from(home);
    let running = env!("CARGO_PKG_VERSION");
    let seen_before = seen(&root);
    let mode = match due(home, seen_before.as_deref()) {
        Due::Run(mode) => mode,
        Due::Current => {
            record_done(&root, running, now());
            return;
        }
        Due::NotDue(_) => return,
    };
    let claim_now = || claim(&root, seen_before.as_deref(), running, now());
    match mode {
        Mode::Auto => {
            if claim_now() {
                update_and_report(home, &root);
            }
        }
        Mode::Ask if stdin_is_terminal => match ask() {
            Some(true) if claim_now() => update_and_report(home, &root),
            Some(true) => lost_claim(&root, home, running),
            Some(false) if claim_now() => {
                record_done(&root, running, now());
                if let Some(id) = plugin_id(&home.join(".claude")) {
                    eprintln!(
                        "playbook: not updating the plugin; to update it, run: {}",
                        command(&id)
                    );
                }
            }
            _ => {}
        },
        Mode::Ask | Mode::Off => {}
    }
}

/// A yes lost the claim: say another process is updating only when the marker
/// still says so, else the marker moved while the question was open (a
/// no or a finished update), so name the manual command.
fn lost_claim(root: &Path, home: &Path, running: &str) {
    let busy = seen(root)
        .and_then(|t| Marker::parse(&t))
        .is_some_and(|m| m.version == running && m.state == State::Running);
    match plugin_id(&home.join(".claude")) {
        _ if busy => eprintln!("{ALREADY_UPDATING}"),
        Some(id) => eprintln!("playbook: to update the plugin, run: {}", command(&id)),
        None => {}
    }
}

/// `Some(answer)` for a yes or no; `None` for end of input or any other
/// reply, so the question comes back at the next command.
fn ask() -> Option<bool> {
    use crate::ui::wizard::{parse_answer, Answer};
    use std::io::{BufRead, Write};
    eprint!("{QUESTION}");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).ok()? == 0 {
        return None;
    }
    match parse_answer(&line, true) {
        Answer::Yes => Some(true),
        Answer::No => Some(false),
        _ => None,
    }
}

fn update_and_report(home: &Path, root: &Path) {
    if let Some(id) = plugin_id(&home.join(".claude")) {
        eprintln!("playbook: updating the playbook plugin ({})", command(&id));
    }
    let (outcome, state) = attempt(home, root);
    let line = match outcome {
        Outcome::Updated { .. } => outcome.line(),
        _ => notice_line(&outcome, state),
    };
    if let Some(line) = line {
        eprintln!("{line}");
    }
}

/// Runs the update, records how it ended and returns both.
fn attempt(home: &Path, root: &Path) -> (Outcome, State) {
    let claude_home = home.join(".claude");
    let outcome = run(&Probe::for_home(home));
    let after = plugin_id(&claude_home).and_then(|id| installed_version(&claude_home, &id));
    let state = finish(
        root,
        &outcome,
        env!("CARGO_PKG_VERSION"),
        after.as_deref(),
        now(),
    );
    (outcome, state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;
    use std::fs;

    fn plugin(home: &Path, version: &str) -> PathBuf {
        let dir = home.join(format!(
            ".claude/plugins/cache/amplifiedengineer/playbook/{version}"
        ));
        fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        fs::create_dir_all(dir.join("skills")).unwrap();
        fs::write(
            dir.join(".claude-plugin/plugin.json"),
            format!(r#"{{"name":"playbook","version":"{version}"}}"#),
        )
        .unwrap();
        dir
    }

    fn install(home: &Path, key: &str, dir: &Path) {
        fs::write(
            home.join(".claude/plugins/installed_plugins.json"),
            format!(
                r#"{{"version":2,"plugins":{{"{key}":[{{"scope":"user","installPath":"{}"}}],"other@x":[{{"scope":"user","installPath":"/nowhere"}}]}}}}"#,
                dir.display()
            ),
        )
        .unwrap();
    }

    /// A fake `claude` that logs its arguments and, for `plugin update`,
    /// points installed_plugins.json at `to` when given.
    fn fake_claude(home: &Path, to: Option<&Path>, exit: i32) -> PathBuf {
        let bin = home.join("bin/claude");
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        let swap = to.map_or(String::new(), |dir| {
            format!(
                "printf '%s' '{{\"version\":2,\"plugins\":{{\"playbook@amplifiedengineer\":[{{\"scope\":\"user\",\"installPath\":\"{}\"}}]}}}}' > \"$HOME/.claude/plugins/installed_plugins.json\"\n",
                dir.display()
            )
        });
        fs::write(
            &bin,
            format!(
                "#!/bin/sh\necho \"$*\" >> \"$HOME/claude-calls.log\"\n{swap}echo 'update failed: network' >&2\nexit {exit}\n"
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    fn calls(home: &Path) -> String {
        fs::read_to_string(home.join("claude-calls.log")).unwrap_or_default()
    }

    #[test]
    fn a_failed_spawn_marks_the_claim_failed() {
        // Arrange
        let root = scratch_dir("plugin-update-spawn-failed");
        assert!(claim(&root, None, "1.2.3", 100));

        // Act
        let spawned = settle_spawn(&root, "1.2.3", false, 101);

        // Assert
        assert!(!spawned);
        assert_eq!(seen(&root).as_deref(), Some("1.2.3 failed 101"));
    }

    #[test]
    fn a_started_spawn_leaves_the_running_marker() {
        // Arrange
        let root = scratch_dir("plugin-update-spawn-ok");
        assert!(claim(&root, None, "1.2.3", 100));

        // Act
        let spawned = settle_spawn(&root, "1.2.3", true, 101);

        // Assert
        assert!(spawned);
        assert_eq!(seen(&root).as_deref(), Some("1.2.3 running 100"));
    }

    #[test]
    fn a_still_older_plugin_never_reads_as_up_to_date() {
        // Arrange
        let id = "playbook@amplifiedengineer".to_string();
        let current = Outcome::Current {
            id: id.clone(),
            version: Some("0.36.0".to_string()),
        };
        let updated = Outcome::Updated {
            id,
            from: Some("0.35.0".to_string()),
            to: Some("0.36.0".to_string()),
        };

        // Act
        let lines = [
            notice_line(&current, State::Failed),
            notice_line(&updated, State::Failed),
        ];

        // Assert
        for line in lines {
            let line = line.unwrap();
            assert!(
                line.contains("still older than this playbook binary"),
                "{line}"
            );
            assert!(line.ends_with("claude plugin update playbook@amplifiedengineer"));
        }
    }

    #[test]
    fn a_current_plugin_that_reached_done_keeps_its_own_line() {
        // Arrange
        let current = Outcome::Current {
            id: "playbook@x".to_string(),
            version: Some("1.0.0".to_string()),
        };

        // Act
        let line = notice_line(&current, State::Done);

        // Assert
        assert_eq!(line, current.line());
    }

    #[test]
    fn in_flight_is_true_only_for_a_fresh_running_marker_of_this_version() {
        // Arrange
        let root = scratch_dir("plugin-update-in-flight");
        assert!(claim(&root, None, "1.2.3", 1000));
        let stale = 1000 + RUNNING_STALE.as_secs() + 1;

        // Act and Assert
        assert!(in_flight(&root, "1.2.3", 1001));
        assert!(!in_flight(&root, "1.2.4", 1001));
        assert!(!in_flight(&root, "1.2.3", stale));
    }

    #[test]
    fn it_updates_only_the_playbook_plugin_and_says_to_restart() {
        // Arrange
        let home = scratch_dir("plugin-update-ok");
        let old = plugin(&home, "0.36.0");
        let new = plugin(&home, "0.37.0");
        install(&home, "playbook@amplifiedengineer", &old);
        let claude = fake_claude(&home, Some(&new), 0);

        // Act
        let out = run(&Probe {
            home: home.clone(),
            claude: Some(claude),
        });

        // Assert
        assert_eq!(calls(&home), "plugin update playbook@amplifiedengineer\n");
        assert_eq!(
            out.line().unwrap(),
            "plugin: updated playbook@amplifiedengineer from 0.36.0 to 0.37.0. Restart Claude Code to load it"
        );
    }

    #[test]
    fn an_install_that_kept_the_old_name_is_updated_under_that_name() {
        let home = scratch_dir("plugin-update-old-name");
        let old = plugin(&home, "0.36.0");
        let new = plugin(&home, "0.37.0");
        install(&home, "playbook@pragmatic-engineer", &old);
        let claude = fake_claude(&home, Some(&new), 0);

        run(&Probe {
            home: home.clone(),
            claude: Some(claude),
        });

        assert_eq!(calls(&home), "plugin update playbook@pragmatic-engineer\n");
    }

    #[test]
    fn the_new_id_is_picked_first_when_both_ids_are_listed() {
        let home = scratch_dir("plugin-update-both-ids");
        let dir = plugin(&home, "0.37.0");
        fs::write(
            home.join(".claude/plugins/installed_plugins.json"),
            format!(
                r#"{{"version":2,"plugins":{{"playbook@pragmatic-engineer":[{{"installPath":"{0}"}}],"playbook@amplifiedengineer":[{{"installPath":"{0}"}}]}}}}"#,
                dir.display()
            ),
        )
        .unwrap();

        assert_eq!(
            plugin_id(&home.join(".claude")).as_deref(),
            Some("playbook@amplifiedengineer")
        );
        install(&home, "playbook@pragmatic-engineer", &dir);
        assert_eq!(
            plugin_id(&home.join(".claude")).as_deref(),
            Some("playbook@pragmatic-engineer")
        );
    }

    fn placed_prompt_after_update(edit_before: Option<&str>) -> (String, bool) {
        let home = scratch_dir(if edit_before.is_some() {
            "plugin-update-prompt-edited"
        } else {
            "plugin-update-prompt"
        });
        let old = plugin(&home, "0.36.0");
        let new = plugin(&home, "0.37.0");
        for (dir, text) in [(&old, "old rules"), (&new, "new rules")] {
            fs::create_dir_all(dir.join("prompts")).unwrap();
            fs::write(dir.join("prompts/SYSTEM_PROMPT.md"), text).unwrap();
        }
        let placed = crate::init::system_prompt::path(&home);
        fs::create_dir_all(placed.parent().unwrap()).unwrap();
        fs::write(&placed, "old rules").unwrap();
        crate::init::migrate::record_system_prompt(&home);
        if let Some(text) = edit_before {
            fs::write(&placed, text).unwrap();
        }
        install(&home, "playbook@amplifiedengineer", &old);
        let claude = fake_claude(&home, Some(&new), 0);

        run(&Probe {
            home: home.clone(),
            claude: Some(claude),
        });

        let recorded = crate::init::migrate::placed_unchanged(&home, "system-prompt", &placed);
        (fs::read_to_string(&placed).unwrap(), recorded)
    }

    #[test]
    fn an_update_refreshes_the_placed_system_prompt_from_the_new_plugin() {
        let (text, recorded) = placed_prompt_after_update(None);

        assert_eq!(text, "new rules");
        assert!(recorded, "the new copy's hash must be recorded");
    }

    #[test]
    fn an_update_leaves_an_edited_system_prompt_alone() {
        assert_eq!(placed_prompt_after_update(Some("my rules")).0, "my rules");
    }

    #[test]
    fn a_missing_cli_or_a_failed_update_names_the_command_and_does_not_fail() {
        let home = scratch_dir("plugin-update-fail");
        let dir = plugin(&home, "0.36.0");
        install(&home, "playbook@amplifiedengineer", &dir);
        let missing = run(&Probe {
            home: home.clone(),
            claude: None,
        });
        let failed = run(&Probe {
            home: home.clone(),
            claude: Some(fake_claude(&home, None, 1)),
        });
        assert!(missing
            .line()
            .unwrap()
            .ends_with("run: claude plugin update playbook@amplifiedengineer"));
        assert_eq!(
            failed.line().unwrap(),
            "plugin: the update failed (update failed: network); run: claude plugin update playbook@amplifiedengineer"
        );
    }

    #[test]
    fn a_plugin_that_is_off_or_a_false_config_key_runs_nothing() {
        // Arrange
        let home = scratch_dir("plugin-update-off");
        let dir = plugin(&home, "0.36.0");
        install(&home, "playbook@amplifiedengineer", &dir);
        let claude = fake_claude(&home, None, 0);
        fs::write(
            home.join(".claude/settings.json"),
            r#"{"enabledPlugins":{"playbook@amplifiedengineer":false}}"#,
        )
        .unwrap();

        // Act
        let off = run(&Probe {
            home: home.clone(),
            claude: Some(claude.clone()),
        });
        fs::write(home.join(".claude/settings.json"), "{}").unwrap();
        let root = crate::common::paths::playbook_root_from(&home);
        fs::create_dir_all(&root).unwrap();
        crate::config::store::put(
            &root,
            crate::config::write::Tier::Global,
            None,
            CONFIG_KEY,
            &serde_json::json!("off"),
        )
        .unwrap();
        let config_off = run(&Probe {
            home: home.clone(),
            claude: Some(claude),
        });

        // Assert
        let (off, config_off) = (off.line().unwrap(), config_off.line().unwrap());
        assert!(off.contains("turned off in Claude Code"), "{off}");
        assert!(config_off.contains("update.plugin is off"), "{config_off}");
        assert_eq!(calls(&home), "");
    }

    #[test]
    fn an_id_with_a_control_character_is_not_used() {
        let home = scratch_dir("plugin-update-ctl");
        fs::create_dir_all(home.join(".claude/plugins")).unwrap();
        fs::write(
            home.join(".claude/plugins/installed_plugins.json"),
            r#"{"version":2,"plugins":{"playbook@evil\u001b[2J":[{"scope":"user","installPath":"/x"}]}}"#,
        )
        .unwrap();

        assert_eq!(plugin_id(&home.join(".claude")), None);
    }

    #[test]
    fn mode_reads_each_word() {
        // Arrange
        let words = [("auto", Mode::Auto), ("ask", Mode::Ask), ("off", Mode::Off)];

        for (word, expected) in words {
            // Act
            let mode = mode_from(Some(&serde_json::json!(word)));

            // Assert
            assert_eq!(mode, expected, "{word}");
        }
        assert_eq!(
            mode_from(None),
            Mode::Auto,
            "a missing value is the default"
        );
    }

    #[test]
    fn a_stored_true_reads_as_auto_and_false_as_off() {
        // Arrange
        let home = scratch_dir("plugin-update-bool-mode");
        let root = crate::common::paths::playbook_root_from(&home);
        fs::create_dir_all(&root).unwrap();
        let put = |value: serde_json::Value| {
            crate::config::store::put(
                &root,
                crate::config::write::Tier::Global,
                None,
                CONFIG_KEY,
                &value,
            )
            .unwrap();
        };

        // Act
        put(serde_json::json!(true));
        let on = mode(&home);
        put(serde_json::json!(false));
        let off = mode(&home);

        // Assert
        assert_eq!((on, off), (Mode::Auto, Mode::Off));
    }

    #[test]
    fn every_allowed_word_for_the_key_parses_to_a_mode() {
        // Arrange
        let words = crate::config::keys::allowed_enum_values(CONFIG_KEY).unwrap();

        for word in words {
            // Act
            let mode = parse_mode(&serde_json::json!(word));

            // Assert
            assert!(mode.is_some(), "{word}");
        }
    }

    #[test]
    fn a_stored_ask_updates_the_plugin() {
        // Arrange
        let home = scratch_dir("plugin-update-ask");
        let dir = plugin(&home, "0.36.0");
        install(&home, "playbook@amplifiedengineer", &dir);
        let claude = fake_claude(&home, None, 0);
        let root = crate::common::paths::playbook_root_from(&home);
        fs::create_dir_all(&root).unwrap();
        crate::config::store::put(
            &root,
            crate::config::write::Tier::Global,
            None,
            CONFIG_KEY,
            &serde_json::json!(MODE_ASK),
        )
        .unwrap();

        // Act
        let out = run(&Probe {
            home: home.clone(),
            claude: Some(claude),
        });

        // Assert
        assert_eq!(calls(&home), "plugin update playbook@amplifiedengineer\n");
        assert!(!matches!(out, Outcome::Skipped(_)), "{out:?}");
    }

    #[test]
    fn an_unknown_word_reads_as_auto() {
        // Arrange
        let unknown = serde_json::json!("sometimes");

        // Act
        let mode = mode_from(Some(&unknown));

        // Assert
        assert_eq!(mode, Mode::Auto);
        assert_eq!(parse_mode(&unknown), None);
    }

    #[test]
    fn a_fork_id_that_is_off_runs_nothing() {
        // Arrange
        let home = scratch_dir("plugin-update-fork-off");
        let dir = plugin(&home, "0.36.0");
        install(&home, "playbook@my-fork", &dir);
        let claude = fake_claude(&home, None, 0);
        fs::write(
            home.join(".claude/settings.json"),
            r#"{"enabledPlugins":{"playbook@my-fork":false}}"#,
        )
        .unwrap();

        // Act
        let out = run(&Probe {
            home: home.clone(),
            claude: Some(claude),
        });

        // Assert
        assert!(out.line().unwrap().contains("turned off"));
        assert_eq!(calls(&home), "");
    }

    #[test]
    fn the_id_comes_from_installed_plugins_and_nothing_runs_without_the_plugin() {
        let home = scratch_dir("plugin-update-id");
        fs::create_dir_all(home.join(".claude/plugins")).unwrap();
        assert_eq!(plugin_id(&home.join(".claude")), None);
        assert_eq!(
            run(&Probe {
                home: home.clone(),
                claude: None
            }),
            Outcome::NotInstalled
        );
        let dir = plugin(&home, "0.37.0");
        install(&home, "playbook@my-fork", &dir);
        assert_eq!(
            plugin_id(&home.join(".claude")).as_deref(),
            Some("playbook@my-fork")
        );
        install(&home, "playbook@amplifiedengineer", &dir);
        assert_eq!(
            plugin_id(&home.join(".claude")).as_deref(),
            Some("playbook@amplifiedengineer")
        );
    }

    const RUN: &str = "1.2.0";
    const T0: u64 = 1_000_000;
    const H24: u64 = 24 * 60 * 60;
    const M10: u64 = 10 * 60;

    fn marker(version: &str, state: State, at: u64) -> Option<Marker> {
        Some(Marker {
            version: version.into(),
            state,
            at,
        })
    }

    fn base() -> DueInputs<'static> {
        DueInputs {
            marker: marker("1.1.0", State::Done, T0),
            running: RUN,
            installed: Some("1.1.0"),
            mode: Mode::Auto,
            skip_env: None,
            headless: false,
            ci_vendor: false,
            disabled_in_claude: false,
            plugin_id: Some("playbook@amplifiedengineer"),
        }
    }

    #[test]
    fn due_from_applies_every_rule() {
        let not_due = |why| Due::NotDue(why);
        let cases: Vec<(&str, DueInputs, u64, Due)> = vec![
            ("older marker version", base(), T0, Due::Run(Mode::Auto)),
            (
                "ask is passed through",
                DueInputs {
                    mode: Mode::Ask,
                    ..base()
                },
                T0,
                Due::Run(Mode::Ask),
            ),
            (
                "no marker",
                DueInputs {
                    marker: None,
                    ..base()
                },
                T0,
                Due::Run(Mode::Auto),
            ),
            (
                "failed at 24h plus 1s",
                DueInputs {
                    marker: marker(RUN, State::Failed, T0),
                    ..base()
                },
                T0 + H24 + 1,
                Due::Run(Mode::Auto),
            ),
            (
                "failed at 24h minus 1s",
                DueInputs {
                    marker: marker(RUN, State::Failed, T0),
                    ..base()
                },
                T0 + H24 - 1,
                not_due("failed recently"),
            ),
            (
                "failed at exactly 24h",
                DueInputs {
                    marker: marker(RUN, State::Failed, T0),
                    ..base()
                },
                T0 + H24,
                not_due("failed recently"),
            ),
            (
                "running at 10min plus 1s",
                DueInputs {
                    marker: marker(RUN, State::Running, T0),
                    ..base()
                },
                T0 + M10 + 1,
                Due::Run(Mode::Auto),
            ),
            (
                "running at 10min minus 1s",
                DueInputs {
                    marker: marker(RUN, State::Running, T0),
                    ..base()
                },
                T0 + M10 - 1,
                not_due("another process is running it"),
            ),
            (
                "a clock set back",
                DueInputs {
                    marker: marker(RUN, State::Running, T0),
                    ..base()
                },
                T0 - 5,
                not_due("another process is running it"),
            ),
            (
                "done for this version",
                DueInputs {
                    marker: marker(RUN, State::Done, T0),
                    ..base()
                },
                T0 + 9 * H24,
                not_due("already done for this version"),
            ),
            (
                "plugin equal, marker done",
                DueInputs {
                    marker: marker(RUN, State::Done, T0),
                    installed: Some(RUN),
                    ..base()
                },
                T0,
                not_due("already recorded as current"),
            ),
            (
                "plugin equal, no record",
                DueInputs {
                    installed: Some(RUN),
                    ..base()
                },
                T0,
                Due::Current,
            ),
            (
                "plugin newer",
                DueInputs {
                    installed: Some("1.3.0"),
                    ..base()
                },
                T0,
                Due::Current,
            ),
            (
                "a prerelease plugin is older than the release",
                DueInputs {
                    installed: Some("1.2.0-rc.1"),
                    ..base()
                },
                T0,
                Due::Run(Mode::Auto),
            ),
            (
                "plugin version unknown",
                DueInputs {
                    installed: None,
                    ..base()
                },
                T0,
                not_due("the installed plugin version is unknown"),
            ),
            (
                "mode off",
                DueInputs {
                    mode: Mode::Off,
                    ..base()
                },
                T0,
                not_due("update.plugin is off"),
            ),
            (
                "skip env 1",
                DueInputs {
                    skip_env: Some("1"),
                    ..base()
                },
                T0,
                not_due("the skip variable is set"),
            ),
            (
                "skip env 0 does not skip",
                DueInputs {
                    skip_env: Some("0"),
                    ..base()
                },
                T0,
                Due::Run(Mode::Auto),
            ),
            (
                "headless",
                DueInputs {
                    headless: true,
                    ..base()
                },
                T0,
                not_due("headless run"),
            ),
            (
                "CI vendor",
                DueInputs {
                    ci_vendor: true,
                    ..base()
                },
                T0,
                not_due("CI run"),
            ),
            (
                "disabled in Claude Code",
                DueInputs {
                    disabled_in_claude: true,
                    ..base()
                },
                T0,
                not_due("the plugin is turned off in Claude Code"),
            ),
            (
                "no id",
                DueInputs {
                    plugin_id: None,
                    ..base()
                },
                T0,
                not_due("the plugin is not installed"),
            ),
            (
                "off wins over current",
                DueInputs {
                    mode: Mode::Off,
                    installed: Some(RUN),
                    ..base()
                },
                T0,
                not_due("update.plugin is off"),
            ),
        ];
        for (name, inputs, now, expected) in cases {
            // Act
            let got = due_from(&inputs, now);

            // Assert
            assert_eq!(got, expected, "{name}");
        }
    }

    #[test]
    fn a_marker_round_trips_and_unparsable_text_is_not_a_marker() {
        // Arrange
        let m = Marker {
            version: "1.2.0-rc.1".into(),
            state: State::Failed,
            at: 42,
        };

        // Act
        let parsed = Marker::parse(&m.format());

        // Assert
        assert_eq!(parsed, Some(m));
        for bad in [
            "",
            "1.2.0",
            "1.2.0 done",
            "1.2.0 stuck 5",
            "1.2.0 done x",
            "1.2.0 done 5 6",
        ] {
            assert_eq!(Marker::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn an_unparsable_marker_counts_as_another_version() {
        // Arrange
        let inputs = DueInputs {
            marker: Marker::parse("garbage"),
            ..base()
        };

        // Act
        let got = due_from(&inputs, T0);

        // Assert
        assert_eq!(got, Due::Run(Mode::Auto));
    }

    fn store(tag: &str) -> PathBuf {
        let root = crate::common::paths::playbook_root_from(&scratch_dir(tag));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn a_claim_succeeds_once_for_callers_that_read_the_same_value() {
        // Arrange
        let root = store("pu-claim");
        let seen = seen(&root);

        // Act
        let first = claim(&root, seen.as_deref(), RUN, T0);
        let second = claim(&root, seen.as_deref(), RUN, T0);

        // Assert
        assert!(first);
        assert!(!second);
        assert_eq!(super::seen(&root), Some(format!("{RUN} running {T0}")));
    }

    #[test]
    fn a_claim_moves_from_the_value_it_read() {
        // Arrange
        let root = store("pu-claim-from");
        crate::state::set(&root, MARKER_KEY, "1.1.0 done 5").unwrap();

        // Act
        let stale = claim(&root, None, RUN, T0);
        let fresh = claim(&root, Some("1.1.0 done 5"), RUN, T0);

        // Assert
        assert!(!stale);
        assert!(fresh);
    }

    fn failed() -> Outcome {
        Outcome::Failed {
            id: "p@x".into(),
            why: "timeout".into(),
        }
    }

    fn current() -> Outcome {
        Outcome::Current {
            id: "p@x".into(),
            version: None,
        }
    }

    fn updated() -> Outcome {
        Outcome::Updated {
            id: "p@x".into(),
            from: None,
            to: None,
        }
    }

    #[test]
    fn finish_writes_failed_or_done_by_outcome() {
        let cases: Vec<(&str, Outcome, Option<&str>, State)> = vec![
            ("failed", failed(), Some(RUN), State::Failed),
            (
                "current but still older",
                current(),
                Some("1.1.0"),
                State::Failed,
            ),
            (
                "current, version unreadable",
                current(),
                None,
                State::Failed,
            ),
            (
                "current and at the running version",
                current(),
                Some(RUN),
                State::Done,
            ),
            ("updated", updated(), Some(RUN), State::Done),
            (
                "updated but still older",
                updated(),
                Some("1.1.0"),
                State::Failed,
            ),
            ("not installed", Outcome::NotInstalled, None, State::Done),
            (
                "no claude on PATH",
                Outcome::NoCli { id: "p@x".into() },
                Some("1.1.0"),
                State::Failed,
            ),
        ];
        for (name, outcome, after, expected) in cases {
            // Arrange
            let root = store("pu-finish");

            // Act
            finish(&root, &outcome, RUN, after, T0);

            // Assert
            let written = Marker::parse(&seen(&root).unwrap()).unwrap();
            assert_eq!(
                written,
                Marker {
                    version: RUN.into(),
                    state: expected,
                    at: T0
                },
                "{name}"
            );
        }
    }

    #[test]
    fn a_current_plugin_is_recorded_done() {
        // Arrange
        let root = store("pu-record");

        // Act
        record_done(&root, RUN, T0);

        // Assert
        assert_eq!(seen(&root), Some(format!("{RUN} done {T0}")));
    }
}
