// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Binary tests for `playbook code lsp`. Each test gets a scratch git repo with
//! a fake `origin`, a scratch `HOME` and a scratch `PATH` that holds only git.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    repo: PathBuf,
    home: PathBuf,
    bin: PathBuf,
}

fn which_git() -> PathBuf {
    for dir in ["/usr/bin", "/opt/homebrew/bin", "/usr/local/bin", "/bin"] {
        let candidate = Path::new(dir).join("git");
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from("git")
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!(
            "playbook-code-lsp-{tag}-{}-{n}",
            playbook::testing::run_id()
        ));
        let repo = base.join("repo");
        let home = base.join("home");
        let bin = base.join("bin");
        for dir in [&repo, &home, &bin] {
            fs::create_dir_all(dir).expect("scratch dir");
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(which_git(), bin.join("git")).expect("git link");
        let fx = Self {
            repo: repo.canonicalize().expect("repo"),
            home,
            bin,
        };
        fx.git(&["init", "--quiet"]);
        fx.git(&[
            "remote",
            "add",
            "origin",
            "https://github.com/test-owner/test-repo.git",
        ]);
        fx
    }

    fn git(&self, args: &[&str]) {
        let status = Command::new(which_git())
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env_clear()
            .env("HOME", &self.home)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?}");
    }

    fn playbook(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(args)
            .current_dir(&self.repo)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", &self.bin)
            .output()
            .expect("playbook runs")
    }
}

fn json_of(out: &Output) -> serde_json::Value {
    let text = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("not one JSON document ({e}): {text}"))
}

#[test]
fn status_with_only_git_on_path_lists_four_servers_not_found() {
    // Arrange
    let fx = Fixture::new("status-empty");

    // Act
    let out = fx.playbook(&["code", "lsp", "status", "--json"]);

    // Assert
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc = json_of(&out);
    assert_eq!(doc["command"], "lsp status");
    let servers = doc["results"]["servers"].as_array().unwrap();
    assert_eq!(servers.len(), 4);
    assert!(servers.iter().all(|s| s["found"] == false), "{servers:?}");
    assert_eq!(doc["results"]["trust"]["workspace"], false);
}

#[test]
fn list_prints_every_server_with_its_license() {
    // Arrange
    let fx = Fixture::new("list");

    // Act
    let out = fx.playbook(&["code", "lsp", "list"]);

    // Assert
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for id in [
        "typescript-language-server",
        "rust-analyzer",
        "pyright",
        "gopls",
    ] {
        assert!(text.contains(id), "{text}");
    }
    assert!(text.contains("BSD-3-Clause"), "{text}");
}

#[cfg(unix)]
#[test]
fn a_server_set_through_config_shows_source_config_and_the_scripts_version() {
    // Arrange
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new("status-config");
    let script = fx.home.join("fake-ra");
    fs::write(&script, "#!/bin/sh\necho rust-analyzer 1.2.3\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let set = fx.playbook(&[
        "config",
        "set",
        "--global",
        "code.lsp.rust-analyzer.path",
        script.to_str().unwrap(),
    ]);
    assert!(
        set.status.success(),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );

    // Act
    let out = fx.playbook(&["code", "lsp", "status", "--json"]);

    // Assert
    assert!(out.status.success());
    let doc = json_of(&out);
    let ra = doc["results"]["servers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "rust-analyzer")
        .unwrap();
    assert_eq!(ra["source"], "config");
    assert_eq!(ra["version"], "rust-analyzer 1.2.3");
}

#[test]
fn status_shows_trust_and_its_tier_once_the_repo_is_trusted() {
    // Arrange
    let fx = Fixture::new("status-trust");
    let set = fx.playbook(&["config", "set", "code.lsp.trustWorkspace", "true"]);
    assert!(
        set.status.success(),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );

    // Act
    let out = fx.playbook(&["code", "lsp", "status", "--json"]);

    // Assert
    let doc = json_of(&out);
    assert_eq!(doc["results"]["trust"]["workspace"], true);
    assert_eq!(doc["results"]["trust"]["tier"], "repo");
}
