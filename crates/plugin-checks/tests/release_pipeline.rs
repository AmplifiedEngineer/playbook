// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Runs the release pipeline scripts (ADR 0039) against small throwaway
//! repositories: `release-plan.sh` picks the next step, and
//! `release-prepare.sh` writes the release pull request.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

const SCRIPTS: [&str; 6] = [
    "version-lib.sh",
    "check-plugin.sh",
    "bump-version.sh",
    "release-notes.sh",
    "release-plan.sh",
    "release-prepare.sh",
];

struct Repo {
    dir: PathBuf,
}

impl Repo {
    /// A repository whose files carry `version`, with the tags `tags`.
    fn new(version: &str, tags: &[&str]) -> Repo {
        assert!(
            Command::new("jq").arg("--version").output().is_ok(),
            "these checks need jq on PATH"
        );
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("release-pipeline-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("scripts")).expect("scratch repo");
        for name in SCRIPTS {
            let script = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../scripts")
                .join(name);
            fs::copy(script, dir.join("scripts").join(name)).expect("copy the script");
        }
        let repo = Repo { dir };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.set_version(version);
        repo.write(
            "Cargo.lock",
            &format!("[[package]]\nname = \"playbook\"\nversion = \"{version}\"\n"),
        );
        repo.write(
            "crates/cli/Cargo.toml",
            "[package]\nname = \"playbook\"\nversion.workspace = true\n",
        );
        repo.write("README.md", &format!("Pin `PLAYBOOK_REF=v{version}`.\n"));
        repo.write(
            "docs/guides/00-install.md",
            &format!("PLAYBOOK_REF=v{version} bash\n"),
        );
        repo.write(
            "CHANGELOG.md",
            "# Changelog\n\nThe header.\n\n## playbook 0.9.0\n\n- Old notes.\n",
        );
        repo.commit("chore: start");
        for tag in tags {
            repo.git(&["tag", tag]);
        }
        repo
    }

    fn set_version(&self, version: &str) {
        let (minor, _) = version.rsplit_once('.').expect("a version");
        self.write(
            "Cargo.toml",
            &format!("[workspace.package]\nversion = \"{version}\"\n"),
        );
        self.write(
            "plugins/playbook/.claude-plugin/plugin.json",
            &format!(
                r#"{{"name":"playbook","version":"{version}","metadata":{{"minBinaryVersion":"{minor}.0"}}}}"#
            ),
        );
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.dir.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        fs::write(path, text).expect("write");
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.dir.join(rel)).expect("read")
    }

    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
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

    fn script(&self, name: &str, args: &[&str]) -> Output {
        Command::new("bash")
            .arg(format!("scripts/{name}"))
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GITHUB_REPOSITORY", "o/r")
            .current_dir(&self.dir)
            .output()
            .expect("bash")
    }

