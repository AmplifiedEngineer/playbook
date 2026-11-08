// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `install.sh` places the binary and one PATH marker per shell, hands the
//! right flags to `playbook init`, and copes with a release binary older than
//! the script. Removal is `playbook uninstall`, covered by tests/uninstall.rs.

#[path = "support/install_stubs.rs"]
mod install_stubs;
#[path = "support/shell_exec.rs"]
mod shell_exec;
#[path = "support/shell_fixture.rs"]
mod shell_fixture;
#[path = "support/shell_run.rs"]
mod shell_run;

use install_stubs::{install_stubs, sha256_hex};
use shell_exec::write_exec;
use shell_fixture::Work;
use shell_run::{both, repo_root};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const RELEASE_BODY: &str = r#"{"tag_name": "v1.2.3"}"#;
const ASSET: &str = "playbook-1.2.3-x86_64-unknown-linux-musl";
const GOOD_ASSET: &str = "#!/usr/bin/env bash\necho \"playbook 1.2.3\"\n";
const MARKER: &str = "# playbook binary";

/// Install through the stubbed network path (`--no-init`), which ends once the
/// binary and PATH work are done, all these scenarios look at.
fn binary_install(work: &Work, stubs: &Path, home: &Path, bindir: &Path, shell: &str) {
    let sums = format!("{}  {ASSET}", sha256_hex(GOOD_ASSET.as_bytes()));
    let path = format!(
        "{}:{}",
        stubs.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let _ = work;
    let out = Command::new("bash")
        .arg(repo_root().join("install.sh"))
        .arg("--no-init")
        .env("PATH", path)
        .env("CLAUDE_HOME", home.join(".claude"))
        .env("HOME", home)
        .env("PLAYBOOK_BIN_DIR", bindir)
        .env("SHELL", shell)
        .env("STUB_CODE", "200")
        .env("STUB_BODY", RELEASE_BODY)
        .env("STUB_ASSET_BODY", GOOD_ASSET)
        .env("STUB_SUMS_BODY", sums)
        .output()
        .expect("bash spawns");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success() && text.contains("Installed playbook"),
        "{text}"
    );
}

fn markers(file: &Path) -> usize {
    fs::read_to_string(file)
        .map(|t| t.matches(MARKER).count())
        .unwrap_or(0)
}

struct Net {
    work: Work,
    stubs: PathBuf,
}

fn net(tag: &str) -> Net {
    let work = Work::new(tag);
    let stubs = work.dir("stubs");
    install_stubs(&stubs);
    Net { work, stubs }
}

#[test]
fn install_places_the_binary_and_exactly_one_path_marker() {
    let n = net("bp-one");
    let home = n.work.dir("home");
    let bindir = n.work.path("bindir");
    binary_install(&n.work, &n.stubs, &home, &bindir, "/bin/bash");
    assert!(bindir.join("playbook").exists());
    assert_eq!(markers(&home.join(".bashrc")), 1);
}

#[test]
fn installing_twice_leaves_exactly_one_path_marker() {
    let n = net("bp-twice");
    let home = n.work.dir("home");
    let bindir = n.work.path("bindir");
    binary_install(&n.work, &n.stubs, &home, &bindir, "/bin/bash");
    binary_install(&n.work, &n.stubs, &home, &bindir, "/bin/bash");
    assert_eq!(markers(&home.join(".bashrc")), 1);
}

#[test]
fn zsh_gets_one_path_block_in_zshenv_and_nothing_in_zshrc() {
    let n = net("bp-zsh");
    let home = n.work.dir("home");
    let bindir = n.work.path("zsh-dir");
    binary_install(&n.work, &n.stubs, &home, &bindir, "/bin/zsh");
    binary_install(&n.work, &n.stubs, &home, &bindir, "/bin/zsh");
    let zshenv = fs::read_to_string(home.join(".zshenv")).unwrap_or_default();
    assert_eq!(markers(&home.join(".zshenv")), 1, "{zshenv}");
    assert!(!home.join(".zshrc").exists());
    assert!(
        zshenv.contains(&format!("export PATH=\"{}:$PATH\"", bindir.display())),
        "{zshenv}"
    );
}

#[test]
fn the_bash_path_block_goes_into_the_existing_login_file() {
    let n = net("bp-login");
    let home = n.work.dir("home");
    fs::write(home.join(".profile"), "# mine\n").unwrap();
    binary_install(
        &n.work,
        &n.stubs,
        &home,
        &n.work.path("login-dir"),
        "/bin/bash",
    );
    assert_eq!(markers(&home.join(".profile")), 1);
    assert!(
        !home.join(".bash_profile").exists(),
        "a shadowing login file was created"
    );
}

