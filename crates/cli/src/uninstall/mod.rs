// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook uninstall`: the inverse of `playbook setup` and `install.sh`.
//!
//! It removes only what playbook placed: the hook entries and status line
//! `init` wrote into `settings.json`, the managed launcher block in the rc
//! files, the files `init` copies under `~/.config/playbook`, and the two
//! installer scripts in `~/.claude`, a collector service file an older setup left, and the
//! unedited skill folders `skills sync` wrote. The binary and its rc `PATH`
//! block go only on request. Memory, the data folder with every database, settings the
//! user owns, sessions, history and earlier backups are never touched.

pub mod style;

use crate::init::completion::{self, Shell};
use crate::init::{migrate, rcfile, wire};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// What to remove, resolved once by the caller.
pub struct Options {
    pub home: PathBuf,
    /// Where the installer put the binary.
    pub bin_dir: PathBuf,
    pub remove_binary: bool,
    pub dry_run: bool,
}

/// One line per action, plus the steps that failed.
#[derive(Debug, Default)]
pub struct Report {
    pub lines: Vec<String>,
    pub errors: Vec<String>,
}

/// Files `init` copies under `~/.config/playbook`, relative to it, with the
/// migration key that tracks user edits where one exists.
const PLACED: &[(&str, Option<&str>)] = &[
    ("statusline.sh", Some("statusline")),
    ("prompts/SYSTEM_PROMPT.md", Some("system-prompt")),
    ("hooks/lib/config-hash.sh", None),
    ("plugin-seen", None),
];

/// Directories earlier installs filled, removed whole.
const LEGACY_DIRS: &[&str] = &["shell"];

/// Files the installer copies into `~/.claude`.
const CLAUDE_HOME_FILES: &[&str] = &["install.sh", "uninstall.sh"];

pub fn run(opts: &Options) -> Report {
    let mut report = Report::default();
    let verb = if opts.dry_run {
        "would remove"
    } else {
        "removed"
    };
    let claude_home = opts.home.join(".claude");
    let stamp = crate::common::time::now_epoch_secs();

    let settings = settings_step(opts, &claude_home, stamp, &mut report);
    codex_hooks_step(&opts.home, stamp, opts.dry_run, &mut report);
    rc_step(opts, stamp, &mut report);
    completion_step(
        &opts.home,
        completion::effective_uid(),
        stamp,
        opts.dry_run,
        &mut report,
    );
    // The service runs the binary, so it goes before the binary can.
    match crate::init::collector_service::remove(&opts.home, opts.dry_run) {
        Ok(Some(line)) => report.lines.push(line),
        Ok(None) => {}
        Err(err) => report
            .errors
            .push(format!("collector service left in place: {err}")),
    }
    for foreign in crate::init::collector_service::foreign_files(&opts.home) {
        report.lines.push(format!(
            "{}; to remove it, run: {}",
            foreign.sentence(),
            foreign.fix
        ));
    }
    if !settings.ok {
        report.lines.push(
            "kept the placed files and the binary, since settings.json still refers to them"
                .to_string(),
        );
        remove_claude_home_files(&claude_home, verb, opts.dry_run, &mut report);
        manual_steps(&opts.home, &mut report);
        return report;
    }
    let status_line_in_use = !settings.statusline_removed
        && fs::read_to_string(claude_home.join("settings.json"))
            .is_ok_and(|text| text.contains("playbook/statusline.sh"));

    let config_dir = opts.home.join(".config").join("playbook");
    for (rel, key) in PLACED {
        let path = config_dir.join(rel);
        if !path.is_file() {
            continue;
        }
        if key.is_some_and(|k| !migrate::placed_unchanged(&opts.home, k, &path)) {
            report.lines.push(format!(
                "kept {} (edited, or not recorded as placed by playbook)",
                path.display()
            ));
            continue;
        }
        if *rel == "statusline.sh" && status_line_in_use {
            report.lines.push(format!(
                "kept {} (your status line still runs it)",
                path.display()
            ));
            continue;
        }
        remove_file(&path, verb, opts.dry_run, &mut report);
    }
    for dir in LEGACY_DIRS {
        let path = config_dir.join(dir);
        if path.is_dir() {
            remove_dir_all(&path, verb, opts.dry_run, &mut report);
        }
    }
    if !opts.dry_run {
        for rel in ["prompts", "hooks/lib", "hooks"] {
            let _ = fs::remove_dir(config_dir.join(rel));
        }
    }
    remove_claude_home_files(&claude_home, verb, opts.dry_run, &mut report);
    synced_skills_step(opts, &mut report);

    if opts.remove_binary {
        binary_step(opts, verb, &mut report);
    }
    manual_steps(&opts.home, &mut report);
    report
}

