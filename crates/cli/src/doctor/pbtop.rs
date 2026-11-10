// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A separate `pbtop` binary that is older than playbook. A Homebrew `pbtop`
//! link or a `pbtop` from another install shows a dashboard of an older
//! release, so doctor names the fix, and the first run after an upgrade
//! mentions it once. The probe runs `pbtop --version`, so only doctor and the
//! first run after an upgrade call it: never a hook or the status line.

use super::check::{on_path_in, Env, Level, Row};
use crate::fastpath::PBTOP_PROGRAM;
use crate::update::is_homebrew;
use crate::update::resolve::compare;
use std::cmp::Ordering;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The script that installs `pbtop` alone.
const INSTALL_SCRIPT_FIX: &str = "curl -fsSL https://raw.githubusercontent.com/amplifiedengineer/playbook/main/install-pbtop.sh | sh";
/// The Homebrew fix when playbook came from Homebrew too: the playbook
/// formula links `pbtop`, so the separate formula goes.
const BREW_LINK_FIX: &str = "brew uninstall pbtop && brew reinstall playbook";
/// The Homebrew fix when only `pbtop` came from Homebrew.
const BREW_UPGRADE_FIX: &str = "brew upgrade pbtop";
/// Longest `pbtop --version` may run. The first command after an upgrade waits
/// for it, so it is short.
const VERSION_TIMEOUT: Duration = Duration::from_secs(2);
/// The folder of the Homebrew playbook formula's keg, which a `pbtop` link
/// resolves into.
const PLAYBOOK_KEG: &str = "/Cellar/playbook/";
/// The Homebrew fix for a `pbtop` link that belongs to an older playbook.
const BREW_PLAYBOOK_FIX: &str = "brew upgrade playbook";

/// A `pbtop` on PATH that is older than the running playbook.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Skew {
    pub path: PathBuf,
    pub version: String,
}

/// The version `bin --version` prints, or an empty string when it does not
/// answer in time or its last word is not a version. The word reaches the
/// terminal, so only digits, letters and `.+-` after a leading digit count.
fn probe_version(bin: &Path) -> String {
    let mut cmd = Command::new(bin);
    cmd.arg("--version");
    crate::common::proc::run_with_timeout(&mut cmd, VERSION_TIMEOUT)
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .and_then(|text| text.split_whitespace().next_back().map(str::to_string))
        .filter(|word| is_version(word))
        .unwrap_or_default()
}

fn is_version(word: &str) -> bool {
    word.starts_with(|c: char| c.is_ascii_digit())
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-'))
}

/// The `pbtop` first on `path_var` when `version_of` reports a version below
/// `running`. A `pbtop` that is missing, current, newer or silent gives `None`.
pub(crate) fn skew(
    path_var: &OsStr,
    running: &str,
    version_of: &dyn Fn(&Path) -> String,
) -> Option<Skew> {
    let path = on_path_in(
        path_var,
        &format!("{PBTOP_PROGRAM}{}", std::env::consts::EXE_SUFFIX),
    )?;
    let version = version_of(&path);
    (!version.is_empty() && compare(&version, running) == Ordering::Less)
        .then_some(Skew { path, version })
}

fn follow_links(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The command that brings `pbtop` to the version of the playbook at `exe`.
///
/// `resolve` follows links: a Homebrew `pbtop` is often a link in a folder such
/// as /usr/local/bin, and only the real path says which formula owns it.
pub(crate) fn fix(pbtop: &Path, exe: &Path, resolve: &dyn Fn(&Path) -> PathBuf) -> String {
    let real = resolve(pbtop);
    let brew = is_homebrew(&real) || is_homebrew(pbtop);
    if real.to_string_lossy().contains(PLAYBOOK_KEG) {
        BREW_PLAYBOOK_FIX.to_string()
    } else if brew && is_homebrew(exe) {
        BREW_LINK_FIX.to_string()
    } else if brew {
        BREW_UPGRADE_FIX.to_string()
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
    let found = skew(path_var, running, &probe_version)?;
    let exe = std::env::current_exe().unwrap_or_default();
    let fix = fix(&found.path, &exe, &follow_links);
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

    /// A link resolver for a machine with no links, so paths stay as written.
    fn as_is(path: &Path) -> PathBuf {
        path.to_path_buf()
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

        assert_eq!(found.map(|s| s.version).as_deref(), Some("0.41.0-rc.1"));
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
            &as_is,
        );

        assert_eq!(fix, "brew uninstall pbtop && brew reinstall playbook");
    }

    #[test]
    fn homebrew_pbtop_next_to_another_playbook_is_upgraded_alone() {
        let fix = fix(
            Path::new("/opt/homebrew/bin/pbtop"),
            Path::new("/home/me/.local/bin/playbook"),
            &as_is,
        );

        assert_eq!(fix, "brew upgrade pbtop");
    }

    #[test]
    fn a_pbtop_in_usr_bin_is_reinstalled_with_the_script() {
        let fix = fix(
            Path::new("/usr/bin/pbtop"),
            Path::new("/home/me/.local/bin/playbook"),
            &as_is,
        );

        assert!(fix.ends_with("install-pbtop.sh | sh"), "{fix}");
    }

    #[test]
    fn a_pbtop_link_into_the_playbook_keg_is_fixed_by_upgrading_playbook() {
        let fix = fix(
            Path::new("/opt/homebrew/Cellar/playbook/0.40.1/bin/pbtop"),
            Path::new("/home/me/.local/bin/playbook"),
            &as_is,
        );

        assert_eq!(fix, "brew upgrade playbook");
    }

    #[test]
    fn a_pbtop_link_that_resolves_into_the_playbook_keg_is_fixed_by_upgrading_playbook() {
        let into_keg = |_: &Path| PathBuf::from("/opt/homebrew/Cellar/playbook/0.40.1/bin/pbtop");

        let fix = fix(
            Path::new("/opt/homebrew/bin/pbtop"),
            Path::new("/home/me/.local/bin/playbook"),
            &into_keg,
        );

        assert_eq!(fix, "brew upgrade playbook");
    }

    #[test]
    fn only_a_version_shaped_word_is_trusted() {
        for word in ["0.40.1", "1.0.0-rc.1", "0.41.0+build5"] {
            assert!(is_version(word), "{word}");
        }
        for word in [
            "",
            "usage:",
            "\u{1b}[31m0.1.0",
            "0.1.0\u{7}",
            "v0.1.0",
            "0 1",
        ] {
            assert!(!is_version(word), "{word:?}");
        }
    }

    #[test]
    fn any_other_pbtop_is_reinstalled_with_the_script() {
        let fix = fix(
            Path::new("/home/me/.local/bin/pbtop"),
            Path::new("/home/me/.local/bin/playbook"),
            &as_is,
        );

        assert!(fix.ends_with("install-pbtop.sh | sh"), "{fix}");
    }

    /// A PATH whose `pbtop` script prints `version` the way the binary does.
    /// The helper writes, flushes and runs the script once before the caller's
    /// timed probe, so a slow first launch cannot eat the probe's short limit.
    #[cfg(unix)]
    fn path_with_script(name: &str, version: &str) -> OsString {
        let dir = scratch_dir(name);
        let script = dir.join(PBTOP_PROGRAM);
        crate::testing::write_stub(&script, &format!("#!/bin/sh\necho \"pbtop {version}\"\n"));
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
