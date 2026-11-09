// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook upgrade`: fetch a release binary, verify it (SHA256SUMS, then the
//! build attestation), show its release notes, swap it in with a backup,
//! then check PATH.
//! Install.sh and this share one policy; see `verify`.

pub mod auto;
pub mod check;
pub mod download;
pub mod pathcheck;
pub mod resolve;
pub mod style;
pub mod swap;
pub mod tarball;
pub mod verify;

#[cfg(all(test, unix))]
pub(crate) use tarball::pbdoc_fixture;

use download::Fetcher;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use verify::{Attest, REPO};

/// What the user asked for on the command line.
#[derive(Debug, Default, Clone)]
pub struct Options {
    pub version: Option<String>,
    pub check: bool,
    pub list: bool,
    pub pre: bool,
    pub yes: bool,
}

/// Everything the update reads from the machine, so tests can fake it.
pub struct Env {
    pub exe: PathBuf,
    pub path_var: OsString,
    pub auto_mode: bool,
    pub strict: bool,
    pub windows: bool,
    pub os: String,
    pub arch: String,
    pub current_version: String,
    pub api_base: String,
    pub download_base: String,
    /// The playbook folder, where the release notes are recorded as shown.
    pub root: PathBuf,
    /// Shows text to the user at once, before the swap: the release notes.
    pub announce: fn(&str),
    /// Asks the user to confirm an upgrade, with the question as its text.
    pub confirm: fn(&str) -> Reply,
}

/// What the user said to the confirmation of an upgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    Yes,
    No,
    /// Nobody could answer: stdin or stderr is not a terminal.
    NoTerminal,
}

/// Asks `question` on the terminal. An empty line or anything but a yes
/// counts as no, since an upgrade replaces the running binary.
pub fn ask_on_terminal(question: &str) -> Reply {
    use crate::ui::wizard::{parse_answer, Answer};
    use std::io::{BufRead, IsTerminal, Write};
    let terminal = crate::plugin_update::assumed_tty(true)
        || (std::io::stdin().is_terminal() && std::io::stderr().is_terminal());
    if !terminal {
        return Reply::NoTerminal;
    }
    eprint!("{question}");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(n) if n > 0 && parse_answer(&line, false) == Answer::Yes => Reply::Yes,
        _ => Reply::No,
    }
}

/// How an upgrade run ended, as a stable word for `--json`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// `--list` printed the releases.
    #[default]
    Listed,
    /// This binary is the latest release, or newer.
    Current,
    /// `--check` found a newer release.
    Available,
    /// A Homebrew install: the binary stays, and the output names the brew
    /// command.
    Homebrew,
    /// A newer release exists and nobody could confirm it.
    NoTerminal,
    /// The user said no.
    Declined,
    /// The binary was replaced.
    Upgraded,
}

impl Status {
    pub fn word(self) -> &'static str {
        match self {
            Status::Listed => "listed",
            Status::Current => "current",
            Status::Available => "available",
            Status::Homebrew => "homebrew",
            Status::NoTerminal => "no-terminal",
            Status::Declined => "declined",
            Status::Upgraded => "upgraded",
        }
    }
}

impl Env {
    /// The real machine. The running binary is canonicalised so a symlink is
    /// replaced at its target, not turned into a regular file.
    pub fn real() -> Result<Env, String> {
        let exe = std::env::current_exe()
            .map_err(|e| format!("cannot locate the running binary: {e}"))?;
        Ok(Env {
            exe: std::fs::canonicalize(&exe).unwrap_or(exe),
            path_var: std::env::var_os("PATH").unwrap_or_default(),
            auto_mode: crate::common::mode::resolve_for_hook().mode
                == crate::common::mode::Mode::Auto,
            strict: std::env::var("PLAYBOOK_REQUIRE_ATTESTATION").is_ok_and(|v| v == "1"),
            windows: cfg!(windows),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            api_base: api_base(),
            download_base: format!("https://github.com/{REPO}/releases/download"),
            root: crate::common::paths::playbook_root(),
            announce: |text| eprint!("{text}"),
            confirm: ask_on_terminal,
        })
    }
}

/// The GitHub API base for the release repo.
pub fn api_base() -> String {
    format!("https://api.github.com/repos/{REPO}")
}

/// Lines to print: `out` to stdout, `warnings` to stderr.
#[derive(Debug, Default)]
pub struct Outcome {
    pub out: Vec<String>,
    pub warnings: Vec<String>,
    /// The version swapped in, when the update replaced the binary.
    pub installed: Option<String>,
    /// The release notes of an upgrade (ADR 0037), shown before the swap.
    /// Empty for any other run.
    pub notes: String,
    /// Whether `notes` held the release's own notes, which the user saw, so
    /// the heal need not show the release again. False when only the link
    /// was shown.
    pub notes_shown: bool,
    /// How the run ended.
    pub status: Status,
    /// The latest release the run found, when it looked.
    pub latest: Option<String>,
}

/// The `--json` report of an upgrade run: the shared envelope with the
/// versions, the status word, the output lines and the plugin steps. It
/// exits 0 whatever the result, so a failure sets `ok` false and `error`.
pub fn json_report(
    result: &Result<Outcome, String>,
    plugin: Option<&crate::plugin_update::Ensured>,
) -> serde_json::Value {
    let plugin_json = plugin.map(crate::plugin_update::Ensured::json);
    match result {
        Ok(o) => crate::json::envelope::wrap(
            0,
            "",
            serde_json::json!({
                "status": o.status.word(),
                "current": env!("CARGO_PKG_VERSION"),
                "installed": o.installed,
                "latest": o.latest,
                "lines": o.out,
                "warnings": o.warnings,
                "plugin": plugin_json,
            }),
        ),
        Err(err) => crate::json::envelope::wrap(
            1,
            err,
            serde_json::json!({ "current": env!("CARGO_PKG_VERSION"), "plugin": plugin_json }),
        ),
    }
}

/// The first release whose binary has `doctor --fix`; `migrate` comes in the
/// same or a later one. The `-0` sorts below every 0.27.0 pre-release, so a
/// release candidate counts too.
pub const FIRST_FIX_VERSION: &str = "0.27.0-0";

/// How long `update` waits for the new binary's `migrate`.
const HEAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// After a swap, runs `<exe> migrate` so the new binary runs its own
/// migrations (ADR 0034). A release that has `doctor --fix` but not
/// `migrate` answers with a usage error (exit 2), and gets `doctor --fix`
/// instead. Returns the command that ran and what it printed. Best effort: a
/// release older than `FIRST_FIX_VERSION` is skipped, and any failure is a
/// warning that names the command that ran and carries the lines it printed.
pub fn heal_after_swap(
    exe: &Path,
    installed: &str,
) -> Result<Option<(&'static str, String)>, String> {
    if resolve::compare(installed, FIRST_FIX_VERSION).is_lt() {
        return Ok(None);
    }
    let late = |name: &str| {
        format!(
            "the new binary's {name} did not run or took over {} s; run `playbook {name}` by hand",
            HEAL_TIMEOUT.as_secs()
        )
    };
    let mut name = "migrate";
    let mut out = run_fresh(exe, &[name], HEAL_TIMEOUT).ok_or_else(|| late(name))?;
    if out.status.code() == Some(2) {
        name = "doctor --fix";
        out = run_fresh(exe, &["doctor", "--fix"], HEAL_TIMEOUT).ok_or_else(|| late(name))?;
    }
    // Whatever the new binary prints on stderr is passed on with the report.
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    if !out.status.success() {
        let detail: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        return Err(format!(
            "{name} exited with {}{}; run `playbook {name}` by hand",
            out.status,
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {}", detail.join("; "))
            }
        ));
    }
    Ok(Some((name, text)))
}

