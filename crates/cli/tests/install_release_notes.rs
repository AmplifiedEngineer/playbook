// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The release notes that `install.sh` prints when it replaces an older
//! binary (ADR 0037 decision 5.4). The installer runs as the real script
//! under bash, with the shared `curl`, `uname` and `gh` stubs serving a fake
//! 0.17.0 release. `$HOME` and `$PLAYBOOK_BIN_DIR` are scratch directories
//! and every inherited `PLAYBOOK_*`, `CLAUDE_*`, `XDG_*` and `CI` variable is
//! removed.

#![cfg(unix)]

#[path = "support/install_stubs.rs"]
mod install_stubs;
#[path = "support/shell_exec.rs"]
mod shell_exec;
#[path = "support/shell_fixture.rs"]
mod shell_fixture;
#[path = "support/shell_run.rs"]
mod shell_run;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use install_stubs::{install_stubs, sha256_hex};
use shell_exec::write_exec;
use shell_fixture::Work;
use shell_run::{both, repo_root};

/// A 0.17.0 binary that knows `update --print-notes`. Every call except
/// `--version` and `version` logs its arguments.
const NEW_BINARY: &str = r#"#!/usr/bin/env bash
case "$*" in
  --version|version) echo "playbook 0.17.0"; exit 0 ;;
esac
printf '%s\n' "$*" >> "$HOME/calls.log"
case "$*" in
  "update --print-notes "*)
    if [ -f "$3" ]; then
      printf 'Breaking changes in playbook 0.17.0:\n  - The old flag is gone\nFull notes: the link\n'
    else
      printf 'Full notes: the link\n'
    fi ;;
  *) exit 0 ;;
esac"#;

/// A 0.17.0 binary from before `update --print-notes`: it rejects every
/// subcommand the way clap does.
const OLD_STYLE_BINARY: &str = r#"#!/usr/bin/env bash
case "$1" in
  --version|version) echo "playbook 0.17.0" ;;
  *) echo "error: unrecognized subcommand '$1'" >&2; exit 2 ;;
esac"#;

const NOTES: &str = r#"{"format":1,"version":"0.17.0","sections":[]}"#;

const LINK: &str =
    "Release notes: https://github.com/amplifiedengineer/playbook/releases/tag/v0.17.0";

struct Sandbox {
    work: Work,
    stubs: PathBuf,
    home: PathBuf,
    bin_dir: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let work = Work::new(&format!("install-notes-{tag}"));
        let stubs = work.dir("stubs");
        let home = work.dir("home");
        let bin_dir = home.join("bin");
        install_stubs(&stubs);
        Self {
            work,
            stubs,
            home,
            bin_dir,
        }
    }

    /// Puts an installed playbook in the bin dir that answers `--version`
    /// with `reply`.
    fn with_installed(self, reply: &str) -> Self {
        write_exec(
            &self.bin_dir.join("playbook"),
            &format!("#!/usr/bin/env bash\necho \"{reply}\"\n"),
        );
        self
    }

    /// Installs `body` as release 0.17.0 with `--no-init`, serving `notes`
    /// as release-notes.json when it is set, listed in SHA256SUMS with the
    /// hash of `notes_hash_of`. Returns stdout then stderr.
    fn install(&self, body: &str, notes: Option<&str>, notes_hash_of: &str) -> String {
        let mut sums = format!(
            "{}  playbook-0.17.0-x86_64-unknown-linux-musl\n",
            sha256_hex(body.as_bytes())
        );
        sums.push_str(&format!(
            "{}  release-notes.json\n",
            sha256_hex(notes_hash_of.as_bytes())
        ));
        let mut path = vec![self.stubs.clone()];
        path.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let mut command = Command::new("bash");
        command
            .arg(repo_root().join("install.sh"))
            .arg("--no-init")
            .current_dir(&self.work.0)
            .stdin(Stdio::null());
        for (key, _) in std::env::vars_os() {
            let key_text = key.to_string_lossy();
            if ["PLAYBOOK_", "CLAUDE_", "XDG_"]
                .iter()
                .any(|prefix| key_text.starts_with(prefix))
                || key_text == "CI"
            {
                command.env_remove(&key);
            }
        }
        command
            .env("HOME", &self.home)
            .env("CLAUDE_HOME", self.home.join(".claude"))
            .env("PLAYBOOK_BIN_DIR", &self.bin_dir)
            .env(
                "PATH",
                std::env::join_paths(path).expect("PATH should join"),
            )
            .env("SHELL", "/bin/bash")
            .env("STUB_BODY", "{\"tag_name\": \"v0.17.0\"}")
            .env("STUB_ASSET_BODY", body)
            .env("STUB_SUMS_BODY", sums);
        if let Some(notes) = notes {
            command.env("STUB_NOTES_BODY", notes);
        }
        let out = both(&command.output().expect("bash should spawn"));
        assert!(
            out.contains("Installed playbook 0.17.0"),
            "install did not reach the end of the binary step: {out}"
        );
        out
    }

    /// The calls to the installed binary that print the notes.
    fn notes_calls(&self) -> Vec<String> {
        fs::read_to_string(self.home.join("calls.log"))
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with("update --print-notes"))
            .map(str::to_string)
            .collect()
    }
}

