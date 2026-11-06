// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! One of the two places `src/usage` touches the program that embeds it (the
//! other is `util`). The embedder
//! implements [`Host`] once and installs it with [`install`] before calling
//! into usage. Without an installed host the standalone one is used (see
//! `standalone`). Config values, the state directory, the program name in user
//! facing text, the repo path layout and the sessions panel data all come from
//! here.

use super::run::Paths;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use super::session_view::Record;

/// What usage needs from the program that embeds it.
pub trait Host: Send + Sync {
    /// The program name in user facing messages.
    fn program(&self) -> &str;

    /// The command line a user runs for `args`, for messages. `args` always
    /// starts with `usage`: `usage --summary` becomes `playbook usage
    /// --summary` for a host that nests the command, or `pbtop --summary`
    /// for a program that is the usage tool itself.
    fn command(&self, args: &str) -> String;

    /// The command that runs one part of playbook's setup, such as
    /// `telemetry`, for messages. A host that may run next to an older
    /// playbook adds the old command name.
    fn setup_command(&self, part: &str) -> String;

    /// What to do when telemetry points at the collector and none runs, for
    /// messages. Only a host whose commands start the collector can say
    /// [`daemon::START_HINT`](super::daemon::START_HINT).
    fn start_hint(&self) -> String;

    /// The arguments, before any usage subcommand, that make the program run
    /// usage. Used to start a detached `warm` run of the same program.
    fn launch_args(&self) -> Vec<String>;

    /// Path fragment under which the host keeps one folder per cloned repo
    /// (`<marker><owner>/<repo>/<worktree>`), used to name a repo from a cwd.
    /// `None` when the host has no such layout.
    fn repos_marker(&self) -> Option<&str>;

    /// The user's home directory.
    fn home_dir(&self) -> PathBuf;

    /// The value of config key `key` (for example `usage.theme`) for the user
    /// at `home`, or `None` when it is not set or cannot be read. Usage falls
    /// back to its own default then.
    fn config_value(&self, key: &str, home: &Path) -> Option<Value>;

    /// Stores config key `key` where [`Host::config_value`] reads it back, for
    /// the user at `home`. A host that cannot write settings keeps this
    /// default.
    fn set_config_value(&self, _key: &str, _value: &Value, _home: &Path) -> Result<(), String> {
        Err("this program cannot change settings".to_string())
    }

    /// The folder holding the usage database under `home`.
    fn state_dir_from(&self, home: &Path) -> PathBuf;

    /// The folder holding the usage database for the real environment.
    fn state_dir(&self) -> PathBuf;

    /// The records for the sessions panel, with the time they were read at.
    fn sessions(&self, paths: &Paths, home: &Path) -> Result<(Vec<Record>, i64), String>;
}

static HOST: RwLock<Option<&'static dyn Host>> = RwLock::new(None);
static DEFAULT: OnceLock<Box<dyn Host>> = OnceLock::new();

/// Installs the host, replacing any earlier one. A program does this once at
/// startup; the host lives for the rest of the process. Without one the
/// standalone host is used.
pub fn install(host: Box<dyn Host>) {
    let leaked: &'static dyn Host = Box::leak(host);
    *HOST.write().unwrap_or_else(|e| e.into_inner()) = Some(leaked);
}

pub(crate) fn current() -> &'static dyn Host {
    let installed = *HOST.read().unwrap_or_else(|e| e.into_inner());
    installed.unwrap_or_else(|| DEFAULT.get_or_init(default_host).as_ref())
}

#[cfg(test)]
fn default_host() -> Box<dyn Host> {
    Box::new(TestHost)
}

#[cfg(not(test))]
fn default_host() -> Box<dyn Host> {
    Box::new(crate::standalone::Standalone)
}

/// The host of the unit tests: the layout of the program this crate was
/// extracted from, so tests written against that layout keep their meaning.
/// Config is a nested JSON document in `<home>/.config/playbook/config.json`.
#[cfg(test)]
struct TestHost;

#[cfg(test)]
impl Host for TestHost {
    fn program(&self) -> &str {
        "playbook"
    }

    fn command(&self, args: &str) -> String {
        format!("playbook {args}")
    }

    fn setup_command(&self, part: &str) -> String {
        format!("{} setup --only {part}", self.program())
    }

    fn start_hint(&self) -> String {
        super::daemon::START_HINT.to_string()
    }

    fn launch_args(&self) -> Vec<String> {
        vec!["usage".to_string()]
    }

    fn repos_marker(&self) -> Option<&str> {
        Some("/.config/playbook/repos/")
    }

    fn home_dir(&self) -> PathBuf {
        crate::util::home_dir()
    }

    fn config_value(&self, key: &str, home: &Path) -> Option<Value> {
        let text = std::fs::read_to_string(home.join(".config/playbook/config.json")).ok()?;
        let mut node: Value = serde_json::from_str(&text).ok()?;
        for part in key.split('.') {
            node = node.get(part)?.clone();
        }
        Some(node)
    }

    fn state_dir_from(&self, home: &Path) -> PathBuf {
        crate::data::data_dir(home)
    }

    fn state_dir(&self) -> PathBuf {
        self.state_dir_from(&self.home_dir())
    }

    fn sessions(&self, _paths: &Paths, _home: &Path) -> Result<(Vec<Record>, i64), String> {
        Ok((Vec::new(), crate::util::time::now_secs()))
    }
}

/// The program name used in user facing messages.
pub fn program() -> &'static str {
    current().program()
}

/// The command line a user runs for `args`, for messages: `usage --summary`
/// becomes `playbook usage --summary`.
pub fn command(args: &str) -> String {
    current().command(args)
}

/// The command that runs one part of playbook's setup, for messages.
pub fn setup_command(part: &str) -> String {
    current().setup_command(part)
}

/// What to do while the collector is down, for messages.
pub fn start_hint() -> String {
    current().start_hint()
}

/// The arguments that make the program run usage, for a detached child.
pub fn launch_args() -> Vec<String> {
    current().launch_args()
}

/// The host's repo folder marker, if it has one.
pub fn repos_marker() -> Option<&'static str> {
    current().repos_marker()
}

/// The user's home directory.
pub fn home_dir() -> PathBuf {
    current().home_dir()
}

/// The value of config key `key`, or `None`.
pub fn config_value(key: &str, home: &Path) -> Option<Value> {
    current().config_value(key, home)
}

/// Stores config key `key` through the host, for the user at `home`.
pub fn set_config_value(key: &str, value: &Value, home: &Path) -> Result<(), String> {
    current().set_config_value(key, value, home)
}

/// [`config_value`] for the current user's home.
pub fn config_value_now(key: &str) -> Option<Value> {
    config_value(key, &home_dir())
}

/// The folder holding the usage database under `home`.
pub fn state_dir_from(home: &Path) -> PathBuf {
    current().state_dir_from(home)
}

/// The folder holding the usage database for the real environment.
pub fn state_dir() -> PathBuf {
    current().state_dir()
}

/// The sessions panel data.
pub fn load_sessions(paths: &Paths, home: &Path) -> Result<(Vec<Record>, i64), String> {
    current().sessions(paths, home)
}
