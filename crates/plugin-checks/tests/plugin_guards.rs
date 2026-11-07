// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Runs `scripts/check-plugin.sh` and `scripts/bump-version.sh` against
//! small throwaway repositories: the payload guard rejects a file that is not
//! markdown, the version guard asks the binary and the plugin to share one
//! version (ADR 0038), and the bump script sets it everywhere.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Repo {
    dir: PathBuf,
}

impl Repo {
    fn new() -> Repo {
        assert!(
            Command::new("jq").arg("--version").output().is_ok(),
            "these checks need jq on PATH"
        );
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("plugin-guards-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("scripts")).expect("scratch repo");
        for name in ["version-lib.sh", "check-plugin.sh", "bump-version.sh"] {
            let script = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../scripts")
                .join(name);
            fs::copy(script, dir.join("scripts").join(name)).expect("copy the script");
        }
        let repo = Repo { dir };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.manifest("1.0.0", "1.0.0");
        repo.write("plugins/playbook/commands/fix.md", "one\n");
        repo.write(
            "Cargo.toml",
            "[workspace]\nresolver = \"2\"\n\n[workspace.package]\nversion = \"1.0.0\"\nedition = \"2021\"\n\n[workspace.dependencies]\nversion = \"9.9.9\"\n",
        );
        repo.write(
            "Cargo.lock",
            "[[package]]\nname = \"other\"\nversion = \"1.0.0\"\n\n[[package]]\nname = \"playbook\"\nversion = \"1.0.0\"\n\n[[package]]\nname = \"playbook-code\"\nversion = \"1.0.0\"\n\n[[package]]\nname = \"playbook-telemetry\"\nversion = \"1.0.0\"\n\n[[package]]\nname = \"code-bench\"\nversion = \"1.0.0\"\n",
        );
        for name in [
            "playbook",
            "playbook-code",
            "playbook-telemetry",
            "code-bench",
        ] {
            repo.write(
                &format!("crates/{name}/Cargo.toml"),
                &format!("[package]\nname = \"{name}\"\nversion.workspace = true\n"),
            );
        }
        repo.write(
            "crates/other/Cargo.toml",
            "[package]\nname = \"other\"\nversion = \"1.0.0\"\n",
        );
        repo.write("README.md", "Pin with `PLAYBOOK_REF=v1.0.0`.\n");
        repo.write(
            "docs/guides/00-install.md",
            "curl x | PLAYBOOK_REF=v1.0.0 bash\n",
        );
        repo.commit("base");
        repo
    }

    fn manifest(&self, version: &str, min: &str) {
        self.write(
            "plugins/playbook/.claude-plugin/plugin.json",
            &format!(
                r#"{{"name":"playbook","version":"{version}","metadata":{{"minBinaryVersion":"{min}"}}}}"#
            ),
        );
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.dir.join(rel)).expect("read")
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.dir.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        fs::write(path, text).expect("write");
    }

    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .current_dir(&self.dir)
            .output()
            .expect("git");
        assert!(out.status.success(), "{args:?}: {}", text_of(&out));
    }

    fn commit(&self, msg: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", msg]);
    }

    fn run(&self, args: &[&str]) -> Output {
        self.script("check-plugin.sh", args)
    }

    fn script(&self, name: &str, args: &[&str]) -> Output {
        Command::new("bash")
            .arg(format!("scripts/{name}"))
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .current_dir(&self.dir)
            .output()
            .expect("bash")
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn text_of(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn the_payload_guard_passes_for_markdown_and_the_manifest() {
    let r = Repo::new();

    let out = r.run(&["payload"]);

    assert!(out.status.success(), "{}", text_of(&out));
}

#[test]
fn the_payload_guard_rejects_a_file_that_is_not_markdown() {
    let r = Repo::new();
    r.write("plugins/playbook/skills/x/run.sh", "echo hi\n");
    r.commit("script");

    let out = r.run(&["payload"]);

    assert!(!out.status.success());
    assert!(text_of(&out).contains("run.sh"), "{}", text_of(&out));
}

#[test]
fn the_same_version_everywhere_passes() {
    let r = Repo::new();

    let out = r.run(&["version"]);

    assert!(out.status.success(), "{}", text_of(&out));
}

#[test]
fn a_plugin_version_or_minimum_that_differs_from_cargo_fails() {
    // The plugin is off, the minimum is off, or both agree with each other
    // and Cargo.toml is the one that differs.
    for (version, min) in [("1.0.1", "1.0.0"), ("1.0.0", "0.9.0"), ("1.1.0", "1.1.0")] {
        // Arrange
        let r = Repo::new();
        r.manifest(version, min);

        // Act
        let out = r.run(&["version"]);

        // Assert
        assert!(!out.status.success(), "{version} {min}");
        assert!(
            text_of(&out).contains("run scripts/bump-version.sh"),
            "{}",
            text_of(&out)
        );
    }
}

#[test]
fn the_minimum_binary_is_the_first_release_of_the_minor_line() {
    // Arrange
    let r = Repo::new();
    r.write("Cargo.toml", "[workspace.package]\nversion = \"1.0.2\"\n");

    // Act
    r.manifest("1.0.2", "1.0.2");
    let full = r.run(&["version"]);
    r.manifest("1.0.2", "1.0.0");
    let floor = r.run(&["version"]);

    // Assert
    assert!(
        text_of(&full).contains("minBinaryVersion 1.0.0"),
        "{}",
        text_of(&full)
    );
    assert!(!full.status.success());
    assert!(floor.status.success(), "{}", text_of(&floor));
}

#[test]
fn the_bump_script_sets_one_version_everywhere() {
    // Arrange
    let r = Repo::new();

    // Act
    let out = r.script("bump-version.sh", &["1.1.3"]);

    // Assert
    assert!(out.status.success(), "{}", text_of(&out));
    assert!(r.run(&["version"]).status.success());
    let cargo = r.read("Cargo.toml");
    assert!(
        cargo.contains("[workspace.package]\nversion = \"1.1.3\""),
        "{cargo}"
    );
    assert!(
        cargo.contains("[workspace.dependencies]\nversion = \"9.9.9\""),
        "{cargo}"
    );
    let lock = r.read("Cargo.lock");
    assert_eq!(
        lock.matches("version = \"1.1.3\"").count(),
        4,
        "only the crates that take the workspace version move"
    );
    assert!(
        lock.contains("name = \"code-bench\"\nversion = \"1.1.3\"")
            && lock.contains("name = \"other\"\nversion = \"1.0.0\""),
        "{lock}"
    );
    let manifest = r.read("plugins/playbook/.claude-plugin/plugin.json");
    assert!(manifest.contains("\"version\": \"1.1.3\""), "{manifest}");
    assert!(
        manifest.contains("\"minBinaryVersion\": \"1.1.0\""),
        "{manifest}"
    );
    assert!(r.read("README.md").contains("PLAYBOOK_REF=v1.1.3"));
    assert!(r
        .read("docs/guides/00-install.md")
        .contains("PLAYBOOK_REF=v1.1.3"));
}

#[test]
fn the_bump_script_refuses_a_version_that_does_not_rise() {
    for (version, plugin, reason) in [
        ("1.0.0", "1.0.0", "is not above the current version 1.0.0"),
        ("0.9.9", "1.0.0", "is not above the current version 1.0.0"),
        ("1.1", "1.0.0", "is not MAJOR.MINOR.PATCH"),
        ("1.1.0", "1.2.0", "is not above the plugin version 1.2.0"),
    ] {
        // Arrange
        let r = Repo::new();
        r.manifest(plugin, "1.0.0");

        // Act
        let out = r.script("bump-version.sh", &[version]);

        // Assert
        assert!(!out.status.success(), "{version}");
        assert!(
            text_of(&out).contains(reason),
            "{version}: {}",
            text_of(&out)
        );
        assert!(r.read("Cargo.toml").contains("version = \"1.0.0\""));
    }
}

#[test]
fn the_bump_script_changes_nothing_when_a_pin_is_missing() {
    // Arrange
    let r = Repo::new();
    r.write("README.md", "No pin here.\n");

    // Act
    let out = r.script("bump-version.sh", &["1.1.0"]);

    // Assert
    assert!(!out.status.success());
    assert!(
        text_of(&out).contains("README.md has no PLAYBOOK_REF"),
        "{}",
        text_of(&out)
    );
    assert!(r.read("Cargo.toml").contains("version = \"1.0.0\""));
    assert!(r
        .read("plugins/playbook/.claude-plugin/plugin.json")
        .contains("\"version\":\"1.0.0\""));
}

#[test]
fn the_payload_guard_rejects_a_symlink_named_like_markdown() {
    let r = Repo::new();
    std::os::unix::fs::symlink(
        "/etc/hosts",
        r.dir.join("plugins/playbook/commands/link.md"),
    )
    .expect("symlink");
    r.commit("link");

    let out = r.run(&["payload"]);

    assert!(!out.status.success());
    assert!(text_of(&out).contains("plain file"), "{}", text_of(&out));
}

#[test]
fn the_payload_guard_rejects_markdown_nested_too_deep() {
    let r = Repo::new();
    r.write("plugins/playbook/commands/a/b.md", "x\n");
    r.commit("deep");

    let out = r.run(&["payload"]);

    assert!(!out.status.success());
}

#[test]
fn the_payload_guard_accepts_a_path_with_non_ascii_letters() {
    let r = Repo::new();
    r.write("plugins/playbook/skills/caf\u{e9}/SKILL.md", "x\n");
    r.commit("accent");

    let out = r.run(&["payload"]);

    assert!(out.status.success(), "{}", text_of(&out));
}

#[test]
fn an_invalid_manifest_is_reported() {
    let r = Repo::new();
    r.write("plugins/playbook/.claude-plugin/plugin.json", "{not json");
    r.commit("broken");

    let out = r.run(&["version"]);

    assert!(!out.status.success());
}
