// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Finds a language server binary: the configured path first, then a managed
//! install, then `PATH`.

use super::install;
use super::manifest::{self, Runner};
use super::registry::ServerSpec;
use crate::host::LspSettings;
use std::ffi::OsString;
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
    /// An install made by `lsp install`.
    Managed,
    Path,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Managed => "managed",
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
    /// The arguments to start it with when they differ from the registry's, as
    /// for a managed Node server that is started as `node <entry> --stdio`.
    pub args: Option<Vec<OsString>>,
    /// The managed `tsserver.js`, for the TypeScript server.
    pub tsserver: Option<PathBuf>,
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

/// Finds `spec` with no managed install. See [`find_with`].
pub fn find(
    spec: &ServerSpec,
    settings: &LspSettings,
    path_dirs: &[PathBuf],
    probe_timeout: Duration,
) -> Result<Option<Found>, BadPath> {
    find_with(spec, settings, None, path_dirs, probe_timeout)
}

/// Finds `spec`: the configured path wins and must be valid, then a managed
/// install under `managed_root` whose version matches the pin, then the first
/// match on `path_dirs`. `Ok(None)` means no server is installed.
pub fn find_with(
    spec: &ServerSpec,
    settings: &LspSettings,
    managed_root: Option<&Path>,
    path_dirs: &[PathBuf],
    probe_timeout: Duration,
) -> Result<Option<Found>, BadPath> {
    if let Some(configured) = settings.server_path(spec.id) {
        check_executable(configured)?;
        return Ok(Some(found_at(
            configured.to_path_buf(),
            Source::Config,
            probe_timeout,
        )));
    }
    if let Some(found) = managed_root.and_then(|root| managed(spec, root, path_dirs)) {
        return Ok(Some(found));
    }
    Ok(search_path(spec.executable, path_dirs)
        .map(|hit| found_at(hit, Source::Path, probe_timeout)))
}

fn found_at(path: PathBuf, source: Source, probe_timeout: Duration) -> Found {
    let version = probe_version(&path, probe_timeout);
    Found {
        path,
        source,
        version,
        args: None,
        tsserver: None,
    }
}

/// The install of the current pin, if complete. A published folder always has
/// its receipt, so a folder without one is ignored.
fn managed(spec: &ServerSpec, root: &Path, path_dirs: &[PathBuf]) -> Option<Found> {
    let pin = manifest::pin_for(spec.id)?;
    let dir = install::version_dir(root, pin);
    if !dir.join("install.json").is_file() {
        return None;
    }
    let release = pin.releases.first()?;
    let entry = dir.join(release.component).join(release.entry);
    let extra = spec.args.iter().map(OsString::from);
    let (path, args) = match release.runner {
        Runner::Direct => (entry, None),
        Runner::Node => {
            let node = search_path("node", path_dirs)?;
            let args = std::iter::once(entry.into_os_string())
                .chain(extra)
                .collect();
            (node, Some(args))
        }
    };
    let tsserver = pin
        .releases
        .iter()
        .find(|r| r.component == "typescript")
        .map(|r| dir.join(r.component).join(r.entry));
    Some(Found {
        path,
        source: Source::Managed,
        version: release.version.to_string(),
        args,
        tsserver,
    })
}

/// Installed versions of the server that are not the current pin.
pub fn old_versions(spec: &ServerSpec, root: &Path) -> Vec<String> {
    let Some(pin) = manifest::pin_for(spec.id) else {
        return Vec::new();
    };
    let current = pin.releases[0].version;
    let mut old: Vec<String> = std::fs::read_dir(root.join(spec.id))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name != current)
        .collect();
    old.sort();
    old
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
        // A loaded machine can cut the probe off before the shell records its
        // pid, so retry with a longer limit until it did.
        let mut version = String::new();
        for seconds in [1, 3, 10] {
            version = probe_version(&blocker, Duration::from_secs(seconds));
            if pidfile.exists() {
                break;
            }
        }

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

    /// A published managed install of the current pin, with a `node` on a PATH dir.
    fn managed_install(tag: &str, id: &str) -> (PathBuf, PathBuf) {
        let root = scratch(&format!("{tag}-lsp"));
        let bin = scratch(&format!("{tag}-bin"));
        script(&bin, "node", "exit 0");
        let pin = manifest::pin_for(id).unwrap();
        let dir = install::version_dir(&root, pin);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("install.json"), "{}").unwrap();
        (root, bin)
    }

    #[test]
    fn a_managed_install_of_the_current_pin_beats_path_and_loses_to_config() {
        // Arrange
        let (root, bin) = managed_install("managed-prec", "pyright");
        script(&bin, "pyright-langserver", "echo from-path");
        let spec = registry::by_id("pyright").unwrap();
        let dirs = [bin];

        // Act
        let managed = find_with(spec, &LspSettings::default(), Some(&root), &dirs, PATIENT)
            .unwrap()
            .unwrap();
        let configured = script(&scratch("managed-cfg"), "pl", "echo configured");
        let config = find_with(
            spec,
            &settings_with("pyright", &configured),
            Some(&root),
            &dirs,
            PATIENT,
        )
        .unwrap()
        .unwrap();

        // Assert
        assert_eq!(managed.source, Source::Managed);
        assert_eq!(
            managed.version,
            manifest::pin_for("pyright").unwrap().releases[0].version
        );
        let args = managed.args.unwrap();
        assert!(
            args[0]
                .to_string_lossy()
                .ends_with("pyright/package/langserver.index.js"),
            "{args:?}"
        );
        assert_eq!(args[1], "--stdio");
        assert_eq!(config.source, Source::Config);
    }

    #[test]
    fn a_leftover_older_version_is_ignored_and_listed() {
        // Arrange
        let (root, bin) = managed_install("managed-old", "pyright");
        let pin = manifest::pin_for("pyright").unwrap();
        std::fs::remove_dir_all(install::version_dir(&root, pin)).unwrap();
        std::fs::create_dir_all(root.join("pyright/1.0.0")).unwrap();
        let spec = registry::by_id("pyright").unwrap();

        // Act
        let got = find_with(spec, &LspSettings::default(), Some(&root), &[bin], PATIENT).unwrap();

        // Assert
        assert_eq!(got, None);
        assert_eq!(old_versions(spec, &root), ["1.0.0"]);
    }

    #[test]
    fn the_typescript_server_reports_its_managed_companion() {
        // Arrange
        let (root, bin) = managed_install("managed-ts", "typescript-language-server");
        let spec = registry::by_id("typescript-language-server").unwrap();

        // Act
        let found = find_with(spec, &LspSettings::default(), Some(&root), &[bin], PATIENT)
            .unwrap()
            .unwrap();

        // Assert
        let tsserver = found.tsserver.unwrap();
        assert!(
            tsserver.ends_with("typescript/package/lib/tsserver.js"),
            "{tsserver:?}"
        );
    }

    #[test]
    fn a_managed_node_server_without_node_on_path_is_not_found() {
        // Arrange
        let (root, _) = managed_install("managed-nonode", "pyright");
        let spec = registry::by_id("pyright").unwrap();

        // Act
        let got = find_with(
            spec,
            &LspSettings::default(),
            Some(&root),
            &[scratch("no-node")],
            PATIENT,
        )
        .unwrap();

        // Assert
        assert_eq!(got, None);
    }
}
