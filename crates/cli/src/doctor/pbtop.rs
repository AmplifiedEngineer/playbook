// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A separate `pbtop` binary that is older than playbook. A Homebrew `pbtop`
//! link or a `pbtop` from another install shows a dashboard of an older
//! release, so doctor names the fix, and the first run after an upgrade
//! mentions it once. The probe runs `pbtop --version`, so only doctor and the
//! first run after an upgrade call it: never a hook or the status line.

use super::check::{on_path_in, version_of, Env, Level, Row};
use crate::fastpath::PBTOP_PROGRAM;
use crate::update::resolve::compare;
use crate::update::{is_homebrew, is_system_package};
use std::cmp::Ordering;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The script that installs `pbtop` alone.
const INSTALL_SCRIPT_FIX: &str = "curl -fsSL https://raw.githubusercontent.com/pragmatic-engineer/playbook/main/install-pbtop.sh | sh";
/// The Homebrew fix when playbook came from Homebrew too: the playbook
/// formula links `pbtop`, so the separate formula goes.
const BREW_LINK_FIX: &str = "brew uninstall pbtop && brew reinstall playbook";
/// The Homebrew fix when only `pbtop` came from Homebrew.
const BREW_UPGRADE_FIX: &str = "brew upgrade pbtop";
/// The fix for the `.deb` and `.rpm` packages, which ship both binaries.
const PACKAGE_FIX: &str =
    "install the latest playbook .deb or .rpm package, which updates both binaries";

/// A `pbtop` on PATH that is older than the running playbook.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Skew {
    pub path: PathBuf,
    pub version: String,
}

/// The `pbtop` first on `path_var` when `version_of` reports a version below
/// `running`. A `pbtop` that is missing, current, newer or silent gives `None`.
pub(crate) fn skew(
    path_var: &OsStr,
    running: &str,
    version_of: &dyn Fn(&Path) -> String,
) -> Option<Skew> {
    let path = on_path_in(path_var, PBTOP_PROGRAM)?;
    let version = version_of(&path);
    (!version.is_empty() && compare(&version, running) == Ordering::Less)
        .then_some(Skew { path, version })
}

/// The command that brings `pbtop` to the version of the playbook at `exe`.
pub(crate) fn fix(pbtop: &Path, exe: &Path) -> String {
    if is_homebrew(pbtop) && is_homebrew(exe) {
        BREW_LINK_FIX.to_string()
    } else if is_homebrew(pbtop) {
        BREW_UPGRADE_FIX.to_string()
    } else if pbtop.starts_with("/usr/bin") && is_system_package(exe) {
        PACKAGE_FIX.to_string()
    } else {
        INSTALL_SCRIPT_FIX.to_string()
    }
}

fn describe(skew: &Skew, running: &str) -> String {
    format!(
        "pbtop {} at {} is older than playbook {running}",
        skew.version,
        skew.path.display()
    )
}

fn current_skew(path_var: &OsStr) -> Option<(Skew, String)> {
    let running = env!("CARGO_PKG_VERSION");
    let found = skew(path_var, running, &version_of)?;
    let exe = std::env::current_exe().unwrap_or_default();
    let fix = fix(&found.path, &exe);
    Some((found, fix))
}

/// The doctor row, when an older `pbtop` is on PATH.
pub(crate) fn rows(env: &Env) -> Vec<Row> {
    current_skew(&env.path_var)
        .map(|(found, fix)| {
            Row::new(
                "-",
                Level::Warn,
                describe(&found, env!("CARGO_PKG_VERSION")),
            )
            .hint(format!("run: {fix}"))
        })
        .into_iter()
        .collect()
}