#[test]
fn an_upgrade_prints_the_line_and_the_notes_the_new_binary_reads() {
    // Arrange
    let sandbox = Sandbox::new("upgrade").with_installed("playbook 0.16.0");

    // Act
    let out = sandbox.install(NEW_BINARY, Some(NOTES), NOTES);

    // Assert
    assert!(
        out.contains(
            "playbook updated from 0.16.0 to 0.17.0.\nBreaking changes in playbook 0.17.0:\n  - The old flag is gone\nFull notes: the link\n"
        ),
        "{out}"
    );
    assert!(!out.contains(LINK), "{out}");
    let calls = sandbox.notes_calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(
        calls[0].ends_with("/release-notes.json --from 0.16.0"),
        "{calls:?}"
    );
}

#[test]
fn a_release_without_notes_prints_the_line_and_the_link() {
    // Arrange
    let sandbox = Sandbox::new("missing").with_installed("playbook 0.16.0");

    // Act
    let out = sandbox.install(NEW_BINARY, None, NOTES);

    // Assert
    assert!(
        out.contains("playbook updated from 0.16.0 to 0.17.0.\nFull notes: the link\n"),
        "{out}"
    );
    let calls = sandbox.notes_calls();
    assert_eq!(
        calls.len(),
        1,
        "the binary still records the release: {calls:?}"
    );
    assert!(calls[0].contains("/no-release-notes.json"), "{calls:?}");
}

#[test]
fn notes_that_do_not_match_their_checksum_are_not_read() {
    // Arrange
    let sandbox = Sandbox::new("mismatch").with_installed("playbook 0.16.0");

    // Act
    let out = sandbox.install(NEW_BINARY, Some(NOTES), "other bytes");

    // Assert
    assert!(out.contains("Full notes: the link"), "{out}");
    assert!(!out.contains("Breaking changes"), "{out}");
    let calls = sandbox.notes_calls();
    assert!(
        calls.iter().all(|c| c.contains("/no-release-notes.json")),
        "{calls:?}"
    );
}

#[test]
fn a_binary_without_the_notes_flag_prints_the_link_and_no_error() {
    // Arrange
    let sandbox = Sandbox::new("old-style").with_installed("playbook 0.16.0");

    // Act
    let out = sandbox.install(OLD_STYLE_BINARY, Some(NOTES), NOTES);

    // Assert
    assert!(out.contains(LINK), "{out}");
    assert!(!out.contains("unrecognized subcommand"), "{out}");
}

#[test]
fn a_fresh_install_a_reinstall_a_downgrade_and_an_unreadable_version_print_no_line() {
    for (tag, installed) in [
        ("fresh", None),
        ("same", Some("playbook 0.17.0")),
        ("downgrade", Some("playbook 0.18.0")),
        ("unreadable", Some("error: broken!")),
    ] {
        // Arrange
        let mut sandbox = Sandbox::new(tag);
        if let Some(reply) = installed {
            sandbox = sandbox.with_installed(reply);
        }

        // Act
        let out = sandbox.install(NEW_BINARY, Some(NOTES), NOTES);

        // Assert
        assert!(!out.contains("playbook updated"), "{tag}: {out}");
        assert!(sandbox.notes_calls().is_empty(), "{tag}");
    }
}