/// The skill folders `playbook skills sync` wrote for other agents. Only the
/// ones whose files still match their record go; the rest are listed as kept.
fn synced_skills_step(opts: &Options, report: &mut Report) {
    let root = crate::common::paths::playbook_root_from(&opts.home);
    let target_root = opts.home.join(crate::skills_sync::TARGET_DIR);
    let removed = crate::skills_sync::apply::remove_synced(&root, &target_root, opts.dry_run);
    report.lines.extend(removed.lines);
    report.errors.extend(removed.errors);
}

/// The steps uninstall never runs for the user: it never removes a plugin or
/// a marketplace, even though `playbook setup` may have installed one with
/// the user's consent. Only the ones that still apply are listed.
fn manual_steps(home: &Path, report: &mut Report) {
    use crate::common::plugin_state::{
        hooks_plugin_keys, known_marketplaces, listed_in, plugin_keys,
    };
    let mut steps = Vec::new();
    for key in plugin_keys().into_iter().chain(hooks_plugin_keys()) {
        if listed_in(home, &key) == Some(true) {
            steps.push(format!("claude plugin uninstall {key}"));
        }
    }
    for name in known_marketplaces(home) {
        steps.push(format!("claude plugin marketplace remove {name}"));
    }
    if steps.is_empty() {
        return;
    }
    report.lines.push(
        "remaining manual steps (playbook does not change Claude Code's plugins):".to_string(),
    );
    report
        .lines
        .extend(steps.into_iter().map(|s| format!("  {s}")));
}

fn remove_claude_home_files(claude_home: &Path, verb: &str, dry_run: bool, report: &mut Report) {
    for name in CLAUDE_HOME_FILES {
        let path = claude_home.join(name);
        if path.is_file() {
            remove_file(&path, verb, dry_run, report);
        }
    }
}

/// How the `settings.json` step ended.
struct SettingsResult {
    /// False when the file could not be edited, so it may still name placed files.
    ok: bool,
    statusline_removed: bool,
}

fn settings_step(
    opts: &Options,
    claude_home: &Path,
    stamp: u64,
    report: &mut Report,
) -> SettingsResult {
    let done = |statusline_removed| SettingsResult {
        ok: true,
        statusline_removed,
    };
    let path = claude_home.join("settings.json");
    if !path.is_file() {
        return done(false);
    }
    let verb = if opts.dry_run {
        "would remove"
    } else {
        "removed"
    };
    match crate::init::telemetry::remove(&opts.home, &path, stamp, opts.dry_run) {
        Ok(out) if out.keys > 0 => {
            report.lines.push(format!(
                "{verb} {} telemetry variables that `playbook setup` wrote from {}",
                out.keys,
                path.display()
            ));
            if let Some(backup) = out.backup {
                report.lines.push(format!("backup: {}", backup.display()));
            }
        }
        Ok(_) => {}
        Err(err) => report
            .errors
            .push(format!("telemetry variables left unchanged: {err}")),
    }
    // Setup may have written it, but the user may also want it: say so, keep it.
    if crate::init::attribution::state(&path) == crate::init::attribution::State::Off {
        let how = match crate::init::attribution::held_off(&path).is_empty() {
            true => "delete the key by hand".to_string(),
            false => format!(
                "run `{}` while playbook is still installed, or delete the key by hand",
                crate::init::attribution::ON_COMMAND
            ),
        };
        report.lines.push(format!(
            "kept the attribution key in {}; to get Claude Code's attribution back, {how}",
            path.display()
        ));
    }
    match wire::unwire_at(&path, &opts.home, stamp, opts.dry_run) {
        Ok(out) if out.hooks_removed == 0 && !out.statusline_removed => done(false),
        Ok(out) => {
            let verb = if opts.dry_run {
                "would remove"
            } else {
                "removed"
            };
            let mut what = format!("{} hook entries", out.hooks_removed);
            if out.statusline_removed {
                what.push_str(" and the status line");
            }
            report
                .lines
                .push(format!("{verb} {what} from {}", path.display()));
            if let Some(backup) = out.backup_path {
                report.lines.push(format!("backup: {}", backup.display()));
            }
            done(out.statusline_removed)
        }
        Err(err) => {
            report
                .errors
                .push(format!("settings.json left unchanged: {err}"));
            SettingsResult {
                ok: false,
                statusline_removed: false,
            }
        }
    }
}