/// The line the first run after an upgrade adds, when an older `pbtop` is on
/// PATH.
pub(crate) fn note(path_var: &OsStr) -> Option<String> {
    current_skew(path_var).map(|(found, fix)| {
        format!(
            "{}. Run: {fix}",
            describe(&found, env!("CARGO_PKG_VERSION"))
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir as scratch_path;
    use std::ffi::OsString;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = scratch_path(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A PATH holding one folder with a `pbtop` file in it.
    fn path_with_pbtop(name: &str) -> OsString {
        let dir = scratch_dir(name);
        std::fs::write(dir.join(PBTOP_PROGRAM), "").unwrap();
        dir.into_os_string()
    }

    fn says(version: &'static str) -> impl Fn(&Path) -> String {
        move |_| version.to_string()
    }

    #[test]
    fn an_older_pbtop_is_reported_with_its_path_and_version() {
        let path = path_with_pbtop("pbtop-skew-older");

        let found = skew(&path, "0.41.0", &says("0.40.0")).unwrap();

        assert_eq!(found.version, "0.40.0");
        assert!(found.path.ends_with(PBTOP_PROGRAM), "{found:?}");
    }

    #[test]
    fn a_current_newer_or_silent_pbtop_is_not_reported() {
        let path = path_with_pbtop("pbtop-skew-fine");

        for reported in ["0.41.0", "0.42.0", "1.0.0", ""] {
            assert_eq!(skew(&path, "0.41.0", &says(reported)), None, "{reported:?}");
        }
    }

    #[test]
    fn a_pre_release_of_the_same_version_is_older() {
        let path = path_with_pbtop("pbtop-skew-pre");

        let found = skew(&path, "0.41.0", &says("0.41.0-rc.1"));

        assert!(found.is_some(), "{found:?}");
    }

    #[test]
    fn no_pbtop_on_path_asks_nothing() {
        let empty = scratch_dir("pbtop-skew-none").into_os_string();
        let never_called = |_: &Path| -> String { panic!("pbtop --version must not run") };

        assert_eq!(skew(&empty, "0.41.0", &never_called), None);
        assert_eq!(skew(OsStr::new(""), "0.41.0", &never_called), None);
    }

    #[test]
    fn a_relative_path_entry_is_not_searched() {
        let dir = scratch_dir("pbtop-skew-relative");
        std::fs::write(dir.join(PBTOP_PROGRAM), "").unwrap();
        let relative = OsString::from("bin");

        assert_eq!(skew(&relative, "0.41.0", &says("0.1.0")), None);
    }

    #[test]
    fn homebrew_pbtop_next_to_homebrew_playbook_is_replaced_by_the_playbook_link() {
        let fix = fix(
            Path::new("/opt/homebrew/bin/pbtop"),
            Path::new("/opt/homebrew/Cellar/playbook/0.41.0/bin/playbook"),
        );

        assert_eq!(fix, "brew uninstall pbtop && brew reinstall playbook");
    }

    #[test]
    fn homebrew_pbtop_next_to_another_playbook_is_upgraded_alone() {
        let fix = fix(
            Path::new("/opt/homebrew/bin/pbtop"),
            Path::new("/home/me/.local/bin/playbook"),
        );

        assert_eq!(fix, "brew upgrade pbtop");
    }

    #[test]
    fn a_packaged_pbtop_is_fixed_with_the_package() {
        let fix = fix(Path::new("/usr/bin/pbtop"), Path::new("/usr/bin/playbook"));

        assert!(fix.contains(".deb or .rpm"), "{fix}");
    }

    #[test]
    fn any_other_pbtop_is_reinstalled_with_the_script() {
        let fix = fix(
            Path::new("/home/me/.local/bin/pbtop"),
            Path::new("/home/me/.local/bin/playbook"),
        );

        assert!(fix.ends_with("install-pbtop.sh | sh"), "{fix}");
    }

    /// A PATH whose `pbtop` script prints `version` the way the binary does.
    #[cfg(unix)]
    fn path_with_script(name: &str, version: &str) -> OsString {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir(name);
        let script = dir.join(PBTOP_PROGRAM);
        std::fs::write(&script, format!("#!/bin/sh\necho \"pbtop {version}\"\n")).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        dir.into_os_string()
    }

    fn env_with(path_var: OsString) -> Env {
        Env {
            home: PathBuf::new(),
            claude_home: PathBuf::new(),
            plugin_root: None,
            shell: String::new(),
            path_var,
            repo_root: None,
            repo_slug: None,
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_row_is_a_warning_with_a_run_hint_when_pbtop_is_older() {
        let env = env_with(path_with_script("pbtop-row-old", "0.0.1"));

        let rows = rows(&env);

        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].level, Level::Warn);
        assert!(rows[0].message.contains("pbtop 0.0.1"), "{rows:?}");
        assert!(
            rows[0].message.contains(env!("CARGO_PKG_VERSION")),
            "{rows:?}"
        );
        assert!(
            rows[0]
                .hint
                .as_deref()
                .is_some_and(|h| h.starts_with("run: ")),
            "{rows:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_pbtop_at_the_running_version_gives_no_row_and_no_note() {
        let path = path_with_script("pbtop-row-same", env!("CARGO_PKG_VERSION"));

        assert!(rows(&env_with(path.clone())).is_empty());
        assert_eq!(note(&path), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_note_names_both_versions_and_the_fix() {
        let path = path_with_script("pbtop-note-old", "0.0.1");

        let text = note(&path).unwrap();

        assert!(text.starts_with("pbtop 0.0.1 at "), "{text}");
        assert!(
            text.contains(&format!(
                "older than playbook {}",
                env!("CARGO_PKG_VERSION")
            )),
            "{text}"
        );
        assert!(text.contains(". Run: "), "{text}");
    }

    #[test]
    fn the_description_names_the_path_and_both_versions() {
        let found = Skew {
            path: PathBuf::from("/opt/homebrew/bin/pbtop"),
            version: "0.40.0".into(),
        };

        let text = describe(&found, "0.41.0");

        assert_eq!(
            text,
            "pbtop 0.40.0 at /opt/homebrew/bin/pbtop is older than playbook 0.41.0"
        );
    }
}