#[test]
fn fish_gets_a_conf_d_file_with_fish_add_path() {
    let n = net("bp-fish");
    let home = n.work.dir("home");
    let bindir = n.work.path("fish-dir");
    binary_install(&n.work, &n.stubs, &home, &bindir, "/usr/bin/fish");
    let file =
        fs::read_to_string(home.join(".config/fish/conf.d/playbook.fish")).unwrap_or_default();
    assert!(
        file.contains(&format!("fish_add_path -g \"{}\"", bindir.display())),
        "{file}"
    );
}

/// Run the installer with `PLAYBOOK_SKIP_DOWNLOAD=1` and a stub `playbook` in
/// the bin dir (no network path).
fn local_install(
    work: &Work,
    stub_dir: &Path,
    flags: &[&str],
    tag: &str,
) -> (i32, PathBuf, String) {
    let home = work.dir(&format!("home-{tag}"));
    let out = Command::new("bash")
        .arg(repo_root().join("install.sh"))
        .args(flags)
        .env("PLAYBOOK_SKIP_DOWNLOAD", "1")
        .env("CLAUDE_HOME", home.join(".claude"))
        .env("HOME", &home)
        .env("PLAYBOOK_BIN_DIR", stub_dir)
        .env("SHELL", "/bin/bash")
        .output()
        .unwrap();
    (out.status.code().unwrap_or(-1), home, both(&out))
}

#[test]
fn install_succeeds_against_a_release_binary_that_predates_an_optional_init_flag() {
    let work = Work::new("bp-strict");
    let stub = work.dir("strict-init-bin");
    write_exec(
        &stub.join("playbook"),
        r#"case "${1:-}" in
  --version) printf 'playbook 0.10.0\n'; exit 0 ;;
  init)
    shift
    if [ "${1:-}" = "--help" ]; then
      printf 'Install or repair the local Claude Code configuration\n\nUsage: playbook init\n'
      exit 0
    fi
    if [ $# -gt 0 ]; then
      printf "error: unexpected argument '%s' found\n\nUsage: playbook init\n" "$1" >&2
      exit 2
    fi
    home="${CLAUDE_HOME:-$HOME/.claude}"
    mkdir -p "$home"
    [ -f "$home/settings.json" ] || printf '{}\n' > "$home/settings.json"
    [ -f "$home/.settings.base.json" ] || printf '{}\n' > "$home/.settings.base.json"
    printf 'settings: wired - seeded from template\n'
    printf 'guards: ok - already in place\n'
    printf 'hooks: ok - all hooks already wired\n'
    printf 'shim: skipped - $SHELL is neither bash nor zsh\n'
    printf 'statusline: ok - already up to date\n'
    printf 'system-prompt: skipped - not installed; pass --system-prompt to opt in\n'
    exit 0
    ;;
esac
exit 0
"#,
    );
    let (rc, home, out) = local_install(&work, &stub, &["--yes"], "strict");
    assert_eq!(rc, 0, "{out}");
    assert!(home.join(".claude/settings.json").exists(), "{out}");
}

fn recording_stub(work: &Work) -> (PathBuf, PathBuf) {
    let dir = work.dir("recording-init-bin");
    let argv = work.path("init-argv.txt");
    write_exec(
        &dir.join("playbook"),
        &format!(
            r#"case "${{1:-}}" in
  --version) printf 'playbook 0.0.0-stub\n' ;;
  init)
    shift
    printf '%s\n' "$@" > "{}"
    if [ "${{1:-}}" = "--help" ]; then
      printf -- '--system-prompt\n--no-system-prompt\n'
      exit 0
    fi
    home="${{CLAUDE_HOME:-$HOME/.claude}}"
    [ -f "$home/settings.json" ] || printf '{{}}\n' > "$home/settings.json"
    [ -f "$home/.settings.base.json" ] || printf '{{}}\n' > "$home/.settings.base.json"
    printf 'settings: wired - seeded from template\n'
    printf 'guards: ok - already in place\n'
    printf 'hooks: ok - all hooks already wired\n'
    printf 'shim: ok - installed\n'
    printf 'statusline: ok - already up to date\n'
    printf 'system-prompt: ok - installed\n'
    ;;
esac
exit 0
"#,
            argv.display()
        ),
    );
    (dir, argv)
}