/// Removes the guards `playbook setup --codex-hooks` put in `~/.codex/hooks.json`
/// and keeps every other entry. A failure is reported and stops nothing else.
fn codex_hooks_step(home: &Path, stamp: u64, dry_run: bool, report: &mut Report) {
    let file = crate::init::codex_hooks::hooks_path(home);
    if !file.is_file() {
        return;
    }
    let verb = if dry_run { "would remove" } else { "removed" };
    match crate::init::codex_hooks::unwire(&file, stamp, dry_run) {
        Ok(0) => {}
        Ok(n) => report
            .lines
            .push(format!("{verb} {n} hook entries from {}", file.display())),
        Err(err) => report
            .errors
            .push(format!("{} left unchanged: {err}", file.display())),
    }
}

/// Removes the completion stubs, the zsh loader and the `~/.zshrc` line
/// `playbook setup` wrote, keeping any the user edited. Runs with or without
/// the binary, since the stubs and the loader are silent when it is gone.
fn completion_step(home: &Path, uid: u32, stamp: u64, dry_run: bool, report: &mut Report) {
    let verb = if dry_run { "would remove" } else { "removed" };
    for shell in [Shell::Bash, Shell::Fish] {
        stub_files_step(home, shell, verb, dry_run, report);
    }
    legacy_stub_step(home, verb, dry_run, report);
    loader_step(home, verb, dry_run, report);
    zshrc_line_step(home, uid, stamp, dry_run, report);
    keep_zshrc_backups(home, stamp, report);
}

fn stub_files_step(home: &Path, shell: Shell, verb: &str, dry_run: bool, report: &mut Report) {
    let ours = completion::stub_text(shell);
    for stub in completion::stub_paths(home, shell) {
        let Ok(bytes) = fs::read(&stub) else { continue };
        if bytes == ours.as_bytes() {
            remove_file(&stub, verb, dry_run, report);
        } else if String::from_utf8_lossy(&bytes)
            .lines()
            .any(|l| l == completion::STUB_MARKER)
        {
            report
                .lines
                .push(format!("kept {} (edited)", stub.display()));
        }
    }
}

/// Removes the pre-loader zsh stub when it carries our marker, and its
/// directory when that leaves it empty.
fn legacy_stub_step(home: &Path, verb: &str, dry_run: bool, report: &mut Report) {
    let stub = home.join(completion::LEGACY_STUB);
    let Ok(bytes) = fs::read(&stub) else { return };
    if !completion::legacy_stub_is_ours(&bytes) {
        return;
    }
    remove_file(&stub, verb, dry_run, report);
    if let (false, Some(dir)) = (dry_run, stub.parent()) {
        let _ = fs::remove_dir(dir);
    }
}

/// Removes the loader when it carries our header, and its directory when that
/// leaves it empty. A file without the header is not ours and is kept.
fn loader_step(home: &Path, verb: &str, dry_run: bool, report: &mut Report) {
    let loader = completion::loader_path(home);
    let Ok(bytes) = fs::read(&loader) else { return };
    if !completion::loader_is_ours(&bytes) {
        report
            .lines
            .push(format!("kept {} (not ours)", loader.display()));
        return;
    }
    remove_file(&loader, verb, dry_run, report);
    if let (false, Some(dir)) = (dry_run, loader.parent()) {
        let _ = fs::remove_dir(dir);
    }
}

