// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The previous-binary backup that `install.sh` keeps when it replaces an
//! existing `$PLAYBOOK_BIN_DIR/playbook`, and `playbook uninstall --remove-binary`
//! removing those backups. The installer runs as the real script under bash.
//!
//! A stub `curl` serves a fake release, a stub `uname` pins the platform and a
//! stub `gh` stands in for the attestation check, all ahead of the real tools
//! on a scratch `PATH`, so the shipped functions
//! run unmodified. Every install runs with `--no-init`, so it ends once the
//! binary is placed; installs are checked for their `Installed playbook` line. `$HOME` and
//! `$PLAYBOOK_BIN_DIR` are scratch directories and every inherited
//! `PLAYBOOK_*`, `CLAUDE_*`, `XDG_*` and `CI` variable is removed, so a
//! developer's real install can never be read or modified.

#![cfg(unix)]

use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

const CURL_STUB: &str = r#"#!/usr/bin/env bash
out=""; url=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    -w) shift 2 ;;
    -*) shift ;;
    *)  url="$1"; shift ;;
  esac
done
emit() { if [ -n "$out" ]; then printf '%s' "$1" > "$out"; else printf '%s' "$1"; fi; }
case "$url" in
  *api.github.com/repos/*/releases/latest)
    emit "$STUB_BODY"
    if [ -n "$out" ]; then printf '200'; fi
    exit 0 ;;
  */releases/download/*/SHA256SUMS) emit "$STUB_SUMS_BODY"; exit 0 ;;
  */releases/download/*)            emit "$STUB_ASSET_BODY"; exit 0 ;;
  *) exit 22 ;;
esac
"#;

const UNAME_STUB: &str = r#"#!/usr/bin/env bash
case "$1" in
  -s) printf 'Linux\n' ;;
  -m) printf 'x86_64\n' ;;
  *) command -p uname "$@" ;;
esac
"#;

/// Plays the attestation check without a network: `gh attestation` exists and
/// finds no attestation, which the installer reports as a gap and goes on. The
/// real `gh` on the machine would call the GitHub API, so the outcome would
/// depend on its login, its rate limit and the network.
const GH_STUB: &str = r#"#!/usr/bin/env bash
[ "$1 $2" = "attestation --help" ] && exit 0
echo "no attestations found for subject" >&2
exit 1
"#;

/// Refuses to write the backup temp file and defers to the real `cp` for
/// everything else, so only the backup step fails.
const FAILING_CP_STUB: &str = r#"#!/usr/bin/env bash
for a in "$@"; do
  case "$a" in *.playbook.bak.*) exit 1 ;; esac
done
exec /bin/cp "$@"
"#;

/// A scratch `$HOME`, bin dir, and stub tools, removed again on drop.
struct Sandbox {
    root: PathBuf,
    stubs: PathBuf,
    failing_cp: PathBuf,
    home: PathBuf,
    bin_dir: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let n = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "playbook-install-binary-backup-{}-{tag}-{n}",
            playbook::testing::run_id()
        ));
        let stubs = root.join("stubs");
        let failing_cp = root.join("failing-cp");
        let home = root.join("home");
        let bin_dir = home.join("bin");
        write_executable(&stubs.join("curl"), CURL_STUB);
        write_executable(&stubs.join("uname"), UNAME_STUB);
        write_executable(&stubs.join("gh"), GH_STUB);
        write_executable(&failing_cp.join("cp"), FAILING_CP_STUB);
        fs::create_dir_all(&home).expect("scratch home should be creatable");
        Self {
            root,
            stubs,
            failing_cp,
            home,
            bin_dir,
        }
    }

    fn playbook(&self) -> PathBuf {
        self.bin_dir.join("playbook")
    }

    fn backup(&self, version: &str) -> PathBuf {
        self.bin_dir.join(format!("playbook.{version}.bak"))
    }

    /// File names of every `playbook.*.bak` in the bin dir, sorted.
    fn backups(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.bin_dir)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.starts_with("playbook.") && n.ends_with(".bak"))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Names of the bin-dir entries that start with `prefix`.
    fn leftovers(&self, prefix: &str) -> Vec<String> {
        fs::read_dir(&self.bin_dir)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.starts_with(prefix))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Installs a fake release `version`, returning stdout then stderr. Fails
    /// the test unless the run got as far as announcing the new binary, so a
    /// run that died early cannot pass on its absence of side effects.
    fn install(&self, version: &str) -> String {
        self.install_with_path_prefix(version, None)
    }

    /// Like `install`, with `path_prefix` placed ahead of the stub tools.
    fn install_with_path_prefix(&self, version: &str, path_prefix: Option<&Path>) -> String {
        let body = fake_binary_body(version);
        let sums = format!(
            "{}  playbook-{version}-x86_64-unknown-linux-musl",
            sha256_hex(&body)
        );
        let mut path = vec![self.stubs.clone()];
        if let Some(prefix) = path_prefix {
            path.insert(0, prefix.to_path_buf());
        }
        path.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let mut command = self.script_command("install.sh");
        command
            .arg("--no-init")
            .env(
                "PATH",
                std::env::join_paths(path).expect("PATH should join"),
            )
            .env("SHELL", "/bin/bash")
            .env("STUB_BODY", format!("{{\"tag_name\": \"v{version}\"}}"))
            .env("STUB_ASSET_BODY", body)
            .env("STUB_SUMS_BODY", sums);
        let out = combined_output(&mut command);
        assert!(
            out.contains(&format!("Installed playbook {version}")),
            "install did not reach the end of the binary step: {out}"
        );
        out
    }

    fn uninstall(&self) {
        let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(["uninstall", "--yes", "--remove-binary"])
            .env("HOME", &self.home)
            .env("PLAYBOOK_BIN_DIR", &self.bin_dir)
            .env("SHELL", "/bin/bash")
            .current_dir(&self.root)
            .output()
            .expect("playbook should spawn");
        assert!(out.status.success(), "uninstall exited with {}", out.status);
    }

    /// What the installed binary prints for `--version`.
    fn installed_version(&self) -> String {
        let out = Command::new(self.playbook())
            .arg("--version")
            .output()
            .expect("installed binary should run");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn script_command(&self, script: &str) -> Command {
        let mut command = Command::new("bash");
        command
            .arg(Path::new(env!("PLAYBOOK_REPO_ROOT")).join(script))
            .current_dir(&self.root);
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
            .env("PLAYBOOK_BIN_DIR", &self.bin_dir);
        command
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn write_executable(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).expect("parent dir should be creatable");
    fs::write(path, content).expect("scratch file should be writable");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .expect("scratch file should be chmod-able");
}

/// Body of a fake release binary that prints `playbook <version>`. It has no
/// trailing newline so the served bytes and the checksummed bytes match.
fn fake_binary_body(version: &str) -> String {
    format!("#!/usr/bin/env bash\necho \"playbook {version}\"")
}

/// SHA-256 via the same tools `install.sh` verifies with: `shasum` on macOS,
/// `sha256sum` on Linux.
fn sha256_hex(content: &str) -> String {
    let (program, args): (&str, &[&str]) =
        if Command::new("shasum").arg("--version").output().is_ok() {
            ("shasum", &["-a", "256"])
        } else {
            ("sha256sum", &[])
        };
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("a sha256 tool should spawn");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(content.as_bytes())
        .expect("sha256 tool should accept input");
    let out = child.wait_with_output().expect("sha256 tool should finish");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .expect("sha256 tool should print a digest")
        .to_string()
}

fn combined_output(command: &mut Command) -> String {
    let out = command.output().expect("bash should spawn");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Gives the backup for `version` a fixed mtime `hours_after_2020` hours into
/// 2020, so ordering by age never depends on how fast the installs
/// ran.
fn set_backup_age(sandbox: &Sandbox, version: &str, hours_after_2020: u64) {
    const JAN_1_2020: u64 = 1_577_836_800;
    let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(JAN_1_2020 + hours_after_2020 * 3600);
    File::options()
        .write(true)
        .open(sandbox.backup(version))
        .and_then(|f| f.set_modified(mtime))
        .expect("backup mtime should be settable");
}

#[test]
fn first_install_leaves_no_backup() {
    // Arrange
    let sandbox = Sandbox::new("first");

    // Act
    sandbox.install("1.1.0");

    // Assert
    assert!(sandbox.playbook().is_file(), "binary should be installed");
    assert_eq!(sandbox.backups(), Vec::<String>::new());
}

#[test]
fn reinstall_keeps_the_old_binary_as_a_versioned_backup() {
    // Arrange
    let sandbox = Sandbox::new("reinstall");
    sandbox.install("1.1.0");
    let old_bytes = fs::read(sandbox.playbook()).unwrap();

    // Act
    let out = sandbox.install("1.2.3");

    // Assert
    let bak = sandbox.backup("1.1.0");
    assert_eq!(
        fs::read(&bak).unwrap(),
        old_bytes,
        "backup is byte-identical"
    );
    assert_eq!(
        fs::metadata(&bak).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(sandbox.installed_version(), "playbook 1.2.3");
    let rollback = format!("mv -f {} {}", bak.display(), sandbox.playbook().display());
    assert!(out.contains(&rollback), "rollback hint missing from: {out}");
}

#[test]
fn an_existing_same_version_backup_is_kept() {
    // Arrange
    let sandbox = Sandbox::new("existing");
    sandbox.install("1.1.0");
    fs::write(sandbox.backup("1.1.0"), "KEEP").unwrap();

    // Act
    sandbox.install("1.2.3");

    // Assert
    assert_eq!(fs::read_to_string(sandbox.backup("1.1.0")).unwrap(), "KEEP");
    assert_eq!(sandbox.installed_version(), "playbook 1.2.3");
}

#[test]
fn only_the_newest_three_backups_are_kept() {
    // Arrange
    let sandbox = Sandbox::new("prune");
    // 1.2.0 sorts after 1.10.0 and 1.11.0 by name but is the newest backup, and
    // 1.9.0 sorts last by name but is the oldest, so only an mtime ordering
    // keeps the right three.
    let versions = ["1.9.0", "1.10.0", "1.11.0", "1.2.0", "1.3.0"];
    sandbox.install(versions[0]);

    // Act: each upgrade backs up the previous version, then that backup is
    // aged so later ones are strictly newer.
    for (i, pair) in versions.windows(2).enumerate() {
        sandbox.install(pair[1]);
        set_backup_age(&sandbox, pair[0], i as u64 + 1);
    }

    // Assert
    assert_eq!(
        sandbox.backups(),
        [
            "playbook.1.10.0.bak",
            "playbook.1.11.0.bak",
            "playbook.1.2.0.bak"
        ]
    );
}

#[test]
fn an_unrunnable_old_binary_gets_an_unknown_backup() {
    // Arrange
    let sandbox = Sandbox::new("unknown");
    fs::create_dir_all(&sandbox.bin_dir).unwrap();
    write_executable(&sandbox.playbook(), "#!/bin/sh\nexit 1\n");
    let broken_bytes = fs::read(sandbox.playbook()).unwrap();

    // Act
    sandbox.install("1.2.3");

    // Assert
    let backups = sandbox.backups();
    assert_eq!(backups.len(), 1, "expected one backup, got {backups:?}");
    assert!(backups[0].starts_with("playbook.unknown-"), "{backups:?}");
    assert_eq!(
        fs::read(sandbox.bin_dir.join(&backups[0])).unwrap(),
        broken_bytes
    );
    assert_eq!(sandbox.installed_version(), "playbook 1.2.3");
}

#[test]
fn a_failing_backup_warns_and_still_installs() {
    // Arrange
    let sandbox = Sandbox::new("failing");
    sandbox.install("1.1.0");

    // Act
    let out = sandbox.install_with_path_prefix("1.2.3", Some(&sandbox.failing_cp));

    // Assert
    assert!(
        out.contains("could not back up the previous playbook binary"),
        "expected the backup warning in: {out}"
    );
    assert!(out.contains("Installed playbook 1.2.3"), "{out}");
    assert_eq!(sandbox.installed_version(), "playbook 1.2.3");
    assert_eq!(sandbox.backups(), Vec::<String>::new());
    assert_eq!(
        sandbox.leftovers(".playbook.bak."),
        Vec::<String>::new(),
        "no half-written backup should remain"
    );
}

#[test]
fn an_identical_reinstall_makes_no_backup() {
    // Arrange
    let sandbox = Sandbox::new("identical");
    sandbox.install("1.2.3");

    // Act
    let out = sandbox.install("1.2.3");

    // Assert
    assert!(out.contains("Installed playbook 1.2.3"), "{out}");
    assert_eq!(sandbox.backups(), Vec::<String>::new());
}

#[test]
fn uninstall_removes_the_backups() {
    // Arrange
    let sandbox = Sandbox::new("uninstall");
    sandbox.install("1.1.0");
    sandbox.install("1.2.3");
    assert_eq!(sandbox.backups().len(), 1, "precondition: one backup");

    // Act
    sandbox.uninstall();

    // Assert
    assert!(!sandbox.playbook().exists(), "binary should be removed");
    assert_eq!(sandbox.backups(), Vec::<String>::new());
}

#[test]
fn install_warns_when_an_earlier_playbook_on_path_hides_the_new_one() {
    // Arrange
    let sandbox = Sandbox::new("shadow");
    let stale = sandbox.root.join("stale");
    write_executable(
        &stale.join("playbook"),
        "#!/bin/sh\necho 'playbook 0.14.0'\n",
    );

    // Act
    let out = sandbox.install_with_path_prefix("1.1.0", Some(&stale));

    // Assert
    let first = stale.join("playbook");
    assert!(
        out.contains(&format!("{} runs before the new", first.display())),
        "shadow warning missing from: {out}"
    );
}

#[test]
fn install_stays_quiet_when_the_new_binary_is_first_on_path() {
    // Arrange
    let sandbox = Sandbox::new("noshadow");

    // Act
    let out = sandbox.install_with_path_prefix("1.1.0", Some(&sandbox.bin_dir));

    // Assert
    assert!(
        !out.contains("runs before the new"),
        "unexpected warning: {out}"
    );
}
