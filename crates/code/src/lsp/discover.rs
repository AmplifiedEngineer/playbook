// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Finds a language server binary: the configured path first, then `PATH`.

use super::registry::ServerSpec;
use crate::host::LspSettings;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// How long a `--version` probe may run.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

const UNKNOWN_VERSION: &str = "unknown";

/// Where a server binary came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The `code.lsp.<id>.path` setting.
    Config,
    Path,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Path => "PATH",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    pub source: Source,
    /// The first line `--version` printed, or `unknown`.
    pub version: String,
}

/// A configured path that cannot be a server.
#[derive(Debug, PartialEq, Eq)]
pub enum BadPath {
    NotAbsolute(PathBuf),
    NotRegularFile(PathBuf),
    NotExecutable(PathBuf),
}

impl fmt::Display for BadPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAbsolute(p) => write!(f, "{} is not an absolute path", p.display()),
            Self::NotRegularFile(p) => write!(f, "{} is not a regular file", p.display()),
            Self::NotExecutable(p) => write!(f, "{} is not executable", p.display()),
        }
    }
}

impl std::error::Error for BadPath {}

/// Finds `spec`: the configured path wins and must be valid, else the first
/// match on `path_dirs`. `Ok(None)` means no server is installed.
pub fn find(
    spec: &ServerSpec,
    settings: &LspSettings,
    path_dirs: &[PathBuf],
    probe_timeout: Duration,
) -> Result<Option<Found>, BadPath> {
    let (path, source) = if let Some(configured) = settings.server_path(spec.id) {
        check_executable(configured)?;
        (configured.to_path_buf(), Source::Config)
    } else if let Some(hit) = search_path(spec.executable, path_dirs) {
        (hit, Source::Path)
    } else {
        return Ok(None);
    };
    let version = probe_version(&path, probe_timeout);
    Ok(Some(Found {
        path,
        source,
        version,
    }))
}

fn check_executable(path: &Path) -> Result<(), BadPath> {
    if !path.is_absolute() {
        return Err(BadPath::NotAbsolute(path.to_path_buf()));
    }
    let meta = std::fs::metadata(path).map_err(|_| BadPath::NotRegularFile(path.to_path_buf()))?;
    if !meta.is_file() {
        return Err(BadPath::NotRegularFile(path.to_path_buf()));
    }
    if !is_executable(&meta) {
        return Err(BadPath::NotExecutable(path.to_path_buf()));
    }
    Ok(())
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    true
}

/// The first absolute directory in `dirs` that holds a runnable `name`.
pub fn search_path(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| check_executable(candidate).is_ok())
}

/// The directories on the process `PATH`.
pub fn process_path() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect())
        .unwrap_or_default()
}

/// Runs `path --version` from the system temp dir, so a repo cannot steer it,
/// and returns the first output line. Anything slow or failing is `unknown`.
pub fn probe_version(path: &Path, timeout: Duration) -> String {
    let mut command = Command::new(path);
    command.arg("--version").current_dir(std::env::temp_dir());
    playbook_common::proc::run_with_timeout(&mut command, timeout)
        .filter(|out| out.status.success())
        .and_then(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| UNKNOWN_VERSION.to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::lsp::registry;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(tag: &str) -> PathBuf {
        let dir = playbook_common::test_support::scratch_dir(tag);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn settings_with(id: &str, path: &Path) -> LspSettings {
        LspSettings {
            server_paths: vec![(id.to_string(), path.to_path_buf())],
            ..LspSettings::default()
        }
    }

    const QUICK: Duration = Duration::from_millis(100);
    const PATIENT: Duration = Duration::from_secs(10);

    #[test]
    fn a_config_path_to_a_directory_is_rejected() {
        // Arrange
        let dir = scratch("cfg-dir");
        let spec = registry::by_id("rust-analyzer").unwrap();

        // Act
        let got = find(spec, &settings_with("rust-analyzer", &dir), &[], QUICK);

        // Assert
        let err = got.unwrap_err();
        assert!(matches!(err, BadPath::NotRegularFile(_)));
        assert!(err.to_string().contains("not a regular file"), "{err}");
    }

    #[test]
    fn a_relative_or_non_executable_config_path_is_rejected() {
        // Arrange
        let dir = scratch("cfg-bad");
        let plain = dir.join("plain");
        std::fs::write(&plain, "x").unwrap();
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
        let spec = registry::by_id("gopls").unwrap();

        // Act
        let relative = find(
            spec,
            &settings_with("gopls", Path::new("gopls")),
            &[],
            QUICK,
        );
        let plain = find(spec, &settings_with("gopls", &plain), &[], QUICK);

        // Assert
        assert!(matches!(relative, Err(BadPath::NotAbsolute(_))));
        assert!(matches!(plain, Err(BadPath::NotExecutable(_))));
    }

    #[test]
    fn the_config_path_wins_over_a_path_hit() {
        // Arrange
        let configured = script(&scratch("cfg-wins-a"), "gopls", "echo configured 1.0");
        let on_path = scratch("cfg-wins-b");
        script(&on_path, "gopls", "echo from-path 2.0");
        let spec = registry::by_id("gopls").unwrap();

        // Act
        let found = find(
            spec,
            &settings_with("gopls", &configured),
            &[on_path],
            PATIENT,
        )
        .unwrap()
        .unwrap();

        // Assert
        assert_eq!(found.source, Source::Config);
        assert_eq!(found.path, configured);
        assert_eq!(found.version, "configured 1.0");
    }

    #[test]
    fn without_config_the_first_path_directory_with_the_binary_is_used() {
        // Arrange
        let empty = scratch("path-empty");
        let first = scratch("path-first");
        let second = scratch("path-second");
        script(&first, "gopls", "echo first");
        script(&second, "gopls", "echo second");
        let spec = registry::by_id("gopls").unwrap();

        // Act
        let found = find(
            spec,
            &LspSettings::default(),
            &[empty, first, second],
            PATIENT,
        )
        .unwrap()
        .unwrap();

        // Assert
        assert_eq!(found.source, Source::Path);
        assert_eq!(found.version, "first");
    }

    #[test]
    fn no_config_and_no_path_hit_finds_nothing() {
        // Arrange
        let spec = registry::by_id("gopls").unwrap();

        // Act
        let found = find(
            spec,
            &LspSettings::default(),
            &[scratch("path-none")],
            QUICK,
        );

        // Assert
        assert_eq!(found, Ok(None));
    }

    #[test]
    fn a_probe_that_blocks_yields_unknown_and_the_child_is_gone() {
        // Arrange
        let dir = scratch("probe-block");
        let pidfile = dir.join("pid");
        let blocker = script(
            &dir,
            "blocker",
            &format!("echo $$ > '{}'; exec sleep 300", pidfile.display()),
        );

        // Act
        // Long enough for the shell to record its pid before the probe is cut off.
        let version = probe_version(&blocker, Duration::from_secs(1));

        // Assert
        assert_eq!(version, "unknown");
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let pid = rustix::process::Pid::from_raw(pid).unwrap();
        assert!(rustix::process::test_kill_process(pid).is_err());
    }

    #[test]
    fn a_probe_that_fails_yields_unknown() {
        // Arrange
        let failing = script(&scratch("probe-fail"), "bad", "echo nope; exit 2");

        // Act
        let version = probe_version(&failing, PATIENT);

        // Assert
        assert_eq!(version, "unknown");
    }
}