fn zshrc_line_step(home: &Path, uid: u32, stamp: u64, dry_run: bool, report: &mut Report) {
    let rc = home.join(completion::ZSHRC);
    for change in completion::strip(home, uid, stamp, dry_run) {
        let name = change.rc_file.display();
        if change.unwritable {
            report.errors.push(format!(
                "{name} is read-only; remove the playbook completion line by hand"
            ));
        } else if let Some(err) = change.error {
            report.errors.push(format!("{name} left unchanged: {err}"));
        } else if let Some(backup) = change.backup {
            report.lines.push(format!(
                "removed the playbook completion line from {name} (backup: {})",
                backup.display()
            ));
        } else {
            report.lines.push(format!(
                "would remove the playbook completion line from {name}"
            ));
        }
    }
    // Whatever the strip leaves behind is a marker line whose text was edited.
    let Ok(old) = fs::read(&rc) else { return };
    let left = completion::strip_ours(&old).unwrap_or(old);
    if left
        .split(|&b| b == b'\n')
        .any(|l| l.trim_ascii_end().ends_with(completion::MARKER.as_bytes()))
    {
        report.lines.push(format!("kept {} (edited)", rc.display()));
    }
}

/// Backups of `~/.zshrc` are never removed; say so, so the user can find them.
/// The one this run just made is already reported where it was taken.
fn keep_zshrc_backups(home: &Path, stamp: u64, report: &mut Report) {
    let prefix = format!("{}{}", completion::ZSHRC, completion::BACKUP_INFIX);
    let own = format!("{prefix}{stamp}");
    let Ok(entries) = fs::read_dir(home) else {
        return;
    };
    let mut backups: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with(&prefix) && name != own
        })
        .map(|e| e.path())
        .collect();
    backups.sort();
    for backup in backups {
        report
            .lines
            .push(format!("kept {} (earlier backup)", backup.display()));
    }
}

fn rc_step(opts: &Options, stamp: u64, report: &mut Report) {
    let mut changes = rcfile::strip_rc_files(&opts.home, stamp, opts.remove_binary, opts.dry_run);
    if opts.remove_binary {
        changes.extend(crate::init::path::strip(&opts.home, stamp, opts.dry_run));
    }
    for change in changes {
        let name = change.rc_file.display();
        if change.unwritable {
            report.errors.push(format!(
                "{name} is read-only; remove the playbook lines by hand"
            ));
        } else if let Some(err) = change.error {
            report.errors.push(format!("{name} left unchanged: {err}"));
        } else if let Some(backup) = change.backup {
            report.lines.push(format!(
                "removed the playbook lines from {name} (backup: {})",
                backup.display()
            ));
        } else {
            report
                .lines
                .push(format!("would remove the playbook lines from {name}"));
        }
    }
}

/// Binaries the installer puts in the bin directory, each with the
/// `<name>.<version>.bak` copies an update leaves beside it.
const BINARIES: [&str; 3] = ["playbook", "pbdoc", "pbsync"];

fn binary_step(opts: &Options, verb: &str, report: &mut Report) {
    for name in BINARIES {
        let binary = opts.bin_dir.join(name);
        if binary.is_file() {
            remove_file(&binary, verb, opts.dry_run, report);
        }
        if name == "playbook" {
            remove_pb_link(&opts.bin_dir, &binary, verb, opts.dry_run, report);
        }
        remove_backups(&opts.bin_dir, name, verb, opts.dry_run, report);
    }
}

fn remove_backups(bin_dir: &Path, name: &str, verb: &str, dry_run: bool, report: &mut Report) {
    let Ok(entries) = fs::read_dir(bin_dir) else {
        return;
    };
    let prefix = format!("{name}.");
    let mut backups: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".bak"))
        })
        .collect();
    backups.sort();
    for backup in backups {
        remove_file(&backup, verb, dry_run, report);
    }
}

/// The `pb` short name the installer links next to the binary. It goes only
/// when it is a symlink to `playbook` (relative or absolute), so a `pb` of the
/// user's own stays.
fn remove_pb_link(bin_dir: &Path, binary: &Path, verb: &str, dry_run: bool, report: &mut Report) {
    let link = bin_dir.join("pb");
    let Ok(target) = fs::read_link(&link) else {
        return;
    };
    if target == Path::new("playbook") || target == binary {
        remove_file(&link, verb, dry_run, report);
    }
}