#[test]
fn a_binary_with_setup_runs_setup_and_never_init() {
    let work = Work::new("bp-setup");
    let dir = work.dir("setup-bin");
    let argv = work.path("setup-argv.txt");
    let init_calls = work.path("init-calls.txt");
    write_exec(
        &dir.join("playbook"),
        &format!(
            r#"case "${{1:-}}" in
  --version) printf 'playbook 9.9.9\n' ;;
  setup)
    shift
    if [ "${{1:-}}" = "--help" ]; then
      printf 'Usage: playbook setup [OPTIONS]\n  --system-prompt\n  --yes\n'
      exit 0
    fi
    printf '%s\n' "$@" > "{argv}"
    home="${{CLAUDE_HOME:-$HOME/.claude}}"
    mkdir -p "$home"
    printf '{{}}\n' > "$home/settings.json"
    ;;
  init) printf 'init\n' >> "{init}" ;;
esac
exit 0
"#,
            argv = argv.display(),
            init = init_calls.display()
        ),
    );
    let (rc, home, log) = local_install(&work, &dir, &["--yes", "--system-prompt"], "setup");
    assert_eq!(rc, 0, "{log}");
    assert!(log.contains("Running playbook setup"), "{log}");
    assert!(home.join(".claude/settings.json").exists(), "{log}");
    let got = fs::read_to_string(&argv).unwrap_or_default();
    assert!(
        got.contains("--system-prompt") && got.contains("--yes"),
        "setup received: '{got}'"
    );
    assert!(
        !init_calls.exists(),
        "install.sh ran the old init name: {log}"
    );
}

#[test]
fn the_system_prompt_flag_is_forwarded_and_the_retired_aliases_flag_is_not() {
    let work = Work::new("bp-forward");
    let (stub, argv) = recording_stub(&work);
    let (rc, _, log) = local_install(
        &work,
        &stub,
        &["--yes", "--aliases", "--system-prompt"],
        "fwd",
    );
    assert_eq!(rc, 0, "{log}");
    assert!(log.contains("--aliases is retired"), "{log}");
    let got = fs::read_to_string(&argv).unwrap_or_default();
    assert!(!got.contains("--aliases"), "init received: '{got}'");
    assert!(got.contains("--system-prompt"), "init received: '{got}'");
}