    fn plan(&self, want: &str) -> (bool, String) {
        let out = self.script("release-plan.sh", &[want]);
        (out.status.success(), text_of(&out))
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
fn a_bump_after_a_release_plans_the_next_version() {
    // Arrange
    let r = Repo::new("1.2.3", &["v1.2.3", "v1.2.2", "telemetry-v9.0.0"]);

    // Act and assert
    for (want, version) in [
        ("patch", "1.2.4"),
        ("minor", "1.3.0"),
        ("major", "2.0.0"),
        ("1.5.0", "1.5.0"),
    ] {
        let (ok, out) = r.plan(want);
        assert!(ok, "{want}: {out}");
        assert!(
            out.contains(&format!(
                "version={version}\ntag=v{version}\nprevious=1.2.3\nstate=prepare\n"
            )),
            "{want}: {out}"
        );
    }
}

#[test]
fn the_latest_release_can_be_resumed_and_an_older_one_cannot() {
    // Arrange
    let r = Repo::new("1.2.3", &["v1.2.3", "v1.2.2"]);

    // Act
    let latest = r.plan("1.2.3");
    let older = r.plan("1.2.2");
    let below = r.plan("1.2.0");

    // Assert
    assert!(
        latest.0 && latest.1.contains("state=publish"),
        "{}",
        latest.1
    );
    assert!(
        !older.0 && older.1.contains("last release is v1.2.3"),
        "{}",
        older.1
    );
    assert!(!below.0 && below.1.contains("is not above"), "{}", below.1);
}

#[test]
fn files_above_the_last_tag_are_the_release_to_finish() {
    // Arrange
    let r = Repo::new("1.4.0", &["v1.2.3"]);

    // Act
    let bump = r.plan("patch");
    let named = r.plan("1.4.0");
    let other = r.plan("1.5.0");
    r.git(&["tag", "v1.4.0"]);
    let tagged = r.plan("minor");

    // Assert
    assert!(bump.0, "{}", bump.1);
    assert!(
        bump.1
            .contains("version=1.4.0\ntag=v1.4.0\nprevious=1.2.3\nstate=tag\n"),
        "{}",
        bump.1
    );
    assert!(bump.1.contains("'patch' is ignored"), "{}", bump.1);
    assert!(
        bump.1.contains("\ncommit="),
        "the release commit to tag: {}",
        bump.1
    );
    assert!(named.0 && named.1.contains("state=tag"), "{}", named.1);
    assert!(
        !other.0 && other.1.contains("Finish 1.4.0 first"),
        "{}",
        other.1
    );
    assert!(tagged.0, "{}", tagged.1);
    assert!(
        tagged
            .1
            .contains("version=1.5.0\ntag=v1.5.0\nprevious=1.4.0\nstate=prepare\n"),
        "{}",
        tagged.1
    );
}

#[test]
fn the_first_release_starts_from_no_tag_and_bad_input_is_refused() {
    // Arrange
    let r = Repo::new("0.1.0", &[]);

    // Act
    let first = r.plan("0.1.0");
    let word = r.plan("huge");
    r.write("Cargo.toml", "[workspace.package]\nversion = \"0.2.0\"\n");
    let split = r.plan("patch");

    // Assert
    assert!(
        first.0 && first.1.contains("previous=0.0.0\nstate=tag"),
        "{}",
        first.1
    );
    assert!(
        !word.0 && word.1.contains("is not patch, minor, major"),
        "{}",
        word.1
    );
    assert!(
        !split.0 && split.1.contains("versions in the checkout disagree"),
        "{}",
        split.1
    );
}

#[test]
fn prepare_bumps_the_files_and_adds_the_changelog_section_once() {
    // Arrange
    let r = Repo::new("1.0.0", &["v1.0.0"]);
    r.write("crates/cli/src/lib.rs", "// new\n");
    r.commit("feat(cli): add a flag (#5)");

    // Act
    let first = r.script("release-prepare.sh", &["1.1.0"]);
    let changelog = r.read("CHANGELOG.md");
    let again = r.script("release-prepare.sh", &["1.1.0"]);

    // Assert
    assert!(first.status.success(), "{}", text_of(&first));
    assert!(
        changelog.contains(
            "The header.\n\n## playbook 1.1.0\n\n### Features\n\n- **cli**: add a flag ([#5](https://github.com/o/r/pull/5))\n"
        ),
        "{changelog}"
    );
    assert!(changelog.contains("\n## playbook 0.9.0\n"), "{changelog}");
    assert!(!changelog.contains("Full changelog"), "{changelog}");
    assert!(r.read("Cargo.toml").contains("version = \"1.1.0\""));
    assert!(again.status.success(), "{}", text_of(&again));
    assert!(text_of(&again).contains("already carries 1.1.0"));
    assert_eq!(r.read("CHANGELOG.md"), changelog);
}

#[test]
fn prepare_after_a_bump_that_stopped_only_adds_the_changelog_section() {
    // Arrange: the bump ran, and the changelog step did not.
    let r = Repo::new("1.0.0", &["v1.0.0"]);
    r.set_version("1.1.0");

    // Act
    let out = r.script("release-prepare.sh", &["1.1.0"]);

    // Assert
    assert!(out.status.success(), "{}", text_of(&out));
    assert!(r.read("CHANGELOG.md").contains("\n## playbook 1.1.0\n"));
    assert!(r.read("Cargo.toml").contains("version = \"1.1.0\""));
}

const MIGRATIONS: &str = "crates/cli/src/init/migrate.rs";

fn registry_with(sinces: &[&str]) -> String {
    sinces
        .iter()
        .map(|s| format!("        Migration {{\n            since: {s},\n        }},\n"))
        .collect()
}

#[test]
fn prepare_writes_the_release_version_in_place_of_next() {
    // Arrange
    let r = Repo::new("1.0.0", &["v1.0.0"]);
    r.write(
        MIGRATIONS,
        &registry_with(&[r#""0.9.0""#, r#""next""#, r#""next""#]),
    );
    r.commit("feat(migrate): add a migration (#6)");

    // Act
    let out = r.script("release-prepare.sh", &["1.1.0"]);

    // Assert
    assert!(out.status.success(), "{}", text_of(&out));
    assert_eq!(
        r.read(MIGRATIONS),
        registry_with(&[r#""0.9.0""#, r#""1.1.0""#, r#""1.1.0""#])
    );
}

#[test]
fn a_resumed_prepare_still_fills_in_next() {
    // Arrange: the bump and the changelog section are in place, and a
    // migration still says next.
    let r = Repo::new("1.0.0", &["v1.0.0"]);
    r.set_version("1.1.0");
    r.write(
        "CHANGELOG.md",
        "# Changelog\n\n## playbook 1.1.0\n\n- New.\n",
    );
    r.write(MIGRATIONS, &registry_with(&[r#""next""#]));

    // Act
    let out = r.script("release-prepare.sh", &["1.1.0"]);

    // Assert
    assert!(out.status.success(), "{}", text_of(&out));
    assert!(text_of(&out).contains("already carries 1.1.0"));
    assert_eq!(r.read(MIGRATIONS), registry_with(&[r#""1.1.0""#]));
}

#[test]
fn prepare_fails_when_a_next_is_left() {
    // Arrange: a `next` in a form the fill-in does not match.
    let r = Repo::new("1.0.0", &["v1.0.0"]);
    r.write(MIGRATIONS, "            since:  \"next\",\n");

    // Act
    let out = r.script("release-prepare.sh", &["1.1.0"]);

    // Assert
    assert!(!out.status.success());
    assert!(
        text_of(&out).contains("could not fill in"),
        "{}",
        text_of(&out)
    );
    assert!(r.read("Cargo.toml").contains("version = \"1.0.0\""));
}

#[test]
fn prepare_refuses_a_version_that_is_not_plain() {
    // Arrange
    let r = Repo::new("1.0.0", &["v1.0.0"]);
    r.write(MIGRATIONS, &registry_with(&[r#""next""#]));

    // Act
    let out = r.script("release-prepare.sh", &["1.1.0/x"]);

    // Assert
    assert!(!out.status.success());
    assert_eq!(r.read(MIGRATIONS), registry_with(&[r#""next""#]));
}

#[test]
fn prepare_refuses_a_version_that_is_already_tagged() {
    // Arrange: 1.0.0 is out, so writing it over next would be wrong.
    let r = Repo::new("1.0.0", &["v1.0.0"]);
    r.write(MIGRATIONS, &registry_with(&[r#""next""#]));

    // Act
    let out = r.script("release-prepare.sh", &["1.0.0"]);

    // Assert
    assert!(!out.status.success());
    assert!(
        text_of(&out).contains("already released"),
        "{}",
        text_of(&out)
    );
    assert_eq!(r.read(MIGRATIONS), registry_with(&[r#""next""#]));
}