fn remove_file(path: &Path, verb: &str, dry_run: bool, report: &mut Report) {
    let result = if dry_run {
        Ok(())
    } else {
        fs::remove_file(path)
    };
    settle(path, verb, dry_run, report, result);
}

fn remove_dir_all(path: &Path, verb: &str, dry_run: bool, report: &mut Report) {
    let result = if dry_run {
        Ok(())
    } else {
        fs::remove_dir_all(path)
    };
    settle(path, verb, dry_run, report, result);
}

fn settle(path: &Path, verb: &str, dry_run: bool, report: &mut Report, result: io::Result<()>) {
    if dry_run {
        report.lines.push(format!("{verb} {}", path.display()));
        return;
    }
    match result {
        Ok(()) => report.lines.push(format!("{verb} {}", path.display())),
        Err(err) => report
            .errors
            .push(format!("could not remove {}: {err}", path.display())),
    }
}

/// The install directory from `$PLAYBOOK_BIN_DIR`, else `~/.local/bin`. Setup
/// links `pb` only here, so uninstall finds every link setup made.
pub fn install_dir(home: &Path) -> PathBuf {
    default_bin_dir(home, std::env::var("PLAYBOOK_BIN_DIR").ok().as_deref())
}

/// The default install directory, overridable like the installer does.
pub fn default_bin_dir(home: &Path, env: Option<&str>) -> PathBuf {
    match env {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => home.join(".local").join("bin"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bin_dir_with(files: &[&str], tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pb-uninstall-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for name in files {
            fs::write(dir.join(name), "x").unwrap();
        }
        dir
    }

    fn opts(bin_dir: &Path, dry_run: bool) -> Options {
        Options {
            home: bin_dir.to_path_buf(),
            bin_dir: bin_dir.to_path_buf(),
            remove_binary: true,
            dry_run,
        }
    }

    const INSTALLED: [&str; 6] = [
        "playbook",
        "pbdoc",
        "pbsync",
        "pbdoc.0.1.0.bak",
        "pbsync.0.1.0.bak",
        "playbook.0.1.0.bak",
    ];

    #[test]
    fn dry_run_lists_pbdoc_and_its_backups_without_removing_them() {
        // Arrange
        let dir = bin_dir_with(&INSTALLED, "dry");
        let mut report = Report::default();

        // Act
        binary_step(&opts(&dir, true), "would remove", &mut report);

        // Assert
        let listed = report.lines.join("\n");
        assert!(listed.contains("would remove") && listed.contains("pbdoc"));
        assert!(listed.contains("pbdoc.0.1.0.bak"));
        assert!(listed.contains("pbsync") && listed.contains("pbsync.0.1.0.bak"));
        assert!(dir.join("pbdoc").exists() && dir.join("pbdoc.0.1.0.bak").exists());
        assert!(dir.join("pbsync").exists() && dir.join("pbsync.0.1.0.bak").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn real_run_removes_pbdoc_and_its_backups_and_keeps_other_files() {
        // Arrange
        let mut files = INSTALLED.to_vec();
        files.push("pbdocs-notes");
        let dir = bin_dir_with(&files, "real");
        let mut report = Report::default();

        // Act
        binary_step(&opts(&dir, false), "removed", &mut report);

        // Assert
        for name in INSTALLED {
            assert!(!dir.join(name).exists(), "{name} should be gone");
        }
        assert!(dir.join("pbdocs-notes").exists());
        assert!(report.errors.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    mod completion {
        use super::*;
        use crate::init::completion::{self, Shell, MARKER, RC_LINE, SKIP_OTHER_OWNER, ZSHRC};
        use std::os::unix::fs::{symlink, MetadataExt};

        const STAMP: u64 = 1_700_000_000;
        const LATER_STAMP: u64 = 1_700_000_500;
        const BEFORE: &str = "export EDITOR=vim\n";
        const AFTER: &str = "alias ll='ls -l'\n";
        const EDITED_RC_LINE: &str =
            "[ -r \"$HOME/mine.zsh\" ] && . \"$HOME/mine.zsh\"  # playbook completions\n";
        const THEIR_LOADER: &str = "# somebody else's loader\n";
        const EDITED_SUFFIX: &str = "# my own tweak\n";
        const KEPT_EDITED: &str = "kept";
        const EDITED_WORD: &str = "(edited)";
        const EARLIER_BACKUP: &str = "(earlier backup)";
        const OLD_MTIME_SECS: u64 = 86_400;

        fn home(tag: &str) -> PathBuf {
            let dir = crate::common::test_support::scratch_dir(tag);
            fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn uid_of(path: &Path) -> u32 {
            fs::metadata(path).unwrap().uid()
        }

        fn block() -> String {
            format!("{RC_LINE}\n")
        }

        fn backup_name(stamp: u64) -> String {
            format!("{ZSHRC}.bak-{stamp}")
        }

        fn backup(home: &Path, stamp: u64) -> PathBuf {
            home.join(backup_name(stamp))
        }

        fn backups(home: &Path) -> Vec<String> {
            fs::read_dir(home)
                .unwrap()
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with(&format!("{ZSHRC}.bak-")))
                .collect()
        }

        fn run_step(home: &Path, uid: u32, stamp: u64) -> Report {
            let mut report = Report::default();
            completion_step(home, uid, stamp, false, &mut report);
            report
        }

        fn said(report: &Report) -> String {
            report
                .lines
                .iter()
                .chain(&report.errors)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        }

        fn write_zshrc(home: &Path, content: &str) {
            fs::write(home.join(ZSHRC), content).unwrap();
        }

        fn read(path: &Path) -> String {
            fs::read_to_string(path).unwrap_or_else(|e| format!("<unreadable: {e}>"))
        }

        fn marker_lines(content: &str) -> usize {
            content.lines().filter(|l| l.ends_with(MARKER)).count()
        }

        #[test]
        fn uninstall_strips_the_exact_line_and_keeps_user_lines_byte_for_byte() {
            // Arrange
            let home = home("uninstall-completion-strip");
            let original = format!("{BEFORE}{}{AFTER}", block());
            write_zshrc(&home, &original);

            // Act
            run_step(&home, uid_of(&home), STAMP);

            // Assert
            assert_eq!(read(&home.join(ZSHRC)), format!("{BEFORE}{AFTER}"));
            assert_eq!(read(&backup(&home, STAMP)), original);
        }

        #[test]
        fn uninstall_strips_each_exact_line() {
            // Arrange
            let home = home("uninstall-completion-two");
            write_zshrc(&home, &format!("{BEFORE}{}{AFTER}{}", block(), block()));

            // Act
            run_step(&home, uid_of(&home), STAMP);

            // Assert
            let after = read(&home.join(ZSHRC));
            assert_eq!(marker_lines(&after), 0, "{after}");
            assert_eq!(after, format!("{BEFORE}{AFTER}"));
        }

        #[test]
        fn uninstall_keeps_an_edited_marker_line_and_strips_the_exact_one() {
            // Arrange
            let home = home("uninstall-completion-mixed");
            write_zshrc(
                &home,
                &format!("{BEFORE}{EDITED_RC_LINE}{AFTER}{}", block()),
            );

            // Act
            let report = run_step(&home, uid_of(&home), STAMP);

            // Assert
            let after = read(&home.join(ZSHRC));
            assert_eq!(after, format!("{BEFORE}{EDITED_RC_LINE}{AFTER}"));
            let said = said(&report);
            assert!(
                said.contains(KEPT_EDITED) && said.contains(EDITED_WORD),
                "{said}"
            );
        }

        #[test]
        fn uninstall_removes_the_stubs_including_pb_and_keeps_an_edited_stub() {
            // Arrange
            let home = home("uninstall-completion-stubs");
            for shell in [Shell::Bash, Shell::Fish] {
                completion::ensure(&home, shell).unwrap();
            }
            let edited = completion::stub_paths(&home, Shell::Bash).pop().unwrap();
            let mut text = read(&edited);
            text.push_str(EDITED_SUFFIX);
            fs::write(&edited, text).unwrap();

            // Act
            let report = run_step(&home, uid_of(&home), STAMP);

            // Assert
            for shell in [Shell::Bash, Shell::Fish] {
                for stub in completion::stub_paths(&home, shell) {
                    assert_eq!(stub == edited, stub.exists(), "{}", stub.display());
                }
            }
            let said = said(&report);
            assert!(
                said.contains(KEPT_EDITED) && said.contains(EDITED_WORD),
                "{said}"
            );
        }

        #[test]
        fn uninstall_removes_our_loader_and_an_empty_dir_and_keeps_a_foreign_loader() {
            // Arrange
            let ours = home("uninstall-completion-loader");
            completion::ensure(&ours, Shell::Zsh).unwrap();
            let theirs = home("uninstall-completion-loader-foreign");
            completion::ensure(&theirs, Shell::Zsh).unwrap();
            fs::write(completion::loader_path(&theirs), THEIR_LOADER).unwrap();

            // Act
            run_step(&ours, uid_of(&ours), STAMP);
            let report = run_step(&theirs, uid_of(&theirs), STAMP);

            // Assert
            let loader = completion::loader_path(&ours);
            assert!(!loader.exists());
            assert!(!loader.parent().unwrap().exists());
            assert_eq!(read(&completion::loader_path(&theirs)), THEIR_LOADER);
            let said = said(&report);
            assert!(
                said.contains("kept") && said.contains("(not ours)"),
                "{said}"
            );
        }

        #[test]
        fn uninstall_removes_the_pre_loader_stub_and_block_and_keeps_a_foreign_file() {
            // Arrange
            let home = home("uninstall-completion-legacy");
            let dir = home.join(completion::LEGACY_STUB);
            let dir = dir.parent().unwrap().to_path_buf();
            fs::create_dir_all(&dir).unwrap();
            let stub = home.join(completion::LEGACY_STUB);
            fs::write(
                &stub,
                format!("#compdef playbook pb\n{}\n", completion::STUB_MARKER),
            )
            .unwrap();
            let old_block = format!(
                "{MARKER}\n{}\n{}\n",
                completion::LEGACY_BLOCK_REST[0],
                completion::LEGACY_BLOCK_REST[1]
            );
            write_zshrc(&home, &format!("{BEFORE}{old_block}{AFTER}"));
            let other = home.join("other");
            fs::create_dir_all(&other).unwrap();
            let other_stub = other.join(completion::LEGACY_STUB);
            fs::create_dir_all(other_stub.parent().unwrap()).unwrap();
            fs::write(&other_stub, "# not ours\n").unwrap();

            // Act
            run_step(&home, uid_of(&home), STAMP);
            run_step(&other, uid_of(&other), STAMP);

            // Assert
            assert!(!stub.exists() && !dir.exists());
            assert_eq!(read(&home.join(ZSHRC)), format!("{BEFORE}{AFTER}"));
            assert!(other_stub.exists());
        }

        #[test]
        fn a_dry_run_removes_nothing_and_says_what_it_would_remove() {
            // Arrange
            let home = home("uninstall-completion-dry");
            completion::ensure(&home, Shell::Zsh).unwrap();
            let rc_before = read(&home.join(ZSHRC));
            let mut report = Report::default();

            // Act
            completion_step(&home, uid_of(&home), STAMP, true, &mut report);

            // Assert
            assert!(completion::loader_path(&home).exists());
            assert_eq!(read(&home.join(ZSHRC)), rc_before);
            assert!(backups(&home).is_empty(), "{:?}", backups(&home));
            let said = said(&report);
            assert!(
                said.contains("would remove") && said.contains("completions.zsh"),
                "{said}"
            );
        }

        #[test]
        fn uninstall_keeps_the_zshrc_backup_and_its_bytes_equal_the_original() {
            // Arrange
            let home = home("uninstall-completion-backup");
            let original = format!("{BEFORE}{}{AFTER}", block());
            write_zshrc(&home, &original);
            run_step(&home, uid_of(&home), STAMP);
            let opts = Options {
                home: home.clone(),
                bin_dir: home.join(".local/bin"),
                remove_binary: true,
                dry_run: false,
            };

            // Act
            let second = run_step(&home, uid_of(&home), LATER_STAMP);
            run(&opts);

            // Assert
            assert_eq!(read(&backup(&home, STAMP)), original);
            assert_eq!(backups(&home), vec![backup_name(STAMP)]);
            let said = said(&second);
            assert!(
                said.contains(KEPT_EDITED) && said.contains(&backup_name(STAMP)),
                "{said}"
            );
        }

        #[test]
        fn run_keeps_the_original_rc_when_launcher_and_completion_strips_share_a_stamp() {
            // Arrange
            let home = home("uninstall-run-shared-backup");
            let original = format!("{BEFORE}{}\n{}{AFTER}", rcfile::SOURCE_LINE, block());
            write_zshrc(&home, &original);
            let opts = Options {
                home: home.clone(),
                bin_dir: home.join(".local/bin"),
                remove_binary: true,
                dry_run: false,
            };

            // Act
            run(&opts);

            // Assert
            let names = backups(&home);
            assert_eq!(names.len(), 1, "{names:?}");
            assert_eq!(read(&home.join(&names[0])), original);
            assert_eq!(read(&home.join(ZSHRC)), format!("{BEFORE}{AFTER}"));
        }

        #[test]
        fn run_does_not_call_its_own_backup_an_earlier_one() {
            // Arrange
            let home = home("uninstall-run-own-backup");
            write_zshrc(&home, &format!("{BEFORE}{}{AFTER}", block()));
            let opts = Options {
                home: home.clone(),
                bin_dir: home.join(".local/bin"),
                remove_binary: true,
                dry_run: false,
            };

            // Act
            let report = run(&opts);

            // Assert
            assert_eq!(backups(&home).len(), 1);
            assert!(!said(&report).contains(EARLIER_BACKUP), "{}", said(&report));
        }

        #[test]
        fn uninstall_with_no_block_writes_nothing_and_takes_no_backup() {
            // Arrange
            let home = home("uninstall-completion-noblock");
            let rc = home.join(ZSHRC);
            write_zshrc(&home, &format!("{BEFORE}{AFTER}"));
            let old =
                std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(OLD_MTIME_SECS);
            fs::File::options()
                .write(true)
                .open(&rc)
                .unwrap()
                .set_modified(old)
                .unwrap();

            // Act
            run_step(&home, uid_of(&home), STAMP);

            // Assert
            assert_eq!(read(&rc), format!("{BEFORE}{AFTER}"));
            assert_eq!(fs::metadata(&rc).unwrap().modified().unwrap(), old);
            assert!(backups(&home).is_empty(), "{:?}", backups(&home));
        }

        #[test]
        fn uninstall_keeps_a_symlinked_zshrc_a_symlink_and_strips_only_the_target() {
            // Arrange
            let home = home("uninstall-completion-symlink");
            let dotfiles = home.join("dotfiles");
            fs::create_dir_all(&dotfiles).unwrap();
            let target = dotfiles.join("zshrc");
            fs::write(&target, format!("{BEFORE}{}{AFTER}", block())).unwrap();
            let rc = home.join(ZSHRC);
            symlink(&target, &rc).unwrap();

            // Act
            run_step(&home, uid_of(&home), STAMP);

            // Assert
            assert!(fs::symlink_metadata(&rc).unwrap().is_symlink());
            assert_eq!(fs::read_link(&rc).unwrap(), target);
            assert_eq!(read(&target), format!("{BEFORE}{AFTER}"));
        }

        #[test]
        fn uninstall_skips_an_rc_owned_by_another_user_with_a_reason() {
            // Arrange
            let home = home("uninstall-completion-owner");
            let original = format!("{BEFORE}{}{AFTER}", block());
            write_zshrc(&home, &original);
            let other_uid = uid_of(&home) + 1;

            // Act
            let report = run_step(&home, other_uid, STAMP);

            // Assert
            assert_eq!(read(&home.join(ZSHRC)), original);
            assert!(backups(&home).is_empty(), "{:?}", backups(&home));
            let said = said(&report);
            assert!(said.contains(SKIP_OTHER_OWNER), "{said}");
        }
    }
}