fn init_args(argv: &Path) -> Vec<String> {
    fs::read_to_string(argv)
        .expect("the stub records the init arguments")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_no_system_prompt_flag_is_forwarded() {
    let work = Work::new("bp-no-prompt");
    let (stub, argv) = recording_stub(&work);
    let (rc, _, log) = local_install(&work, &stub, &["--yes", "--no-system-prompt"], "nsp");
    assert_eq!(rc, 0, "{log}");
    let got = init_args(&argv);
    assert!(
        got.iter().any(|a| a == "--no-system-prompt"),
        "init received: {got:?}"
    );
    assert!(
        !got.iter().any(|a| a == "--system-prompt"),
        "init received: {got:?}"
    );
}

#[test]
fn yes_alone_passes_no_prompt_flag_so_setup_decides() {
    let work = Work::new("bp-yes-alone");
    let (stub, argv) = recording_stub(&work);
    let (rc, _, log) = local_install(&work, &stub, &["--yes"], "ya");
    assert_eq!(rc, 0, "{log}");
    let got = init_args(&argv);
    assert!(
        !got.iter()
            .any(|a| a == "--system-prompt" || a == "--no-system-prompt"),
        "init received: {got:?}"
    );
}

#[test]
fn binary_only_keeps_the_system_prompt_off() {
    let work = Work::new("bp-binonly");
    let (stub, argv) = recording_stub(&work);
    let (rc, _, log) = local_install(&work, &stub, &["--yes", "--binary-only"], "bo");
    assert_eq!(rc, 0, "{log}");
    assert!(log.contains("Running playbook init"), "{log}");
    let got = init_args(&argv);
    assert!(
        got.iter().any(|a| a == "--no-system-prompt"),
        "init received: {got:?}"
    );
    assert!(
        !got.iter()
            .any(|a| a == "--aliases" || a == "--system-prompt"),
        "init received: {got:?}"
    );
}

#[test]
fn flags_from_older_commands_are_accepted_and_ignored() {
    let work = Work::new("bp-legacy");
    let (stub, argv) = recording_stub(&work);
    let flags = [
        "--no-init",
        "--skip-plugin",
        "--no-setup",
        "--skip-deps",
        "--skip-shell",
    ];
    let (rc, _, log) = local_install(&work, &stub, &flags, "legacy");
    assert_eq!(rc, 0, "{log}");
    assert!(!argv.exists(), "init ran: {log}");
}

#[test]
fn skills_only_is_refused_with_the_plugin_commands() {
    let work = Work::new("bp-skills-only");
    let (stub, _) = recording_stub(&work);
    let (rc, _, log) = local_install(&work, &stub, &["--skills-only"], "so");
    assert_ne!(rc, 0, "{log}");
    assert!(log.contains("--skills-only is gone"), "{log}");
    assert!(
        log.contains("claude plugin marketplace add amplifiedengineer/marketplace"),
        "{log}"
    );
    assert!(
        log.contains("claude plugin install playbook@amplifiedengineer"),
        "{log}"
    );
}

/// Returns `cmd` run with no controlling terminal, as `curl | bash` does
/// under cron or in CI, so a prompt has nobody to ask. std has no safe setsid,
/// so perl, which macOS and Linux both ship, starts a session and then execs.
fn without_terminal(cmd: &Command) -> Command {
    let mut wrapped = Command::new("perl");
    wrapped
        .args(["-MPOSIX", "-e", "POSIX::setsid(); exec @ARGV or die $!"])
        .arg(cmd.get_program())
        .args(cmd.get_args());
    for (key, value) in cmd.get_envs() {
        match value {
            Some(value) => wrapped.env(key, value),
            None => wrapped.env_remove(key),
        };
    }
    wrapped
}

fn installer(work: &Work, stub: &Path, flags: &[&str], tag: &str) -> Command {
    let home = work.dir(&format!("home-{tag}"));
    let mut cmd = Command::new("bash");
    cmd.arg(repo_root().join("install.sh"))
        .args(flags)
        .env("PLAYBOOK_SKIP_DOWNLOAD", "1")
        .env("HOME", &home)
        .env("PLAYBOOK_BIN_DIR", stub)
        .env("SHELL", "/bin/bash");
    cmd
}

#[test]
fn with_no_yes_and_no_terminal_init_is_not_run_and_its_command_is_printed() {
    let work = Work::new("bp-noconsent");
    let (stub, argv) = recording_stub(&work);

    let cmd = installer(&work, &stub, &[], "nc");
    let out = without_terminal(&cmd).output().unwrap();

    let text = both(&out);
    assert!(out.status.success(), "{text}");
    assert!(!argv.exists(), "init ran without a yes: {text}");
    assert!(text.contains("Run: playbook init "), "{text}");
}

#[test]
fn a_requested_system_prompt_is_kept_in_the_printed_init_command() {
    let work = Work::new("bp-noconsent-flags");
    let (stub, argv) = recording_stub(&work);

    let cmd = installer(&work, &stub, &["--system-prompt"], "ncf");
    let out = without_terminal(&cmd).output().unwrap();

    let text = both(&out);
    assert!(out.status.success(), "{text}");
    assert!(!argv.exists(), "init ran without a yes: {text}");
    assert!(
        text.contains("Run: playbook init --system-prompt "),
        "{text}"
    );
}

#[test]
fn no_init_skips_init_even_with_yes() {
    let work = Work::new("bp-noinit");
    let (stub, argv) = recording_stub(&work);

    let out = installer(&work, &stub, &["--yes", "--no-init"], "ni")
        .output()
        .unwrap();

    let text = both(&out);
    assert!(out.status.success(), "{text}");
    assert!(!argv.exists(), "init ran: {text}");
    assert!(text.contains("Run: playbook init "), "{text}");
}

#[test]
fn the_installer_never_touches_the_claude_plugins() {
    let work = Work::new("bp-noplugin");
    let (stub, _) = recording_stub(&work);
    let calls = work.path("claude-calls.txt");
    write_exec(
        &stub.join("claude"),
        &format!("printf '%s\\n' \"$*\" >> \"{}\"\n", calls.display()),
    );
    let path = format!(
        "{}:{}",
        stub.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let out = installer(&work, &stub, &["--yes"], "np")
        .env("PATH", path)
        .output()
        .unwrap();

    let text = both(&out);
    assert!(out.status.success(), "{text}");
    assert!(!calls.exists(), "claude was called: {text}");
}