/// Runs a freshly written binary with a deadline. A spawn that fails at once
/// (a fresh binary can be refused while another process still holds it open)
/// is tried again twice.
fn run_fresh(
    exe: &Path,
    args: &[&str],
    timeout: std::time::Duration,
) -> Option<std::process::Output> {
    for _ in 0..3 {
        let started = std::time::Instant::now();
        let mut cmd = std::process::Command::new(exe);
        // `update` updates the plugin itself, once, after this heal.
        cmd.args(args)
            .env(crate::plugin_update::SKIP_ENV, "1")
            .stdin(std::process::Stdio::null());
        if let Some(out) = crate::common::proc::run_with_timeout(&mut cmd, timeout) {
            return Some(out);
        }
        if started.elapsed() > std::time::Duration::from_secs(1) {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    None
}

/// Runs the heal after a swap, when `outcome` names an installed version,
/// and adds its lines to `outcome`. After an upgrade whose notes the user
/// saw, the new version is recorded as shown first, so the heal does not
/// print the release notes line as well (ADR 0037 decision 5.1).
pub fn finish_with_heal(outcome: &mut Outcome, env: &Env) {
    let Some(version) = outcome.installed.clone() else {
        return;
    };
    if outcome.notes_shown {
        crate::notices::record(&env.root, &version);
    }
    match heal_after_swap(&env.exe, &version) {
        Ok(Some((name, text))) => outcome
            .out
            .extend(text.lines().map(|l| format!("{name}: {l}"))),
        Ok(None) => {}
        Err(warning) => outcome.warnings.push(warning),
    }
}

pub fn is_homebrew(exe: &Path) -> bool {
    let text = exe.to_string_lossy();
    ["/Cellar/", "/opt/homebrew/", "/.linuxbrew/"]
        .iter()
        .any(|m| text.contains(m))
}

/// Whether `exe` sits in `/usr/bin`, where the retired .deb and .rpm packages put it.
pub fn is_system_package(exe: &Path) -> bool {
    exe.parent() == Some(Path::new("/usr/bin"))
}

fn manual_steps() -> String {
    format!(
        "To update by hand, download the asset for your platform from \
https://github.com/{REPO}/releases/latest, check it against SHA256SUMS, \
and replace the binary, or re-run install.sh."
    )
}

fn refusals(env: &Env) -> Result<(), String> {
    if env.windows {
        return Err(format!(
            "playbook upgrade does not support Windows yet. {}",
            manual_steps()
        ));
    }
    if is_homebrew(&env.exe) {
        return Err(format!(
            "{} is managed by Homebrew; to change versions there, run: brew upgrade playbook. Or install a \
             standalone copy with install.sh and put it first on PATH.",
            env.exe.display()
        ));
    }
    if is_system_package(&env.exe) {
        return Err(format!(
            "{} is in /usr/bin, where the retired .deb and .rpm packages put it, and playbook no \
             longer publishes them. Install a standalone copy with install.sh or Homebrew and put \
             it first on PATH.",
            env.exe.display()
        ));
    }
    Ok(())
}

fn fetch_text(fetcher: &dyn Fetcher, url: &str) -> Result<String, String> {
    String::from_utf8(fetcher.get(url)?).map_err(|_| format!("{url}: response is not text"))
}

fn releases(
    env: &Env,
    fetcher: &dyn Fetcher,
    requested: Option<&str>,
) -> Result<Vec<resolve::Release>, String> {
    let url = match requested {
        Some(v) => format!(
            "{}/releases/tags/v{}",
            env.api_base,
            v.strip_prefix('v').unwrap_or(v)
        ),
        None => format!("{}{}", env.api_base, resolve::RELEASES_PAGE),
    };
    resolve::parse_releases(
        &fetch_text(fetcher, &url).map_err(|e| format!("could not read the release list: {e}"))?,
    )
}

const PLAYBOOK: &str = "playbook";
/// The helper binaries installed beside playbook, each in its own tarball.
/// Both are optional: a problem with one is a warning, never a failed update.
const HELPERS: [&str; 2] = ["pbdoc", "pbsync"];
/// What a missing helper reports as its old version.
const NO_HELPER: &str = "none";

/// Where a release's files live and what its SHA256SUMS says.
struct HelperRelease<'a> {
    base: &'a str,
    sums: &'a str,
}

/// What was swapped: its name, and how the backup line calls it.
#[derive(Clone, Copy)]
struct Swapped {
    name: &'static str,
    noun: &'static str,
}

const SWAPPED_PLAYBOOK: Swapped = Swapped {
    name: PLAYBOOK,
    noun: "binary",
};

/// Adds the lines that say `name` was swapped, and where its backup is.
fn report_swap(
    o: &mut Outcome,
    swapped: Swapped,
    target: &Path,
    from: &str,
    to: &str,
    done: swap::Installed,
) {
    let Swapped { name, noun } = swapped;
    o.warnings.extend(done.warnings);
    o.out.push(format!(
        "updated {name} {from} -> {to} at {}",
        target.display()
    ));
    if let Some(bak) = done.backup {
        o.out.push(format!(
            "kept the previous {noun} as {}; to roll back: mv -f {} {}",
            bak.display(),
            bak.display(),
            target.display()
        ));
    }
}

/// Swaps the `name` helper beside the playbook binary. It never fails the
/// update: any problem is a warning and the old helper stays.
fn swap_helper(
    o: &mut Outcome,
    env: &Env,
    fetcher: &dyn Fetcher,
    attest: &dyn Attest,
    release: &HelperRelease,
    name: &'static str,
    version: &str,
) {
    match fetch_helper(o, env, fetcher, attest, release, name, version) {
        Ok((target, from, done)) => {
            let swapped = Swapped { name, noun: name };
            report_swap(o, swapped, &target, &from, version, done);
        }
        Err(err) => o.warnings.push(format!(
            "{name} was not updated and the old one stays: {err}"
        )),
    }
}

/// Swaps every helper that ships with this release.
fn swap_helpers(
    o: &mut Outcome,
    env: &Env,
    fetcher: &dyn Fetcher,
    attest: &dyn Attest,
    release: &HelperRelease,
    version: &str,
) {
    for name in HELPERS {
        swap_helper(o, env, fetcher, attest, release, name, version);
    }
}

/// The version a helper binary reports, the last word of `--version`.
fn helper_version(bin: &Path) -> Option<String> {
    let out = swap::run_version(bin).ok()?;
    out.split_whitespace().last().map(str::to_string)
}

/// Downloads, verifies and installs a helper. Returns the target, the version
/// it replaced and what the swap left behind.
fn fetch_helper(
    o: &mut Outcome,
    env: &Env,
    fetcher: &dyn Fetcher,
    attest: &dyn Attest,
    release: &HelperRelease,
    name: &str,
    version: &str,
) -> Result<(PathBuf, String, swap::Installed), String> {
    let asset = resolve::helper_asset_name(name, version, &env.os, &env.arch)?;
    let bytes = fetcher
        .get(&format!("{}/{asset}", release.base))
        .map_err(|e| format!("could not download {asset}: {e}"))?;
    verify::verify_checksum(&bytes, release.sums, &asset)?;
    let file = tarball::gunzip(&bytes)
        .and_then(|tar| tarball::extract_regular_file(&tar, name))
        .map_err(|e| format!("{asset}: {e}"))?;
    let dir = env
        .exe
        .parent()
        .ok_or("the running binary has no parent directory")?;
    let stage = swap::write_stage(dir, &file)
        .map_err(|e| format!("cannot write to {}: {e}", dir.display()))?;
    // The staged file is attested before it is ever run.
    match attest.verify(&stage, env.strict) {
        Ok(None) => o
            .out
            .push(format!("verified the build attestation for {asset}")),
        Ok(Some(warning)) => o.warnings.push(warning),
        Err(err) => {
            let _ = std::fs::remove_file(&stage);
            return Err(err);
        }
    }
    let target = dir.join(name);
    let from = helper_version(&target).unwrap_or_else(|| NO_HELPER.to_string());
    let done = swap::install(&stage, &target, &from, version, &swap::run_version)?;
    Ok((target, from, done))
}

/// On an equal version playbook stays, but a missing or older helper is still
/// installed, so an upgrade from a release without it gets the helper.
fn complete_helpers(
    o: &mut Outcome,
    opts: &Options,
    env: &Env,
    fetcher: &dyn Fetcher,
    attest: &dyn Attest,
    tag: &str,
    version: &str,
) {
    let Some(dir) = env.exe.parent() else { return };
    let current = HELPERS
        .iter()
        .all(|name| helper_version(&dir.join(name)).as_deref() == Some(version));
    if current || refusals(env).is_err() || (env.auto_mode && !opts.yes) {
        return;
    }
    let base = format!("{}/{tag}", env.download_base);
    match fetch_text(fetcher, &format!("{base}/SHA256SUMS")) {
        Ok(sums) => {
            let release = HelperRelease {
                base: &base,
                sums: &sums,
            };
            for name in HELPERS {
                if helper_version(&dir.join(name)).as_deref() != Some(version) {
                    swap_helper(o, env, fetcher, attest, &release, name, version);
                }
            }
        }
        Err(err) => o.warnings.push(format!(
            "the helpers were not updated: could not download SHA256SUMS: {err}"
        )),
    }
}

/// Runs the update. Read-only for `--list` and `--check`.
pub fn run(
    opts: &Options,
    env: &Env,
    fetcher: &dyn Fetcher,
    attest: &dyn Attest,
    smoke: &dyn Fn(&Path) -> Result<String, String>,
) -> Result<Outcome, String> {
    let mut o = Outcome::default();
    let current = env.current_version.as_str();

    if opts.list {
        for r in releases(env, fetcher, None)? {
            let mut line = r.tag.clone();
            if r.prerelease {
                line.push_str(" (pre-release)");
            }
            if r.version() == current {
                line.push_str(" (installed)");
            }
            o.out.push(line);
        }
        return Ok(o);
    }

    let found = releases(env, fetcher, opts.version.as_deref())?;
    let release = resolve::select(&found, opts.version.as_deref(), opts.pre)?;
    let version = release.version().to_string();
    let order = resolve::compare(&version, current);
    o.latest = Some(version.clone());

    if opts.check {
        o.status = if order.is_gt() {
            Status::Available
        } else {
            Status::Current
        };
        o.out.push(match order {
            std::cmp::Ordering::Greater => {
                format!("update available: {current} -> {version} (run `playbook upgrade`)")
            }
            _ => format!("playbook {current} is up to date (latest is {version})"),
        });
        return Ok(o);
    }

    if order.is_eq() {
        o.status = Status::Current;
        o.out
            .push(format!("playbook {current} is already installed"));
        complete_helpers(&mut o, opts, env, fetcher, attest, &release.tag, &version);
        return Ok(o);
    }
    if order.is_lt() && opts.version.is_none() {
        o.status = Status::Current;
        o.out.push(format!(
            "playbook {current} is newer than the latest release {version}; nothing to do"
        ));
        return Ok(o);
    }

    if order.is_gt() && is_homebrew(&env.exe) {
        o.status = Status::Homebrew;
        o.out.push(format!(
            "playbook {version} is available (you have {current}). Homebrew owns this install, so upgrade leaves the binary alone; run: brew upgrade playbook"
        ));
        return Ok(o);
    }
    refusals(env)?;
    if env.auto_mode && !opts.yes {
        return Err("auto mode is on and an upgrade replaces the running binary; re-run with --yes to confirm".into());
    }
    if !opts.yes {
        let question = format!("Upgrade playbook {current} -> {version}? [y/N] ");
        match (env.confirm)(&question) {
            Reply::Yes => {}
            Reply::No => {
                o.status = Status::Declined;
                o.out.push(format!(
                    "playbook {current} was not upgraded; nothing changed"
                ));
                return Ok(o);
            }
            Reply::NoTerminal => {
                o.status = Status::NoTerminal;
                o.out.push(format!(
                    "playbook {version} is available (you have {current}). There is no terminal to confirm in, so nothing changed; run: playbook upgrade --yes"
                ));
                return Ok(o);
            }
        }
    }

    let asset = resolve::asset_name(&version, &env.os, &env.arch)?;
    let base = format!("{}/{}", env.download_base, release.tag);
    let bytes = fetcher
        .get(&format!("{base}/{asset}"))
        .map_err(|e| format!("could not download {asset}: {e}"))?;
    let sums = fetch_text(fetcher, &format!("{base}/SHA256SUMS"))
        .map_err(|e| format!("could not download SHA256SUMS: {e}"))?;
    verify::verify_checksum(&bytes, &sums, &asset)?;

    let dir = env
        .exe
        .parent()
        .ok_or("the running binary has no parent directory")?;
    let stage = swap::write_stage(dir, &bytes)
        .map_err(|e| format!("cannot write to {}: {e}", dir.display()))?;
    match attest.verify(&stage, env.strict) {
        Ok(None) => o
            .out
            .push(format!("verified the build attestation for {asset}")),
        Ok(Some(warning)) => o.warnings.push(warning),
        Err(err) => {
            let _ = std::fs::remove_file(&stage);
            return Err(err);
        }
    }

    if order.is_gt() {
        // The notes never fail an update: without them, only the link shows.
        let notes = match crate::release_notes::fetch(fetcher, &base, &version, Some(&sums)) {
            Ok(notes) => notes,
            Err(err) => {
                o.warnings
                    .push(format!("the release notes were not shown: {err}"));
                None
            }
        };
        o.notes = crate::release_notes::render(notes.as_ref(), current, &version);
        o.notes_shown = notes.is_some();
        (env.announce)(&o.notes);
    }

    let done = swap::install(&stage, &env.exe, current, &version, smoke)?;
    o.installed = Some(version.clone());
    o.status = Status::Upgraded;
    report_swap(&mut o, SWAPPED_PLAYBOOK, &env.exe, current, &version, done);
    swap_helpers(
        &mut o,
        env,
        fetcher,
        attest,
        &HelperRelease {
            base: &base,
            sums: &sums,
        },
        &version,
    );
    o.warnings
        .extend(pathcheck::shadow_warning(&env.path_var, &env.exe));
    Ok(o)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;
    use download::fake::FakeFetcher;
    use verify::fake::FakeAttest;

    const API: &str = "https://api.test/repos/x";
    const DL: &str = "https://dl.test";

    fn script_bytes(version: &str) -> Vec<u8> {
        format!("#!/bin/sh\necho 'playbook {version}'\n").into_bytes()
    }

    fn env_in(tag: &str) -> Env {
        let dir = scratch_dir(tag);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("playbook");
        std::fs::write(&exe, script_bytes("0.16.0")).unwrap();
        swap::make_executable(&exe).unwrap();
        for name in ["pbdoc", "pbsync"] {
            let helper = dir.join(name);
            std::fs::write(&helper, pbdoc_fixture::script_of(name, "0.16.0")).unwrap();
            swap::make_executable(&helper).unwrap();
        }
        Env {
            exe,
            path_var: dir.clone().into_os_string(),
            auto_mode: false,
            strict: false,
            windows: false,
            os: "linux".into(),
            arch: "x86_64".into(),
            current_version: "0.16.0".into(),
            api_base: API.into(),
            download_base: DL.into(),
            root: dir.join("root"),
            announce: announce_to_test,
            confirm: |_| crate::update::Reply::Yes,
        }
    }

    thread_local! {
        static ANNOUNCED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    fn announce_to_test(text: &str) {
        ANNOUNCED.with(|a| a.borrow_mut().push(text.to_string()));
    }

    fn announced() -> Vec<String> {
        ANNOUNCED.with(|a| a.borrow_mut().drain(..).collect())
    }

    const NOTES: &str = r#"{"format":1,"component":"playbook","version":"0.17.0","tag":"v0.17.0",
        "previous":"0.16.0","previous_tag":"v0.16.0","date":"2026-10-16","sections":[
        {"name":"breaking","title":"Breaking changes","items":[
        {"type":"feat","scope":null,"text":"The old flag is gone; run playbook setup","pr":7,"breaking":true}]}]}"#;

    /// The fixture with release-notes.json, listed in SHA256SUMS with the
    /// hash of `notes_hash_of`.
    fn fixture_with_notes(binary: &[u8], notes_hash_of: &[u8]) -> FakeFetcher {
        let mut f = fixture(binary, binary);
        let url = format!("{DL}/v0.17.0/SHA256SUMS");
        let mut sums = String::from_utf8(f.0[&url].clone()).unwrap();
        sums.push_str(&format!(
            "{}  {}\n",
            crate::common::hash::sha256_hex(notes_hash_of),
            crate::release_notes::ASSET
        ));
        f.0.insert(url, sums.into_bytes());
        f.0.insert(
            format!("{DL}/v0.17.0/{}", crate::release_notes::ASSET),
            NOTES.into(),
        );
        f
    }

    const LINK: &str =
        "Full notes: https://github.com/amplifiedengineer/playbook/releases/tag/v0.17.0\n";

    fn fixture(binary: &[u8], sums_hash_of: &[u8]) -> FakeFetcher {
        let asset = "playbook-0.17.0-x86_64-unknown-linux-musl";
        let list = r#"[{"tag_name":"v0.18.0-rc.1","prerelease":true},{"tag_name":"v0.17.0","prerelease":false}]"#;
        let mut f = FakeFetcher::default();
        f.0.insert(format!("{API}{}", resolve::RELEASES_PAGE), list.into());
        f.0.insert(
            format!("{API}/releases/tags/v0.17.0"),
            r#"{"tag_name":"v0.17.0"}"#.into(),
        );
        f.0.insert(format!("{DL}/v0.17.0/{asset}"), binary.to_vec());
        let sums = format!(
            "{}  {asset}\n",
            crate::common::hash::sha256_hex(sums_hash_of)
        );
        f.0.insert(format!("{DL}/v0.17.0/SHA256SUMS"), sums.into_bytes());
        for name in ["pbdoc", "pbsync"] {
            let tar = pbdoc_fixture::good_of(name, "0.17.0");
            pbdoc_fixture::serve_of(&mut f, &format!("{DL}/v0.17.0"), name, &tar, &tar);
        }
        f
    }

    fn ok_attest() -> FakeAttest {
        FakeAttest::new(Ok(None))
    }

    #[test]
    fn list_shows_tags_with_markers_and_changes_nothing() {
        let mut env = env_in("up-list");
        env.current_version = "0.17.0".into();
        let opts = Options {
            list: true,
            ..Options::default()
        };
        let out = run(
            &opts,
            &env,
            &fixture(b"", b""),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();
        assert_eq!(
            out.out,
            vec!["v0.18.0-rc.1 (pre-release)", "v0.17.0 (installed)"]
        );
    }

    #[test]
    fn check_reports_the_latest_stable_without_writing() {
        let env = env_in("up-check");
        let opts = Options {
            check: true,
            ..Options::default()
        };
        let out = run(
            &opts,
            &env,
            &fixture(b"", b""),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();
        assert!(out.out[0].contains("0.16.0 -> 0.17.0"), "{:?}", out.out);
        assert_eq!(out.installed, None);
        assert_eq!(swap::run_version(&env.exe).unwrap(), "playbook 0.16.0");
    }

    #[test]
    fn check_with_pre_sees_the_release_candidate() {
        let env = env_in("up-check-pre");
        let opts = Options {
            check: true,
            pre: true,
            ..Options::default()
        };
        let out = run(
            &opts,
            &env,
            &fixture(b"", b""),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();
        assert!(out.out[0].contains("0.18.0-rc.1"), "{:?}", out.out);
    }

    #[test]
    fn an_update_replaces_the_binary_and_keeps_a_backup() {
        let env = env_in("up-ok");
        let bin = script_bytes("0.17.0");
        let out = run(
            &Options::default(),
            &env,
            &fixture(&bin, &bin),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();
        assert_eq!(swap::run_version(&env.exe).unwrap(), "playbook 0.17.0");
        assert!(env.exe.with_file_name("playbook.0.16.0.bak").exists());
        assert!(out.out.iter().any(|l| l.contains("0.16.0 -> 0.17.0")));
        assert_eq!(out.installed.as_deref(), Some("0.17.0"));
    }

    #[test]
    fn an_update_shows_the_breaking_changes_of_the_release_before_the_swap() {
        // Arrange
        let env = env_in("up-notes");
        let bin = script_bytes("0.17.0");
        announced();

        // Act
        let out = run(
            &Options::default(),
            &env,
            &fixture_with_notes(&bin, NOTES.as_bytes()),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();

        // Assert
        assert_eq!(
            out.notes,
            format!("Breaking changes in playbook 0.17.0:\n  - The old flag is gone; run playbook setup (#7)\n{LINK}")
        );
        assert_eq!(announced(), vec![out.notes.clone()]);
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        assert_eq!(out.installed.as_deref(), Some("0.17.0"));
    }

    #[test]
    fn a_release_without_notes_shows_only_the_link() {
        // Arrange
        let env = env_in("up-no-notes");
        let bin = script_bytes("0.17.0");
        announced();

        // Act
        let out = run(
            &Options::default(),
            &env,
            &fixture(&bin, &bin),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();

        // Assert
        assert_eq!(out.notes, LINK);
        assert_eq!(announced(), vec![LINK.to_string()]);
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        assert_eq!(out.installed.as_deref(), Some("0.17.0"));
    }

    #[test]
    fn notes_that_do_not_match_their_checksum_show_only_the_link_and_a_warning() {
        // Arrange
        let env = env_in("up-notes-bad");
        let bin = script_bytes("0.17.0");
        announced();

        // Act
        let out = run(
            &Options::default(),
            &env,
            &fixture_with_notes(&bin, b"other bytes"),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();

        // Assert
        assert_eq!(out.notes, LINK);
        assert!(
            out.warnings
                .iter()
                .any(|w| w.starts_with("the release notes were not shown")),
            "{:?}",
            out.warnings
        );
        assert_eq!(out.installed.as_deref(), Some("0.17.0"));
    }

    #[test]
    fn the_notes_are_shown_before_a_swap_that_fails() {
        let env = env_in("up-notes-fail");
        let bin = script_bytes("0.17.0");
        announced();

        let err = run(
            &Options::default(),
            &env,
            &fixture_with_notes(&bin, NOTES.as_bytes()),
            &ok_attest(),
            &|_: &Path| Err("smoke test failed".to_string()),
        )
        .unwrap_err();

        assert!(err.contains("smoke test"), "{err}");
        assert_eq!(announced().len(), 1);
        assert_eq!(swap::run_version(&env.exe).unwrap(), "playbook 0.16.0");
    }

    #[test]
    fn a_downgrade_shows_no_notes() {
        let mut env = env_in("up-down-notes");
        env.current_version = "0.19.0".into();
        let bin = script_bytes("0.17.0");
        let opts = Options {
            version: Some("0.17.0".into()),
            ..Options::default()
        };
        announced();

        let out = run(
            &opts,
            &env,
            &fixture_with_notes(&bin, NOTES.as_bytes()),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();

        assert_eq!(out.notes, "");
        assert!(announced().is_empty());
    }

    #[test]
    fn an_upgrade_records_its_notes_as_shown_before_the_heal() {
        // Arrange
        let env = env_in("heal-shown");
        std::fs::write(
            &env.exe,
            "#!/bin/sh
echo ran\n",
        )
        .unwrap();
        swap::make_executable(&env.exe).unwrap();
        let mut shown = Outcome {
            installed: Some("0.27.0".into()),
            notes: LINK.into(),
            notes_shown: true,
            ..Outcome::default()
        };
        let other = env_in("heal-link-only");
        std::fs::copy(&env.exe, &other.exe).unwrap();
        let mut link_only = Outcome {
            installed: Some("0.27.0".into()),
            notes: LINK.into(),
            ..Outcome::default()
        };
        let notices =
            |root: &Path| crate::state::list_read_only(root, "notices/binary").unwrap_or_default();

        // Act
        let before = notices(&env.root);
        finish_with_heal(&mut shown, &env);
        let after = notices(&env.root);
        finish_with_heal(&mut link_only, &other);

        // Assert
        assert!(before.is_empty());
        assert_eq!(
            after,
            vec![("notices/binary".to_string(), "0.27.0".to_string())]
        );
        assert_eq!(shown.out, vec!["migrate: ran"]);
        assert!(notices(&other.root).is_empty(), "only the link was shown");
    }

    #[test]
    fn the_heal_after_a_swap_skips_releases_without_doctor_fix() {
        let env = env_in("heal-old");
        assert_eq!(heal_after_swap(&env.exe, "0.26.0"), Ok(None));
    }

    #[test]
    fn the_heal_after_a_swap_runs_the_new_binary_and_reports_its_failure() {
        // Arrange
        let env = env_in("heal-run");
        std::fs::write(&env.exe, "#!/bin/sh\necho 'a note' >&2\necho \"ran: $*\"\n").unwrap();
        swap::make_executable(&env.exe).unwrap();
        let mut done = Outcome {
            installed: Some("0.27.0-rc.1".into()),
            ..Outcome::default()
        };

        // Act
        finish_with_heal(&mut done, &env);
        let mut untouched = Outcome::default();
        finish_with_heal(&mut untouched, &env);
        std::fs::write(
            &env.exe,
            "#!/bin/sh\necho 'failed: state: locked'\nexit 1\n",
        )
        .unwrap();
        let failed = heal_after_swap(&env.exe, "1.0.0").unwrap_err();
        std::fs::write(
            &env.exe,
            "#!/bin/sh\n[ \"$1\" = migrate ] && exit 2\necho \"ran: $*\"\n",
        )
        .unwrap();
        let older = heal_after_swap(&env.exe, "0.27.0").unwrap();
        std::fs::write(
            &env.exe,
            "#!/bin/sh\n[ \"$1\" = migrate ] && exit 2\necho 'failed: x'\nexit 1\n",
        )
        .unwrap();
        let older_failed = heal_after_swap(&env.exe, "0.27.0").unwrap_err();

        // Assert
        assert_eq!(done.out, vec!["migrate: a note", "migrate: ran: migrate"]);
        assert_eq!(older, Some(("doctor --fix", "ran: doctor --fix\n".into())));
        assert!(
            older_failed.starts_with("doctor --fix exited with")
                && older_failed.ends_with("run `playbook doctor --fix` by hand"),
            "{older_failed}"
        );
        assert!(done.warnings.is_empty());
        assert!(untouched.out.is_empty() && untouched.warnings.is_empty());
        assert!(failed.contains(": failed: state: locked;"), "{failed}");
        assert!(
            failed.contains("run `playbook migrate` by hand"),
            "{failed}"
        );
    }

    #[test]
    fn an_explicit_version_uses_the_tag_endpoint() {
        let env = env_in("up-explicit");
        let bin = script_bytes("0.17.0");
        let opts = Options {
            version: Some("0.17.0".into()),
            ..Options::default()
        };
        run(
            &opts,
            &env,
            &fixture(&bin, &bin),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();
        assert_eq!(swap::run_version(&env.exe).unwrap(), "playbook 0.17.0");
    }

    #[test]
    fn a_checksum_mismatch_aborts_before_anything_is_written() {
        let env = env_in("up-badsum");
        let err = run(
            &Options::default(),
            &env,
            &fixture(&script_bytes("0.17.0"), b"other"),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap_err();
        assert!(err.contains("checksum mismatch"), "{err}");
        assert_eq!(swap::run_version(&env.exe).unwrap(), "playbook 0.16.0");
        assert!(!env.exe.with_file_name("playbook.0.16.0.bak").exists());
    }

    #[test]
    fn a_failed_attestation_aborts_and_a_lenient_gap_only_warns() {
        let env = env_in("up-attest");
        let bin = script_bytes("0.17.0");
        let bad = FakeAttest::new(Err("attestation verification failed".into()));
        assert!(run(
            &Options::default(),
            &env,
            &fixture(&bin, &bin),
            &bad,
            &swap::run_version
        )
        .is_err());
        assert_eq!(swap::run_version(&env.exe).unwrap(), "playbook 0.16.0");
        let gap = FakeAttest::new(Ok(Some("checksum only".into())));
        let out = run(
            &Options::default(),
            &env,
            &fixture(&bin, &bin),
            &gap,
            &swap::run_version,
        )
        .unwrap();
        assert!(out.warnings.iter().any(|w| w.contains("checksum only")));
    }

    #[test]
    fn a_binary_that_does_not_run_is_rejected_and_the_original_stays() {
        let env = env_in("up-badbin");
        let bin = b"#!/bin/sh\nexit 1\n".to_vec();
        let err = run(
            &Options::default(),
            &env,
            &fixture(&bin, &bin),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap_err();
        assert!(err.contains("smoke test"), "{err}");
        assert_eq!(swap::run_version(&env.exe).unwrap(), "playbook 0.16.0");
    }

    fn brew_env(tag: &str) -> Env {
        let mut env = env_in(tag);
        env.exe = scratch_dir(&format!("{tag}-cellar")).join("Cellar/playbook/0.14.0/bin/playbook");
        env.confirm = |_| panic!("a Homebrew install must not ask");
        env
    }

    #[test]
    fn a_homebrew_binary_with_a_newer_release_gets_the_brew_hint_and_no_change() {
        let env = brew_env("up-brew");
        let out = run(
            &Options::default(),
            &env,
            &fixture(b"", b""),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();
        assert_eq!(out.status, Status::Homebrew);
        assert_eq!(out.latest.as_deref(), Some("0.17.0"));
        assert_eq!(out.installed, None);
        let text = out.out.join("\n");
        assert!(
            text.contains("0.17.0 is available (you have 0.16.0)"),
            "{text}"
        );
        assert!(text.contains("Homebrew owns this install"), "{text}");
        assert!(text.contains("brew upgrade playbook"), "{text}");
        assert!(!env.exe.with_file_name("pbdoc").exists());
    }

    #[test]
    fn a_homebrew_binary_that_is_current_reports_current() {
        let mut env = brew_env("up-brew-current");
        env.current_version = "0.17.0".into();
        let out = run(
            &Options::default(),
            &env,
            &fixture(b"", b""),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();
        assert_eq!(out.status, Status::Current);
    }

    #[test]
    fn a_homebrew_binary_asked_for_an_older_version_is_refused_with_the_brew_command() {
        let env = brew_env("up-brew-older");
        let mut env = env;
        env.current_version = "0.18.0".into();
        let opts = Options {
            version: Some("0.17.0".into()),
            ..Options::default()
        };
        let err = run(
            &opts,
            &env,
            &fixture(b"", b""),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap_err();
        assert!(err.contains("brew upgrade playbook"), "{err}");
    }

    thread_local! {
        static ASKED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    fn asked() -> Vec<String> {
        ASKED.with(|a| a.borrow_mut().drain(..).collect())
    }

    fn remember(question: &str) {
        ASKED.with(|a| a.borrow_mut().push(question.to_string()));
    }

    #[test]
    fn a_yes_to_the_question_installs_and_the_question_names_both_versions() {
        let mut env = env_in("up-confirm-yes");
        env.confirm = |q| {
            remember(q);
            Reply::Yes
        };
        let bin = script_bytes("0.17.0");
        let out = run(
            &Options::default(),
            &env,
            &fixture(&bin, &bin),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();
        assert_eq!(out.status, Status::Upgraded);
        assert_eq!(swap::run_version(&env.exe).unwrap(), "playbook 0.17.0");
        assert_eq!(asked(), ["Upgrade playbook 0.16.0 -> 0.17.0? [y/N] "]);
    }

    #[test]
    fn a_no_to_the_question_changes_nothing() {
        let mut env = env_in("up-confirm-no");
        env.confirm = |q| {
            remember(q);
            Reply::No
        };
        let bin = script_bytes("0.17.0");
        let out = run(
            &Options::default(),
            &env,
            &fixture(&bin, &bin),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();
        assert_eq!(out.status, Status::Declined);
        assert_eq!(out.installed, None);
        assert!(out.out[0].contains("was not upgraded"), "{:?}", out.out);
        assert_eq!(swap::run_version(&env.exe).unwrap(), "playbook 0.16.0");
        assert!(!env.exe.with_file_name("playbook.0.16.0.bak").exists());
        assert_eq!(asked().len(), 1);
    }

    #[test]
    fn no_terminal_and_no_yes_prints_the_hint_and_changes_nothing() {
        let mut env = env_in("up-confirm-none");
        env.confirm = |_| Reply::NoTerminal;
        let bin = script_bytes("0.17.0");
        let out = run(
            &Options::default(),
            &env,
            &fixture(&bin, &bin),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();
        assert_eq!(out.status, Status::NoTerminal);
        let text = out.out.join("\n");
        assert!(
            text.contains("0.17.0 is available (you have 0.16.0)"),
            "{text}"
        );
        assert!(text.contains("playbook upgrade --yes"), "{text}");
        assert_eq!(swap::run_version(&env.exe).unwrap(), "playbook 0.16.0");
    }

    #[test]
    fn yes_skips_the_question() {
        let mut env = env_in("up-confirm-skip");
        env.confirm = |_| panic!("--yes must not ask");
        let bin = script_bytes("0.17.0");
        let opts = Options {
            yes: true,
            ..Options::default()
        };
        let out = run(
            &opts,
            &env,
            &fixture(&bin, &bin),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();
        assert_eq!(out.status, Status::Upgraded);
    }

    #[test]
    fn a_current_binary_never_asks() {
        let mut env = env_in("up-confirm-current");
        env.current_version = "0.17.0".into();
        env.confirm = |_| panic!("nothing to confirm");
        let out = run(
            &Options::default(),
            &env,
            &fixture(b"", b""),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();
        assert_eq!(out.status, Status::Current);
        assert_eq!(out.latest.as_deref(), Some("0.17.0"));
    }

    #[test]
    fn check_reports_available_or_current_without_asking() {
        let mut env = env_in("up-check-status");
        env.confirm = |_| panic!("--check must not ask");
        let opts = Options {
            check: true,
            ..Options::default()
        };
        let out = run(
            &opts,
            &env,
            &fixture(b"", b""),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();
        assert_eq!(out.status, Status::Available);
        env.current_version = "0.17.0".into();
        let out = run(
            &opts,
            &env,
            &fixture(b"", b""),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();
        assert_eq!(out.status, Status::Current);
    }

    #[test]
    fn the_json_report_carries_the_envelope_the_status_and_the_plugin_steps() {
        let outcome = Outcome {
            status: Status::NoTerminal,
            latest: Some("0.17.0".into()),
            out: vec!["a line".into()],
            ..Outcome::default()
        };
        let v = json_report(&Ok(outcome), None);
        assert_eq!(v["ok"], true);
        assert_eq!(v["code"], 0);
        assert_eq!(v["status"], "no-terminal");
        assert_eq!(v["latest"], "0.17.0");
        assert_eq!(v["lines"][0], "a line");
        assert!(v["plugin"].is_null());
        let v = json_report(&Err("no network".into()), None);
        assert_eq!(v["ok"], false);
        assert_eq!(v["code"], 1);
        assert_eq!(v["error"], "no network");
    }

    const RELEASE: &str = "https://dl.test/v0.17.0";

    fn pbsync_of(env: &Env) -> PathBuf {
        env.exe.with_file_name("pbsync")
    }

    fn pbdoc_of(env: &Env) -> PathBuf {
        env.exe.with_file_name("pbdoc")
    }

    #[test]
    fn a_good_pbdoc_is_swapped_with_the_playbook_and_both_are_backed_up() {
        // Arrange
        let env = env_in("up-pbdoc-ok");
        let bin = script_bytes("0.17.0");

        // Act
        let out = run(
            &Options::default(),
            &env,
            &fixture(&bin, &bin),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();

        // Assert
        assert_eq!(swap::run_version(&env.exe).unwrap(), "playbook 0.17.0");
        assert_eq!(swap::run_version(&pbdoc_of(&env)).unwrap(), "pbdoc 0.17.0");
        assert!(env.exe.with_file_name("playbook.0.16.0.bak").exists());
        assert!(env.exe.with_file_name("pbdoc.0.16.0.bak").exists());
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    }

    #[test]
    fn a_release_without_a_pbdoc_asset_still_updates_playbook_and_warns() {
        // Arrange
        let env = env_in("up-pbdoc-missing");
        let bin = script_bytes("0.17.0");
        let mut f = fixture(&bin, &bin);
        pbdoc_fixture::remove(&mut f, RELEASE);

        // Act
        let out = run(
            &Options::default(),
            &env,
            &f,
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();

        // Assert
        assert_eq!(swap::run_version(&env.exe).unwrap(), "playbook 0.17.0");
        assert_eq!(swap::run_version(&pbdoc_of(&env)).unwrap(), "pbdoc 0.16.0");
        assert!(
            out.warnings.iter().any(|w| w.contains("pbdoc")),
            "{:?}",
            out.warnings
        );
    }

    #[test]
    fn a_pbdoc_with_a_bad_checksum_is_left_untouched_with_a_checksum_warning() {
        // Arrange
        let env = env_in("up-pbdoc-badsum");
        let bin = script_bytes("0.17.0");
        let mut f = fixture(&bin, &bin);
        pbdoc_fixture::serve(&mut f, RELEASE, &pbdoc_fixture::good("0.17.0"), b"other");

        // Act
        let out = run(
            &Options::default(),
            &env,
            &f,
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();

        // Assert
        assert_eq!(swap::run_version(&env.exe).unwrap(), "playbook 0.17.0");
        assert_eq!(swap::run_version(&pbdoc_of(&env)).unwrap(), "pbdoc 0.16.0");
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("pbdoc") && w.contains("checksum")),
            "{:?}",
            out.warnings
        );
    }

    #[test]
    fn a_tarball_with_pbdoc_and_its_notice_installs_only_pbdoc() {
        // Arrange
        let env = env_in("up-pbdoc-notice");
        let bin = script_bytes("0.17.0");
        let mut f = fixture(&bin, &bin);
        let tarball = pbdoc_fixture::tarball(&[
            pbdoc_fixture::entry("pbdoc", b'0', "", &pbdoc_fixture::script("0.17.0")),
            pbdoc_fixture::entry("NOTICE-pbdoc", b'0', "", b"notice"),
        ]);
        pbdoc_fixture::serve(&mut f, RELEASE, &tarball, &tarball);

        // Act
        let out = run(
            &Options::default(),
            &env,
            &f,
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();

        // Assert
        assert_eq!(swap::run_version(&pbdoc_of(&env)).unwrap(), "pbdoc 0.17.0");
        assert!(!env.exe.with_file_name("NOTICE-pbdoc").exists());
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    }

    #[test]
    fn a_tarball_that_is_not_one_regular_pbdoc_file_is_refused() {
        // Arrange
        let script = pbdoc_fixture::script("0.17.0");
        let cases = [
            (
                "a symlink",
                "is not a regular file",
                vec![pbdoc_fixture::entry("pbdoc", b'2', "/bin/sh", b"")],
            ),
            (
                "an unknown extra file",
                "expected only pbdoc",
                vec![
                    pbdoc_fixture::entry("pbdoc", b'0', "", &script),
                    pbdoc_fixture::entry("extra", b'0', "", b"x"),
                ],
            ),
            (
                "a notice that is a symlink",
                "is not a regular file",
                vec![
                    pbdoc_fixture::entry("pbdoc", b'0', "", &script),
                    pbdoc_fixture::entry("NOTICE-pbdoc", b'2', "/etc/passwd", b""),
                ],
            ),
            (
                "two pbdoc entries",
                "more than once",
                vec![
                    pbdoc_fixture::entry("pbdoc", b'0', "", &script),
                    pbdoc_fixture::entry("pbdoc", b'0', "", &script),
                ],
            ),
            (
                "a path outside the archive",
                "expected only pbdoc",
                vec![
                    pbdoc_fixture::entry("pbdoc", b'0', "", &script),
                    pbdoc_fixture::entry("../NOTICE-pbdoc", b'0', "", b"x"),
                ],
            ),
            (
                "no pbdoc",
                "does not hold",
                vec![pbdoc_fixture::entry("NOTICE-pbdoc", b'0', "", b"notice")],
            ),
            (
                "another name",
                "expected only pbdoc",
                vec![pbdoc_fixture::entry("evil", b'0', "", &script)],
            ),
        ];
        for (n, (what, reason, entries)) in cases.into_iter().enumerate() {
            let env = env_in(&format!("up-pbdoc-refused-{n}"));
            let bin = script_bytes("0.17.0");
            let mut f = fixture(&bin, &bin);
            let tarball = pbdoc_fixture::tarball(&entries);
            pbdoc_fixture::serve(&mut f, RELEASE, &tarball, &tarball);

            // Act
            let out = run(
                &Options::default(),
                &env,
                &f,
                &ok_attest(),
                &swap::run_version,
            )
            .unwrap();

            // Assert
            assert_eq!(
                swap::run_version(&pbdoc_of(&env)).unwrap(),
                "pbdoc 0.16.0",
                "{what}"
            );
            assert!(
                out.warnings
                    .iter()
                    .any(|w| w.contains("pbdoc") && w.contains(reason)),
                "{what}: {:?}",
                out.warnings
            );
        }
    }

    struct FailsOnPbdoc;

    impl Attest for FailsOnPbdoc {
        fn verify(&self, file: &Path, _: bool) -> Result<Option<String>, String> {
            let text = std::fs::read_to_string(file).unwrap();
            if text.contains("pbdoc") {
                Err("no attestation for pbdoc".into())
            } else {
                Ok(None)
            }
        }
    }

    #[test]
    fn a_pbdoc_that_fails_attestation_is_not_run_or_installed() {
        // Arrange: the fake helper would write a marker if it ever ran.
        let env = env_in("up-pbdoc-attest");
        let bin = script_bytes("0.17.0");
        let marker = env.exe.with_file_name("ran");
        let evil = format!("#!/bin/sh\n: > {}\necho 'pbdoc 0.17.0'\n", marker.display());
        let tarball =
            pbdoc_fixture::tarball(&[pbdoc_fixture::entry("pbdoc", b'0', "", evil.as_bytes())]);
        let mut f = fixture(&bin, &bin);
        pbdoc_fixture::serve(&mut f, RELEASE, &tarball, &tarball);

        // Act
        let out = run(
            &Options::default(),
            &env,
            &f,
            &FailsOnPbdoc,
            &swap::run_version,
        )
        .unwrap();

        // Assert
        assert_eq!(swap::run_version(&pbdoc_of(&env)).unwrap(), "pbdoc 0.16.0");
        assert!(!marker.exists());
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("pbdoc was not updated") && w.contains("no attestation")),
            "{:?}",
            out.warnings
        );
    }

    #[test]
    fn a_lenient_attestation_gap_on_pbdoc_only_warns() {
        let env = env_in("up-pbdoc-gap");
        let bin = script_bytes("0.17.0");
        let attest = FakeAttest::new(Ok(Some("no gh to check".into())));

        let out = run(
            &Options::default(),
            &env,
            &fixture(&bin, &bin),
            &attest,
            &swap::run_version,
        )
        .unwrap();

        assert_eq!(swap::run_version(&pbdoc_of(&env)).unwrap(), "pbdoc 0.17.0");
        assert!(out.warnings.iter().any(|w| w.contains("no gh")));
    }

    #[test]
    fn the_pbdoc_backup_is_named_from_pbdoc_own_version() {
        let env = env_in("up-pbdoc-skew");
        std::fs::write(pbdoc_of(&env), pbdoc_fixture::script("0.15.0")).unwrap();
        let bin = script_bytes("0.17.0");

        run(
            &Options::default(),
            &env,
            &fixture(&bin, &bin),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();

        assert!(env.exe.with_file_name("pbdoc.0.15.0.bak").exists());
        assert!(!env.exe.with_file_name("pbdoc.0.16.0.bak").exists());
    }

    #[test]
    fn the_same_version_still_installs_a_missing_pbdoc() {
        let mut env = env_in("up-same-no-pbdoc");
        env.current_version = "0.17.0".into();
        std::fs::remove_file(pbdoc_of(&env)).unwrap();
        let bin = script_bytes("0.17.0");

        let out = run(
            &Options::default(),
            &env,
            &fixture(&bin, &bin),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();

        assert!(out.out[0].contains("already installed"), "{:?}", out.out);
        assert_eq!(swap::run_version(&pbdoc_of(&env)).unwrap(), "pbdoc 0.17.0");
        assert_eq!(swap::run_version(&env.exe).unwrap(), "playbook 0.16.0");
    }

    #[test]
    fn the_same_version_replaces_a_pbdoc_of_another_version() {
        let mut env = env_in("up-same-old-pbdoc");
        env.current_version = "0.17.0".into();
        let bin = script_bytes("0.17.0");

        run(
            &Options::default(),
            &env,
            &fixture(&bin, &bin),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();

        assert_eq!(swap::run_version(&pbdoc_of(&env)).unwrap(), "pbdoc 0.17.0");
    }

    #[test]
    fn the_same_version_with_a_current_pbdoc_changes_nothing() {
        let mut env = env_in("up-same-pbdoc-ok");
        env.current_version = "0.17.0".into();
        std::fs::write(pbdoc_of(&env), pbdoc_fixture::script("0.17.0")).unwrap();
        std::fs::write(
            pbsync_of(&env),
            pbdoc_fixture::script_of("pbsync", "0.17.0"),
        )
        .unwrap();
        let attest = ok_attest();

        let out = run(
            &Options::default(),
            &env,
            &fixture(b"", b""),
            &attest,
            &swap::run_version,
        )
        .unwrap();

        assert_eq!(out.out.len(), 1, "{:?}", out.out);
        assert!(attest.1.borrow().is_empty());
    }

    #[test]
    fn a_good_pbsync_is_swapped_with_the_playbook_and_backed_up() {
        // Arrange
        let env = env_in("up-pbsync-ok");
        let bin = script_bytes("0.17.0");

        // Act
        let out = run(
            &Options::default(),
            &env,
            &fixture(&bin, &bin),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();

        // Assert
        assert_eq!(
            swap::run_version(&pbsync_of(&env)).unwrap(),
            "pbsync 0.17.0"
        );
        assert!(env.exe.with_file_name("pbsync.0.16.0.bak").exists());
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    }

    #[test]
    fn a_release_without_a_pbsync_asset_still_updates_playbook_and_warns() {
        // Arrange
        let env = env_in("up-pbsync-missing");
        let bin = script_bytes("0.17.0");
        let mut f = fixture(&bin, &bin);
        pbdoc_fixture::remove_of(&mut f, RELEASE, "pbsync");

        // Act
        let out = run(
            &Options::default(),
            &env,
            &f,
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();

        // Assert
        assert_eq!(swap::run_version(&env.exe).unwrap(), "playbook 0.17.0");
        assert_eq!(
            swap::run_version(&pbsync_of(&env)).unwrap(),
            "pbsync 0.16.0"
        );
        assert_eq!(swap::run_version(&pbdoc_of(&env)).unwrap(), "pbdoc 0.17.0");
        assert!(
            out.warnings.iter().any(|w| w.contains("pbsync")),
            "{:?}",
            out.warnings
        );
    }

    #[test]
    fn a_pbsync_with_a_bad_checksum_is_left_untouched() {
        // Arrange
        let env = env_in("up-pbsync-badsum");
        let bin = script_bytes("0.17.0");
        let mut f = fixture(&bin, &bin);
        let tar = pbdoc_fixture::good_of("pbsync", "0.17.0");
        pbdoc_fixture::serve_of(&mut f, RELEASE, "pbsync", &tar, b"other");

        // Act
        let out = run(
            &Options::default(),
            &env,
            &f,
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();

        // Assert
        assert_eq!(
            swap::run_version(&pbsync_of(&env)).unwrap(),
            "pbsync 0.16.0"
        );
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("pbsync") && w.contains("checksum")),
            "{:?}",
            out.warnings
        );
    }

    #[test]
    fn the_same_version_still_installs_a_missing_pbsync() {
        // Arrange
        let mut env = env_in("up-same-no-pbsync");
        env.current_version = "0.17.0".into();
        std::fs::remove_file(pbsync_of(&env)).unwrap();
        let bin = script_bytes("0.17.0");

        // Act
        run(
            &Options::default(),
            &env,
            &fixture(&bin, &bin),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();

        // Assert
        assert_eq!(
            swap::run_version(&pbsync_of(&env)).unwrap(),
            "pbsync 0.17.0"
        );
    }

    #[test]
    fn a_usr_bin_binary_is_refused_with_the_install_routes() {
        let mut env = env_in("up-usr-bin");
        env.exe = PathBuf::from("/usr/bin/playbook");
        let err = run(
            &Options::default(),
            &env,
            &fixture(b"", b""),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap_err();
        assert!(err.contains("retired .deb and .rpm packages"), "{err}");
        assert!(err.contains("install.sh or Homebrew"), "{err}");
        assert!(!err.contains("apt install"), "{err}");
    }

    #[test]
    fn only_a_binary_directly_in_usr_bin_counts_as_a_system_package() {
        assert!(is_system_package(Path::new("/usr/bin/playbook")));
        assert!(!is_system_package(Path::new("/usr/local/bin/playbook")));
        assert!(!is_system_package(Path::new("/home/a/usr/bin/playbook/x")));
    }

    #[test]
    fn windows_is_refused_with_manual_steps() {
        let mut env = env_in("up-win");
        env.windows = true;
        let err = run(
            &Options::default(),
            &env,
            &fixture(b"", b""),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap_err();
        assert!(err.contains("releases/latest"), "{err}");
    }

    #[test]
    fn auto_mode_needs_yes() {
        let mut env = env_in("up-auto");
        env.auto_mode = true;
        let bin = script_bytes("0.17.0");
        let err = run(
            &Options::default(),
            &env,
            &fixture(&bin, &bin),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap_err();
        assert!(err.contains("--yes"), "{err}");
        assert_eq!(swap::run_version(&env.exe).unwrap(), "playbook 0.16.0");
        let opts = Options {
            yes: true,
            ..Options::default()
        };
        run(
            &opts,
            &env,
            &fixture(&bin, &bin),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();
    }

    #[test]
    fn a_shadowing_binary_is_warned_about() {
        let mut env = env_in("up-shadow");
        let other = scratch_dir("up-shadow-first");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("playbook"), script_bytes("0.14.0")).unwrap();
        swap::make_executable(&other.join("playbook")).unwrap();
        let own = env.exe.parent().unwrap().to_path_buf();
        env.path_var = std::env::join_paths([&other, &own]).unwrap();
        let bin = script_bytes("0.17.0");
        let out = run(
            &Options::default(),
            &env,
            &fixture(&bin, &bin),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("runs before the updated")),
            "{:?}",
            out.warnings
        );
    }

    #[test]
    fn strict_mode_reaches_the_attestation_check() {
        let mut env = env_in("up-strict");
        env.strict = true;
        let bin = script_bytes("0.17.0");
        let attest = ok_attest();
        run(
            &Options::default(),
            &env,
            &fixture(&bin, &bin),
            &attest,
            &swap::run_version,
        )
        .unwrap();
        // Once for playbook and once for each helper.
        assert_eq!(*attest.1.borrow(), vec![true, true, true]);
    }

    #[test]
    fn the_same_version_is_a_no_op_even_in_auto_mode_without_yes() {
        let mut env = env_in("up-same");
        env.current_version = "0.17.0".into();
        env.auto_mode = true;
        let out = run(
            &Options::default(),
            &env,
            &fixture(b"", b""),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();
        assert!(out.out[0].contains("already installed"), "{:?}", out.out);
        assert_eq!(swap::run_version(&env.exe).unwrap(), "playbook 0.16.0");
    }

    #[test]
    fn a_binary_newer_than_latest_is_not_downgraded_without_a_version() {
        let mut env = env_in("up-newer");
        env.current_version = "0.19.0".into();
        let out = run(
            &Options::default(),
            &env,
            &fixture(b"", b""),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();
        assert!(out.out[0].contains("nothing to do"), "{:?}", out.out);
    }

    #[test]
    fn an_explicit_older_version_is_installed_as_a_downgrade() {
        let mut env = env_in("up-down");
        env.current_version = "0.19.0".into();
        let bin = script_bytes("0.17.0");
        let opts = Options {
            version: Some("0.17.0".into()),
            ..Options::default()
        };
        run(
            &opts,
            &env,
            &fixture(&bin, &bin),
            &ok_attest(),
            &swap::run_version,
        )
        .unwrap();
        assert_eq!(swap::run_version(&env.exe).unwrap(), "playbook 0.17.0");
    }

    #[test]
    fn check_works_in_auto_mode_without_yes() {
        let mut env = env_in("up-check-auto");
        env.auto_mode = true;
        let opts = Options {
            check: true,
            ..Options::default()
        };
        assert!(run(
            &opts,
            &env,
            &fixture(b"", b""),
            &ok_attest(),
            &swap::run_version
        )
        .is_ok());
    }
}
