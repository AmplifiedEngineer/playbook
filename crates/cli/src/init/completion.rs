// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Wires tab completion for `playbook` and `pb` into the user's shell files
//! (ADR 0054): small loader stubs in the bash and fish autoload directories,
//! and for zsh a loader file plus one marked line in `~/.zshrc` that sources it.

use crate::common::atomic::write_atomic_durable;
use crate::init::path::PathShell;
use crate::init::rcfile::{self, RcStrip};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

/// Trailing comment that marks the zsh line in `~/.zshrc` as ours.
pub const MARKER: &str = "# playbook completions";
/// The one line `playbook setup` adds to `~/.zshrc`.
pub const RC_LINE: &str = r#"[ -r "$HOME/.local/share/playbook/completions.zsh" ] && . "$HOME/.local/share/playbook/completions.zsh"  # playbook completions"#;
pub const ZSHRC: &str = ".zshrc";
/// The fpath stub earlier versions wrote for zsh, relative to `$HOME`.
pub const LEGACY_STUB: &str = ".local/share/zsh/site-functions/_playbook";
/// The two lines after [`MARKER`] in the `~/.zshrc` block earlier versions
/// wrote for zsh. Setup migrates that block and uninstall removes it.
pub const LEGACY_BLOCK_REST: [&str; 2] = [
    r#"fpath=("$HOME/.local/share/zsh/site-functions" $fpath)"#,
    "if (( $+functions[compdef] )); then autoload -Uz _playbook; compdef _playbook playbook; [[ ${commands[pb]} -ef ${commands[playbook]} ]] && compdef _playbook pb; else autoload -Uz compinit && compinit; fi",
];
/// The zsh loader, relative to `$HOME`.
pub const LOADER: &str = ".local/share/playbook/completions.zsh";
/// First line of the loader, which tells our file from a user's.
pub const LOADER_HEADER: &str =
    "# Written by playbook setup; playbook upgrade rewrites it. Edits are lost.";
/// The loader after its header. It registers a stub that loads the real
/// function on the first Tab, so shell startup pays nothing.
pub const LOADER_BODY: [&str; 7] = [
    "(( $+commands[playbook] )) || return 0",
    "(( $+functions[compdef] )) || { autoload -Uz compinit && compinit -C; } 2>/dev/null",
    "(( $+functions[compdef] )) || return 0",
    r#"_playbook() { unfunction _playbook; source <(playbook completions zsh 2>/dev/null); _playbook "$@"; }"#,
    "compdef _playbook playbook",
    "if [[ ${commands[pb]} -ef ${commands[playbook]} ]]; then compdef _playbook pb; fi",
    "return 0",
];
pub const BASH_COMPLETIONS: &str = ".local/share/bash-completion/completions";
pub const FISH_COMPLETIONS: &str = ".config/fish/completions";
pub const BINARY_NAME: &str = "playbook";
pub const ALIAS_NAME: &str = "pb";
pub const FISH_SUFFIX: &str = ".fish";
/// Mode of a `~/.zshrc` this module creates; an existing file keeps its own.
pub const RC_NEW_MODE: u32 = 0o644;
/// Mode of the directories this module creates, whatever a loose umask says.
#[cfg(unix)]
const DIR_MODE: u32 = 0o755;
pub const SKIP_DANGLING: &str = "dangling symlink";
pub const SKIP_OTHER_OWNER: &str = "owned by another user";
pub const SKIP_READ_ONLY: &str = "read-only";
pub const SKIP_NOT_OURS: &str = "not written by playbook";
pub const SKIP_UNREADABLE: &str = "unreadable";
pub const SKIP_UNKNOWN_SHELL: &str = "unknown or unset $SHELL";
pub const BACKUP_INFIX: &str = ".bak-";
/// Second line of every stub, which tells our files from a user's.
pub const STUB_MARKER: &str = "# playbook completions (written by playbook setup)";
const BASH_STUB_BODY: &str = "command -v playbook >/dev/null 2>&1 || return 0\n\
source <(playbook completions bash 2>/dev/null) 2>/dev/null || return 0\n";
// The script is captured first so a broken `playbook` that prints on stdout
// and fails is never sourced.
const FISH_STUB_BODY: &str = "if command -q playbook\n\
\x20   set -l script (playbook completions fish 2>/dev/null)\n\
\x20   if test $status -eq 0; and test -n \"$script\"\n\
\x20       printf '%s\\n' $script | source\n\
\x20   end\n\
end\n";
/// The shell the completion files are written for.
pub type Shell = PathShell;

/// What the caller resolved: the shell to write completion for.
#[derive(Debug, Clone, Copy)]
pub struct Setup {
    pub shell: Shell,
}

/// What `ensure` did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Files this call created.
    pub created: Vec<PathBuf>,
    /// Files this call changed.
    pub changed: Vec<PathBuf>,
    /// Files this call removed.
    pub removed: Vec<PathBuf>,
    /// Files that already held what setup would write.
    pub already: Vec<PathBuf>,
    /// Files with an edited line or stub, left alone.
    pub kept: Vec<PathBuf>,
    /// Files left alone, with the reason.
    pub skipped: Vec<(PathBuf, String)>,
}

/// The effective user and a hook for tests.
pub struct Ctx<'a> {
    /// The uid a file must belong to for setup to rewrite it.
    pub uid: u32,
    /// Called with the rc path between the read and the write, on every path.
    pub between: &'a dyn Fn(&Path),
}

/// What doctor reports for one shell.
#[derive(Debug, PartialEq, Eq)]
pub enum Status {
    /// No stub or loader, or no line that sources it.
    Missing,
    Ok,
    /// A stub or loader path holds a file without our marker.
    StubNotOurs,
    /// The `~/.zshrc` line carries our marker but its text was changed.
    LineEdited,
    /// The exact `~/.zshrc` line appears more than once.
    LineDuplicated,
}

/// Writes the completion stubs for `shell` under `home`, and for zsh the
/// loader and its line in `~/.zshrc`, as the current user. Never overwrites a file it did
/// not write.
pub fn ensure(home: &Path, shell: Shell) -> io::Result<Outcome> {
    ensure_with(
        home,
        shell,
        &Ctx {
            uid: effective_uid(),
            between: &|_| {},
        },
    )
}

#[cfg(unix)]
pub fn effective_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Windows has no uid, and files there are not owner-checked.
#[cfg(not(unix))]
pub fn effective_uid() -> u32 {
    0
}

#[cfg(unix)]
fn owned_by(meta: &fs::Metadata, uid: u32) -> bool {
    use std::os::unix::fs::MetadataExt;
    meta.uid() == uid
}

#[cfg(not(unix))]
fn owned_by(_meta: &fs::Metadata, _uid: u32) -> bool {
    true
}

/// [`ensure`] with an explicit effective uid and a test hook. A file or
/// directory is created only under a `home` that uid owns.
pub fn ensure_with(home: &Path, shell: Shell, ctx: &Ctx) -> io::Result<Outcome> {
    let mut out = Outcome::default();
    if shell == Shell::Other {
        out.skipped
            .push((home.to_path_buf(), SKIP_UNKNOWN_SHELL.to_string()));
        return Ok(out);
    }
    let home_owned = owned_by(&fs::metadata(home)?, ctx.uid);
    let text = stub_text(shell);
    for stub in stub_paths(home, shell) {
        ensure_stub(stub, &text, home_owned, &mut out)?;
    }
    if shell == Shell::Zsh {
        remove_legacy_stub(home, &mut out)?;
        let loader_ours = ensure_loader(home, home_owned, ctx, &mut out)?;
        // A rc line that sources a file we did not write would run their code.
        if loader_ours {
            ensure_zshrc(home, home_owned, ctx, &mut out)?;
        }
    }
    Ok(out)
}

/// Every file setup writes for `shell` with the text it would hold: the
/// stubs for bash and fish, the loader for zsh.
pub fn wired_files(home: &Path, shell: Shell) -> Vec<(PathBuf, String)> {
    match shell {
        Shell::Zsh => vec![(loader_path(home), loader_text())],
        _ => {
            let text = stub_text(shell);
            stub_paths(home, shell)
                .into_iter()
                .map(|path| (path, text.clone()))
                .collect()
        }
    }
}

/// The stub files for `shell`, the `pb` ones included. Zsh has none: it uses
/// [`loader_path`].
pub fn stub_paths(home: &Path, shell: Shell) -> Vec<PathBuf> {
    let names = |dir: &str, suffix: &str| {
        [BINARY_NAME, ALIAS_NAME]
            .map(|name| home.join(dir).join(format!("{name}{suffix}")))
            .to_vec()
    };
    match shell {
        Shell::Bash => names(BASH_COMPLETIONS, ""),
        Shell::Fish => names(FISH_COMPLETIONS, FISH_SUFFIX),
        Shell::Zsh | Shell::Other => Vec::new(),
    }
}

/// The full text of the loader stub written for `shell`; empty for zsh, which
/// has [`loader_text`] instead.
pub fn stub_text(shell: Shell) -> String {
    let body = match shell {
        Shell::Bash => BASH_STUB_BODY,
        Shell::Fish => FISH_STUB_BODY,
        Shell::Zsh | Shell::Other => return String::new(),
    };
    format!("{STUB_MARKER}\n{body}")
}

/// Where the zsh loader lives under `home`.
pub fn loader_path(home: &Path) -> PathBuf {
    home.join(LOADER)
}

/// The full text of the zsh loader.
pub fn loader_text() -> String {
    format!("{LOADER_HEADER}\n{}\n", LOADER_BODY.join("\n"))
}

/// Whether `bytes` is a loader we wrote, whatever its body.
pub fn loader_is_ours(bytes: &[u8]) -> bool {
    bytes
        .split(|&b| b == b'\n')
        .next()
        .is_some_and(|first| first.trim_ascii_end() == LOADER_HEADER.as_bytes())
}

/// Whether `bytes` is a pre-loader zsh stub we wrote.
pub fn legacy_stub_is_ours(bytes: &[u8]) -> bool {
    has_line(bytes, STUB_MARKER)
}

/// Removes the pre-loader zsh stub when it carries our marker, and its
/// directory when that leaves it empty.
fn remove_legacy_stub(home: &Path, out: &mut Outcome) -> io::Result<()> {
    let stub = home.join(LEGACY_STUB);
    let Ok(bytes) = fs::read(&stub) else {
        return Ok(());
    };
    if !legacy_stub_is_ours(&bytes) {
        return Ok(());
    }
    match fs::remove_file(&stub) {
        Ok(()) => out.removed.push(stub.clone()),
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            out.skipped.push((stub, SKIP_READ_ONLY.to_string()));
            return Ok(());
        }
        Err(e) => return Err(e),
    }
    if let Some(dir) = stub.parent() {
        let _ = fs::remove_dir(dir);
    }
    Ok(())
}

/// Writes or rewrites the loader. Returns false when the path holds a file
/// that is not ours.
fn ensure_loader(home: &Path, home_owned: bool, ctx: &Ctx, out: &mut Outcome) -> io::Result<bool> {
    let path = loader_path(home);
    let text = loader_text();
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == ErrorKind::NotFound && !home_owned => {
            out.skipped.push((path, SKIP_OTHER_OWNER.to_string()));
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {
            record_write(path.clone(), create_stub(&path, &text), true, out)?;
        }
        Err(e) => return Err(e),
        Ok(_) => return rewrite_loader(&path, &text, ctx, out),
    }
    Ok(true)
}

fn rewrite_loader(path: &Path, text: &str, ctx: &Ctx, out: &mut Outcome) -> io::Result<bool> {
    let target = match fs::canonicalize(path) {
        Ok(target) => target,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            out.skipped
                .push((path.to_path_buf(), SKIP_DANGLING.to_string()));
            return Ok(true);
        }
        Err(e) => return Err(e),
    };
    if !owned_by(&fs::metadata(&target)?, ctx.uid) {
        out.skipped
            .push((path.to_path_buf(), SKIP_OTHER_OWNER.to_string()));
        return Ok(true);
    }
    let Ok(bytes) = fs::read(&target) else {
        out.skipped
            .push((path.to_path_buf(), SKIP_UNREADABLE.to_string()));
        return Ok(true);
    };
    if !loader_is_ours(&bytes) {
        out.skipped
            .push((path.to_path_buf(), SKIP_NOT_OURS.to_string()));
        return Ok(false);
    }
    if bytes == text.as_bytes() {
        out.already.push(path.to_path_buf());
        return Ok(true);
    }
    let result = if rcfile::can_rewrite(path) {
        write_atomic_durable(&target, text.as_bytes())
    } else {
        Err(ErrorKind::PermissionDenied.into())
    };
    record_write(path.to_path_buf(), result, false, out)?;
    Ok(true)
}

fn ensure_stub(path: PathBuf, text: &str, home_owned: bool, out: &mut Outcome) -> io::Result<()> {
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == ErrorKind::NotFound && !home_owned => {
            out.skipped.push((path, SKIP_OTHER_OWNER.to_string()));
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {
            record_write(path.clone(), create_stub(&path, text), true, out)?;
        }
        Err(e) => return Err(e),
        Ok(_) => match fs::read(&path) {
            Ok(bytes) if has_line(&bytes, STUB_MARKER) && bytes == text.as_bytes() => {
                out.already.push(path);
            }
            Ok(bytes) if has_line(&bytes, STUB_MARKER) => out.kept.push(path),
            Ok(_) => out.skipped.push((path, SKIP_NOT_OURS.to_string())),
            Err(e) if e.kind() == ErrorKind::NotFound => {
                out.skipped.push((path, SKIP_DANGLING.to_string()));
            }
            Err(_) => out.skipped.push((path, SKIP_UNREADABLE.to_string())),
        },
    }
    Ok(())
}

fn create_stub(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        create_dirs(dir)?;
    }
    create_file(path, text.as_bytes())
}

#[cfg(unix)]
fn create_dirs(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(DIR_MODE)
        .create(dir)
}

#[cfg(not(unix))]
fn create_dirs(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)
}

/// `write_atomic_durable` sets permission bits only on an existing file, so a
/// new one would carry the umask; set the mode explicitly.
#[cfg(unix)]
fn create_file(path: &Path, content: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    write_atomic_durable(path, content)?;
    fs::set_permissions(path, fs::Permissions::from_mode(RC_NEW_MODE))
}

#[cfg(not(unix))]
fn create_file(path: &Path, content: &[u8]) -> io::Result<()> {
    write_atomic_durable(path, content)
}

fn has_line(content: &[u8], wanted: &str) -> bool {
    rcfile::split_lines(content)
        .iter()
        .any(|l| l.trim_ascii_end() == wanted.as_bytes())
}

enum Rc {
    Missing,
    Skip(&'static str),
    Present { target: PathBuf, content: Vec<u8> },
}

fn read_rc(rc: &Path, uid: u32) -> io::Result<Rc> {
    match fs::symlink_metadata(rc) {
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Rc::Missing),
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    let target = match fs::canonicalize(rc) {
        Ok(target) => target,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Rc::Skip(SKIP_DANGLING)),
        Err(e) => return Err(e),
    };
    if !owned_by(&fs::metadata(&target)?, uid) {
        return Ok(Rc::Skip(SKIP_OTHER_OWNER));
    }
    Ok(match fs::read(&target) {
        Ok(content) => Rc::Present { target, content },
        Err(_) => Rc::Skip(SKIP_UNREADABLE),
    })
}

fn rc_line_text() -> String {
    format!("{RC_LINE}\n")
}

/// How the lines of `~/.zshrc` relate to ours.
#[derive(Debug, PartialEq, Eq)]
struct RcLines {
    /// Lines equal to [`RC_LINE`].
    exact: usize,
    /// Lines that end with [`MARKER`] but are not [`RC_LINE`], and do not open
    /// a block of [`LEGACY_BLOCK_REST`].
    edited: usize,
    /// Blocks of the pre-loader text.
    legacy: usize,
}

fn rc_lines(content: &[u8]) -> RcLines {
    let mut found = RcLines {
        exact: 0,
        edited: 0,
        legacy: 0,
    };
    let lines = rcfile::split_lines(content);
    for (at, line) in lines.iter().enumerate() {
        let line = line.trim_ascii_end();
        if line == RC_LINE.as_bytes() {
            found.exact += 1;
        } else if rcfile::is_marked_block(&lines, at, MARKER, &LEGACY_BLOCK_REST) {
            found.legacy += 1;
        } else if line.ends_with(MARKER.as_bytes()) {
            found.edited += 1;
        }
    }
    found
}

fn ensure_zshrc(home: &Path, home_owned: bool, ctx: &Ctx, out: &mut Outcome) -> io::Result<()> {
    let rc = home.join(ZSHRC);
    let state = read_rc(&rc, ctx.uid)?;
    (ctx.between)(&rc);
    let existing = match state {
        Rc::Skip(why) => {
            out.skipped.push((rc, why.to_string()));
            return Ok(());
        }
        Rc::Missing if !home_owned => {
            out.skipped.push((rc, SKIP_OTHER_OWNER.to_string()));
            return Ok(());
        }
        Rc::Missing => None,
        Rc::Present { target, content } => Some((target, content)),
    };
    let created = existing.is_none();
    let content = existing.as_ref().map_or(&[][..], |(_, c)| c.as_slice());
    match rc_lines(content) {
        RcLines {
            legacy: 1.., exact, ..
        } => {
            let Some((target, content)) = existing else {
                return Ok(());
            };
            let result = migrate_legacy(&rc, &target, &content, exact == 0);
            record_write(rc, result, false, out)?;
        }
        RcLines { exact: 1.., .. } => out.already.push(rc),
        RcLines { edited: 1.., .. } => out.kept.push(rc),
        RcLines { .. } => {
            let result = match existing {
                None => create_file(&rc, rc_line_text().as_bytes()),
                Some((target, content)) => append_line(&rc, &target, &content),
            };
            record_write(rc, result, created, out)?;
        }
    }
    Ok(())
}

/// Replaces every pre-loader block with the new line, appended when the file
/// has none, after a backup of the original.
fn migrate_legacy(rc: &Path, target: &Path, original: &[u8], add_line: bool) -> io::Result<()> {
    if !rcfile::can_rewrite(rc) {
        return Err(ErrorKind::PermissionDenied.into());
    }
    let mut migrated = rcfile::strip_marked_block(original, MARKER, &LEGACY_BLOCK_REST)
        .unwrap_or_else(|| original.to_vec());
    if add_line {
        if !migrated.is_empty() && !migrated.ends_with(b"\n") {
            migrated.push(b'\n');
        }
        migrated.extend_from_slice(rc_line_text().as_bytes());
    }
    let stamp = crate::common::time::now_epoch_secs();
    backup_unless_append_only(rc, original, &migrated, stamp)?;
    write_atomic_durable(target, &migrated)
}

/// Appends the line through an atomic replace of the real target, so a
/// symlinked rc stays a symlink. The replace would succeed on a read-only
/// file in a writable directory, so writability is checked first.
fn append_line(rc: &Path, target: &Path, original: &[u8]) -> io::Result<()> {
    if !rcfile::can_rewrite(rc) {
        return Err(ErrorKind::PermissionDenied.into());
    }
    let mut appended = original.to_vec();
    if !appended.is_empty() && !appended.ends_with(b"\n") {
        appended.push(b'\n');
    }
    appended.extend_from_slice(rc_line_text().as_bytes());
    write_atomic_durable(target, appended)
}

fn record_write(
    path: PathBuf,
    result: io::Result<()>,
    created: bool,
    out: &mut Outcome,
) -> io::Result<()> {
    match result {
        Ok(()) if created => out.created.push(path),
        Ok(()) => out.changed.push(path),
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            out.skipped.push((path, SKIP_READ_ONLY.to_string()));
        }
        Err(e) => return Err(e),
    }
    Ok(())
}

/// What doctor should report for `shell` under `home`, from the files on disk.
pub fn status(home: &Path, shell: Shell) -> Status {
    if shell == Shell::Zsh {
        return zsh_status(home);
    }
    let stubs = stub_paths(home, shell);
    if stubs.is_empty() {
        return Status::Missing;
    }
    let contents: Vec<Option<Vec<u8>>> = stubs.iter().map(|p| fs::read(p).ok()).collect();
    if contents.iter().any(Option::is_none) {
        return Status::Missing;
    }
    if contents.iter().flatten().any(|c| !has_line(c, STUB_MARKER)) {
        return Status::StubNotOurs;
    }
    Status::Ok
}

fn zsh_status(home: &Path) -> Status {
    let Ok(loader) = fs::read(loader_path(home)) else {
        return Status::Missing;
    };
    if !loader_is_ours(&loader) {
        return Status::StubNotOurs;
    }
    let zshrc = fs::read(home.join(ZSHRC)).unwrap_or_default();
    match rc_lines(&zshrc) {
        RcLines { legacy: 1.., .. } => Status::Missing,
        RcLines { exact: 1, .. } => Status::Ok,
        RcLines { exact: 2.., .. } => Status::LineDuplicated,
        RcLines { edited: 1.., .. } => Status::LineEdited,
        RcLines { .. } => Status::Missing,
    }
}

/// `content` without every exact rc line and every pre-loader block; `None`
/// when there was nothing to remove.
pub fn strip_ours(content: &[u8]) -> Option<Vec<u8>> {
    let no_legacy = rcfile::strip_marked_block(content, MARKER, &LEGACY_BLOCK_REST);
    let rest = no_legacy.as_deref().unwrap_or(content);
    rcfile::strip_marked_block(rest, RC_LINE, &[]).or(no_legacy)
}

/// Removes every exact line and pre-loader block from `~/.zshrc` after a backup, for uninstall.
/// A file whose real target belongs to a uid other than `uid` is reported and
/// left alone. The loader file is the caller's to remove.
pub fn strip(home: &Path, uid: u32, stamp: u64, dry_run: bool) -> Vec<RcStrip> {
    let rc = home.join(ZSHRC);
    let Ok(target) = fs::canonicalize(&rc) else {
        return Vec::new();
    };
    let Ok(old) = fs::read(&target) else {
        return Vec::new();
    };
    let Some(new) = strip_ours(&old) else {
        return Vec::new();
    };
    let mut entry = RcStrip {
        rc_file: rc.clone(),
        backup: None,
        unwritable: !rcfile::can_rewrite(&rc),
        error: None,
    };
    if !fs::metadata(&target).is_ok_and(|meta| owned_by(&meta, uid)) {
        entry.error = Some(SKIP_OTHER_OWNER.to_string());
        return vec![entry];
    }
    if !entry.unwritable && !dry_run {
        let write = backup_unless_append_only(&rc, &old, &new, stamp).and_then(|backup| {
            write_atomic_durable(&target, &new)?;
            Ok(backup)
        });
        match write {
            Ok(backup) => entry.backup = backup,
            Err(e) => entry.error = Some(e.to_string()),
        }
    }
    vec![entry]
}

/// Copies `rc` to `<rc>.bak-<stamp>` unless `new` only appends to `old`.
/// An existing backup under that name is kept and returned as is: it holds
/// the file from before an earlier step of the same run rewrote it.
pub fn backup_unless_append_only(
    rc: &Path,
    old: &[u8],
    new: &[u8],
    stamp: u64,
) -> io::Result<Option<PathBuf>> {
    if new.starts_with(old) {
        return Ok(None);
    }
    let name = rc.file_name().unwrap_or_default().to_string_lossy();
    let backup = rc.with_file_name(format!("{name}{BACKUP_INFIX}{stamp}"));
    if !backup.exists() {
        fs::copy(rc, &backup)?;
    }
    Ok(Some(backup))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::fs;
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
    use std::process::Command;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, SystemTime};

    const RENDEZVOUS_TIMEOUT: Duration = Duration::from_secs(5);
    const PRIVATE_MODE: u32 = 0o600;
    const READ_ONLY_MODE: u32 = 0o444;
    const MODE_BITS: u32 = 0o777;
    const OLD_MTIME_SECS: u64 = 86_400;
    const STAMP: u64 = 1_700_000_000;
    const PRIVATE_UMASK: &str = "077";
    const CHILD_HOME_VAR: &str = "PB_COMPLETION_CHILD_HOME";
    const LOOSE_UMASK: &str = "002";
    const DIR_MODE_BITS: u32 = 0o755;
    const FOREIGN_FILE: &str = "/etc/hosts";
    const CHILD_TEST: &str = "umask_child_creates_the_zshrc";
    const ROOT_SKIP: &str = "skipped: euid 0 ignores file modes";
    const CI_VAR: &str = "CI";
    const USER_LINES: &str = "export EDITOR=vim\nalias ll='ls -l'\n";
    const NO_NEWLINE: &str = "export EDITOR=vim\nalias ll='ls -l'";
    const CREATED: &str = "created";
    const EDITED_RC_LINE: &str =
        "[ -r \"$HOME/mine.zsh\" ] && . \"$HOME/mine.zsh\"  # playbook completions\n";
    const THEIR_LOADER: &str = "# somebody else's loader\n";
    const OLD_LOGIC: &str = "# old logic\n";

    fn stale_loader() -> String {
        format!("{LOADER_HEADER}\n{OLD_LOGIC}")
    }

    fn home(tag: &str) -> PathBuf {
        let dir = crate::common::test_support::scratch_dir(tag);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn zshrc(home: &Path) -> PathBuf {
        home.join(ZSHRC)
    }

    fn ctx_for(uid: u32) -> Ctx<'static> {
        Ctx {
            uid,
            between: &|_| {},
        }
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & MODE_BITS
    }

    /// File text, or a marker string when unreadable, so a missing file fails an
    /// assertion instead of panicking in setup.
    fn read(path: impl AsRef<Path>) -> String {
        fs::read_to_string(path).unwrap_or_else(|e| format!("<unreadable: {e}>"))
    }

    fn marker_lines(path: &Path) -> usize {
        read(path).lines().filter(|l| l.ends_with(MARKER)).count()
    }

    fn backups(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(BACKUP_INFIX))
            .collect()
    }

    fn listing(dir: &Path) -> BTreeSet<PathBuf> {
        let mut found = BTreeSet::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(next) = pending.pop() {
            for entry in fs::read_dir(&next).unwrap().filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.is_dir() && !path.is_symlink() {
                    pending.push(path.clone());
                }
                found.insert(path);
            }
        }
        found
    }

    fn set_old_mtime(path: &Path) -> SystemTime {
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(OLD_MTIME_SECS);
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        old
    }

    /// True when the test must stop: root ignores file modes, and in CI that
    /// is a failure rather than a skip.
    fn root_cannot_prove_modes() -> bool {
        if effective_uid() != 0 {
            return false;
        }
        assert!(
            std::env::var_os(CI_VAR).is_none(),
            "{ROOT_SKIP}, but CI is set"
        );
        println!("{ROOT_SKIP}");
        true
    }

    #[test]
    fn a_missing_zshrc_is_created_with_the_block_and_mode_0644() {
        // Arrange
        let home = home("completion-create");

        // Act
        let out = ensure(&home, Shell::Zsh).unwrap();

        // Assert
        let rc = zshrc(&home);
        assert_eq!(read(&rc), rc_line_text());
        assert_eq!(mode(&rc), RC_NEW_MODE);
        assert!(backups(&home).is_empty(), "{:?}", backups(&home));
        assert!(out.created.contains(&rc), "{CREATED}: {out:?}");
    }

    #[test]
    fn an_append_only_change_takes_no_backup() {
        // Arrange
        let home = home("completion-append");
        fs::write(zshrc(&home), USER_LINES).unwrap();

        // Act
        ensure(&home, Shell::Zsh).unwrap();

        // Assert
        assert_eq!(
            read(zshrc(&home)),
            format!("{USER_LINES}{}", rc_line_text())
        );
        assert!(backups(&home).is_empty(), "{:?}", backups(&home));
    }

    #[test]
    fn an_rc_fixture_without_a_trailing_newline_or_empty_gets_one_block_on_its_own_line() {
        // Arrange
        let rows = [
            ("with-newline", USER_LINES, false),
            ("no-newline", NO_NEWLINE, true),
            ("empty", "", true),
        ];
        for (tag, original, second_run_is_checked) in rows {
            let home = home(&format!("completion-fixture-{tag}"));
            fs::write(zshrc(&home), original).unwrap();
            let separator = if original.is_empty() || original.ends_with('\n') {
                ""
            } else {
                "\n"
            };

            // Act
            ensure(&home, Shell::Zsh).unwrap();
            let after_first = fs::read(zshrc(&home)).unwrap();
            if second_run_is_checked {
                ensure(&home, Shell::Zsh).unwrap();
            }

            // Assert
            assert_eq!(
                String::from_utf8(after_first).unwrap(),
                format!("{original}{separator}{}", rc_line_text()),
                "{tag}"
            );
            assert_eq!(marker_lines(&zshrc(&home)), 1, "{tag}");
            assert!(backups(&home).is_empty(), "{tag}: {:?}", backups(&home));
        }
    }

    #[test]
    fn a_dangling_symlink_zshrc_is_skipped() {
        // Arrange
        let home = home("completion-dangling");
        let target = home.join("missing-target");
        symlink(&target, zshrc(&home)).unwrap();

        // Act
        let out = ensure(&home, Shell::Zsh).unwrap();

        // Assert
        assert!(!target.exists(), "the target was created");
        assert!(
            out.skipped
                .iter()
                .any(|(p, why)| *p == zshrc(&home) && why == SKIP_DANGLING),
            "{out:?}"
        );
    }

    #[test]
    fn a_rerun_changes_nothing() {
        // Arrange
        let home = home("completion-rerun");
        ensure(&home, Shell::Zsh).unwrap();
        let rc = zshrc(&home);
        assert!(rc.exists(), "the first run wrote no .zshrc");
        let old = set_old_mtime(&rc);
        let bytes = fs::read(&rc).unwrap();
        let files = listing(&home);

        // Act
        let out = ensure(&home, Shell::Zsh).unwrap();

        // Assert
        assert_eq!(fs::read(&rc).unwrap(), bytes);
        assert_eq!(fs::metadata(&rc).unwrap().modified().unwrap(), old);
        assert_eq!(listing(&home), files);
        assert!(out.created.is_empty() && out.changed.is_empty(), "{out:?}");
    }

    /// Two parties wait for each other, or panic naming who never came.
    struct Rendezvous {
        arrived: Mutex<Vec<&'static str>>,
        both: Condvar,
    }

    impl Rendezvous {
        fn new() -> Arc<Rendezvous> {
            Arc::new(Rendezvous {
                arrived: Mutex::new(Vec::new()),
                both: Condvar::new(),
            })
        }

        fn arrive(&self, who: &'static str) {
            let mut arrived = self.arrived.lock().unwrap();
            arrived.push(who);
            self.both.notify_all();
            let (arrived, timeout) = self
                .both
                .wait_timeout_while(arrived, RENDEZVOUS_TIMEOUT, |a| a.len() < 2)
                .unwrap();
            assert!(
                !timeout.timed_out(),
                "{who} waited {RENDEZVOUS_TIMEOUT:?}; arrived: {arrived:?}, the other party never did"
            );
        }
    }

    fn interleaved_ensure(home: &Path) {
        let rendezvous = Rendezvous::new();
        let uid = effective_uid();
        let handles: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|who| {
                let home = home.to_path_buf();
                let rendezvous = Arc::clone(&rendezvous);
                std::thread::spawn(move || {
                    let hook = move |_: &Path| rendezvous.arrive(who);
                    let ctx = Ctx {
                        uid,
                        between: &hook,
                    };
                    ensure_with(&home, Shell::Zsh, &ctx).unwrap();
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("an ensure thread panicked");
        }
    }

    #[test]
    fn two_ensure_calls_forced_to_interleave_leave_exactly_one_block() {
        // Arrange
        let with_block = format!("{USER_LINES}{}", rc_line_text());
        let cases = [
            ("create", None, rc_line_text()),
            ("append", Some(USER_LINES.to_string()), with_block.clone()),
            ("no-op", Some(with_block.clone()), with_block),
        ];
        for (path, original, expected) in cases {
            let home = home(&format!("completion-race-{path}"));
            if let Some(original) = &original {
                fs::write(zshrc(&home), original).unwrap();
            }

            // Act
            interleaved_ensure(&home);

            // Assert
            assert_eq!(marker_lines(&zshrc(&home)), 1, "{path}");
            assert_eq!(read(zshrc(&home)), expected, "{path}");
        }
    }

    #[test]
    fn an_edited_marker_line_is_kept_and_not_duplicated() {
        // Arrange
        let home = home("completion-edited");
        let original = format!("{USER_LINES}{EDITED_RC_LINE}");
        fs::write(zshrc(&home), &original).unwrap();

        // Act
        let out = ensure(&home, Shell::Zsh).unwrap();

        // Assert
        assert_eq!(read(zshrc(&home)), original);
        assert!(out.kept.contains(&zshrc(&home)), "{out:?}");
    }

    #[test]
    fn the_atomic_rewrite_keeps_mode_0600() {
        // Arrange
        let home = home("completion-mode-0600");
        let rc = zshrc(&home);
        fs::write(&rc, USER_LINES).unwrap();
        fs::set_permissions(&rc, fs::Permissions::from_mode(PRIVATE_MODE)).unwrap();

        // Act
        ensure(&home, Shell::Zsh).unwrap();

        // Assert
        assert!(read(&rc).ends_with(&rc_line_text()));
        assert_eq!(mode(&rc), PRIVATE_MODE);
    }

    /// Runs only as the child of `run_ensure_under_umask`.
    #[test]
    #[ignore = "runs only as a child process under a chosen umask"]
    fn umask_child_creates_the_zshrc() {
        let Some(home) = std::env::var_os(CHILD_HOME_VAR) else {
            return;
        };
        ensure(Path::new(&home), Shell::Zsh).unwrap();
    }

    /// Runs `ensure` for zsh in a child process with `umask`, so the test
    /// process's own umask never changes.
    fn run_ensure_under_umask(umask: &str, home: &Path) {
        let exe = std::env::current_exe().unwrap();
        let test = format!(
            "{}::{CHILD_TEST}",
            module_path!().split_once("::").map_or("", |(_, rest)| rest)
        );
        let script = format!("umask {umask}; exec \"$0\" --ignored --exact \"$1\"");
        let out = Command::new("sh")
            .env_clear()
            .env(CHILD_HOME_VAR, home)
            .args(["-c", &script])
            .arg(&exe)
            .arg(&test)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn a_new_zshrc_is_0644_under_umask_077() {
        // Arrange
        let home = home("completion-umask");

        // Act
        run_ensure_under_umask(PRIVATE_UMASK, &home);

        // Assert
        assert_eq!(read(zshrc(&home)), rc_line_text());
        assert_eq!(mode(&zshrc(&home)), RC_NEW_MODE);
    }

    #[test]
    fn created_directories_are_0755_under_a_group_writable_umask() {
        // Arrange
        let home = home("completion-dir-mode");

        // Act
        run_ensure_under_umask(LOOSE_UMASK, &home);

        // Assert
        let loader = loader_path(&home);
        let chain: Vec<_> = loader
            .parent()
            .unwrap()
            .ancestors()
            .take_while(|dir| *dir != home)
            .collect();
        assert!(!chain.is_empty());
        for dir in chain {
            assert_eq!(mode(dir), DIR_MODE_BITS, "{}", dir.display());
        }
    }

    #[test]
    fn an_rc_owned_by_another_user_is_refused() {
        // Arrange
        let home = home("completion-other-owner");
        let rc = zshrc(&home);
        fs::write(&rc, USER_LINES).unwrap();
        let other = effective_uid() + 1;

        // Act
        let out = ensure_with(&home, Shell::Zsh, &ctx_for(other)).unwrap();

        // Assert
        assert_eq!(read(&rc), USER_LINES);
        assert!(
            out.skipped
                .iter()
                .any(|(p, why)| *p == rc && why == SKIP_OTHER_OWNER),
            "{out:?}"
        );
    }

    #[test]
    fn an_rc_symlinked_to_a_file_of_another_owner_is_refused_by_the_targets_owner() {
        // Arrange
        let home = home("completion-foreign-target");
        if effective_uid() == 0 {
            println!("{ROOT_SKIP}");
            return;
        }
        let target = Path::new(FOREIGN_FILE);
        assert_ne!(fs::metadata(target).unwrap().uid(), effective_uid());
        symlink(target, zshrc(&home)).unwrap();
        let before = fs::read(target).unwrap();

        // Act
        let out = ensure(&home, Shell::Zsh).unwrap();

        // Assert
        assert_eq!(fs::read(target).unwrap(), before);
        assert!(
            out.skipped
                .iter()
                .any(|(p, why)| *p == zshrc(&home) && why == SKIP_OTHER_OWNER),
            "{out:?}"
        );
    }

    #[test]
    fn nothing_is_created_under_a_home_owned_by_another_user() {
        // Arrange
        let home = home("completion-foreign-home");
        let other = effective_uid() + 1;

        // Act
        let out = ensure_with(&home, Shell::Zsh, &ctx_for(other)).unwrap();

        // Assert
        assert!(listing(&home).is_empty(), "{:?}", listing(&home));
        assert!(out.created.is_empty(), "{out:?}");
        let skipped: Vec<_> = out.skipped.iter().map(|(_, why)| why.as_str()).collect();
        assert_eq!(skipped, [SKIP_OTHER_OWNER; 2], "{out:?}");
    }

    #[test]
    fn a_symlinked_zshrc_stays_a_symlink() {
        // Arrange
        let home = home("completion-symlink");
        let target = home.join("dotfiles-zshrc");
        fs::write(&target, USER_LINES).unwrap();
        symlink(&target, zshrc(&home)).unwrap();

        // Act
        ensure_with(&home, Shell::Zsh, &ctx_for(effective_uid())).unwrap();

        // Assert
        assert!(zshrc(&home).is_symlink());
        assert_eq!(fs::read_link(zshrc(&home)).unwrap(), target);
        assert_eq!(read(&target), format!("{USER_LINES}{}", rc_line_text()));
    }

    #[test]
    fn backup_unless_append_only_backs_up_only_a_non_append() {
        // Arrange
        let old = b"export A=1\n".as_slice();
        let rows: [(&str, Vec<u8>, bool); 2] = [
            ("rewrite", b"export A=2\n".to_vec(), true),
            ("append", [old, b"export B=2\n"].concat(), false),
        ];
        for (tag, new, expects_backup) in rows {
            let home = home(&format!("completion-backup-{tag}"));
            let rc = zshrc(&home);
            fs::write(&rc, old).unwrap();

            // Act
            let backup = backup_unless_append_only(&rc, old, &new, STAMP).unwrap();

            // Assert
            assert_eq!(backup.is_some(), expects_backup, "{tag}");
            match backup {
                Some(path) => {
                    assert_eq!(
                        path.file_name().unwrap().to_string_lossy(),
                        format!("{ZSHRC}{BACKUP_INFIX}{STAMP}")
                    );
                    assert_eq!(fs::read(path).unwrap(), old);
                }
                None => assert!(backups(&home).is_empty(), "{tag}"),
            }
        }
    }

    #[test]
    fn a_symlink_into_a_read_only_target_is_skipped() {
        // Arrange
        let home = home("completion-readonly-link");
        if root_cannot_prove_modes() {
            return;
        }
        let target = home.join("dotfiles-zshrc");
        fs::write(&target, USER_LINES).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(READ_ONLY_MODE)).unwrap();
        symlink(&target, zshrc(&home)).unwrap();

        // Act
        let out = ensure_with(&home, Shell::Zsh, &ctx_for(effective_uid())).unwrap();

        // Assert
        assert_eq!(read(&target), USER_LINES);
        assert!(
            out.skipped
                .iter()
                .any(|(p, why)| *p == zshrc(&home) && why == SKIP_READ_ONLY),
            "{out:?}"
        );
    }

    #[test]
    fn a_read_only_zshrc_is_reported_and_its_bytes_are_unchanged() {
        // Arrange
        let home = home("completion-readonly");
        if root_cannot_prove_modes() {
            return;
        }
        let rc = zshrc(&home);
        fs::write(&rc, USER_LINES).unwrap();
        fs::set_permissions(&rc, fs::Permissions::from_mode(READ_ONLY_MODE)).unwrap();

        // Act
        let out = ensure_with(&home, Shell::Zsh, &ctx_for(effective_uid())).unwrap();

        // Assert
        assert_eq!(read(&rc), USER_LINES);
        assert!(out.skipped.iter().any(|(p, _)| *p == rc), "{out:?}");
    }

    #[test]
    fn a_stub_path_that_is_not_ours_is_never_overwritten() {
        for shell in [Shell::Bash, Shell::Fish] {
            // Arrange
            let home = home("completion-foreign-stub");
            let stubs = stub_paths(&home, shell);
            assert!(!stubs.is_empty(), "{shell:?} has no stub paths");
            let theirs = stubs[0].clone();
            fs::create_dir_all(theirs.parent().unwrap()).unwrap();
            fs::write(&theirs, "# somebody else's completion\n").unwrap();

            // Act
            let out = ensure(&home, shell).unwrap();

            // Assert
            assert_eq!(read(&theirs), "# somebody else's completion\n", "{shell:?}");
            assert!(out.skipped.iter().any(|(p, _)| *p == theirs), "{out:?}");
        }
    }

    #[test]
    fn zsh_has_no_fpath_stub() {
        // Arrange
        let home = home("completion-no-fpath-stub");

        // Act
        let out = ensure(&home, Shell::Zsh).unwrap();

        // Assert
        assert!(stub_paths(&home, Shell::Zsh).is_empty());
        assert!(stub_text(Shell::Zsh).is_empty());
        let written: Vec<_> = out.created.iter().collect();
        assert_eq!(written, [&loader_path(&home), &zshrc(&home)], "{out:?}");
    }

    #[test]
    fn the_loader_is_the_header_then_the_seven_adr_lines() {
        // Arrange
        let home = home("completion-loader-text");

        // Act
        ensure(&home, Shell::Zsh).unwrap();

        // Assert
        let loader = loader_path(&home);
        assert_eq!(read(&loader), loader_text());
        let lines: Vec<_> = loader_text().lines().map(str::to_owned).collect();
        assert_eq!(lines[0], LOADER_HEADER);
        assert_eq!(lines[1..], LOADER_BODY);
        assert_eq!(mode(&loader), RC_NEW_MODE);
        assert_eq!(
            LOADER_BODY[3],
            "_playbook() { unfunction _playbook; source <(playbook completions zsh 2>/dev/null); _playbook \"$@\"; }"
        );
    }

    #[test]
    fn the_rc_line_is_the_adr_line() {
        assert_eq!(
            RC_LINE,
            "[ -r \"$HOME/.local/share/playbook/completions.zsh\" ] && . \"$HOME/.local/share/playbook/completions.zsh\"  # playbook completions"
        );
    }

    #[test]
    fn a_loader_whose_text_differs_is_rewritten_when_it_has_our_header() {
        // Arrange
        let home = home("completion-loader-stale");
        let loader = loader_path(&home);
        fs::create_dir_all(loader.parent().unwrap()).unwrap();
        fs::write(&loader, stale_loader()).unwrap();

        // Act
        let out = ensure(&home, Shell::Zsh).unwrap();

        // Assert
        assert_eq!(read(&loader), loader_text());
        assert!(out.changed.contains(&loader), "{out:?}");
    }

    #[test]
    fn a_loader_without_our_header_is_kept_and_no_rc_line_is_added() {
        // Arrange
        let home = home("completion-loader-theirs");
        let loader = loader_path(&home);
        fs::create_dir_all(loader.parent().unwrap()).unwrap();
        fs::write(&loader, THEIR_LOADER).unwrap();

        // Act
        let out = ensure(&home, Shell::Zsh).unwrap();

        // Assert
        assert_eq!(read(&loader), THEIR_LOADER);
        assert!(
            out.skipped
                .iter()
                .any(|(p, why)| *p == loader && why == SKIP_NOT_OURS),
            "{out:?}"
        );
        assert!(!zshrc(&home).exists(), "{:?}", listing(&home));
    }

    #[test]
    fn a_dangling_symlink_loader_is_skipped_and_a_live_one_is_followed() {
        // Arrange
        let dangling = home("completion-loader-dangling");
        let live = home("completion-loader-live");
        for (home, target_exists) in [(&dangling, false), (&live, true)] {
            let loader = loader_path(home);
            fs::create_dir_all(loader.parent().unwrap()).unwrap();
            let target = home.join("dotfiles-loader");
            if target_exists {
                fs::write(&target, stale_loader()).unwrap();
            }
            symlink(&target, &loader).unwrap();
        }

        // Act
        let out_dangling = ensure(&dangling, Shell::Zsh).unwrap();
        ensure(&live, Shell::Zsh).unwrap();

        // Assert
        assert!(!dangling.join("dotfiles-loader").exists());
        assert!(
            out_dangling
                .skipped
                .iter()
                .any(|(p, why)| *p == loader_path(&dangling) && why == SKIP_DANGLING),
            "{out_dangling:?}"
        );
        assert!(loader_path(&live).is_symlink());
        assert_eq!(read(live.join("dotfiles-loader")), loader_text());
    }

    #[test]
    fn two_exact_lines_are_left_as_they_are_and_a_marker_with_other_text_is_kept() {
        // Arrange
        let twice = home("completion-two-lines");
        let both = format!("{0}{USER_LINES}{0}", rc_line_text());
        fs::write(zshrc(&twice), &both).unwrap();
        let mixed = home("completion-mixed-lines");
        let kept = format!("{EDITED_RC_LINE}{USER_LINES}");
        fs::write(zshrc(&mixed), &kept).unwrap();

        // Act
        let out_twice = ensure(&twice, Shell::Zsh).unwrap();
        let out_mixed = ensure(&mixed, Shell::Zsh).unwrap();

        // Assert
        assert_eq!(read(zshrc(&twice)), both);
        assert!(out_twice.already.contains(&zshrc(&twice)), "{out_twice:?}");
        assert_eq!(read(zshrc(&mixed)), kept);
        assert!(out_mixed.kept.contains(&zshrc(&mixed)), "{out_mixed:?}");
    }

    #[test]
    fn the_rc_line_never_trips_the_retired_launcher_check() {
        // Arrange
        let home = home("completion-layer3");
        fs::write(zshrc(&home), USER_LINES).unwrap();
        let retired = [
            format!("{}  {MARKER}", rcfile::SOURCE_LINE),
            format!("source \"$HOME/.config/playbook/shell/zsh/cc.zsh\"  {MARKER}"),
            format!("eval \"$(playbook shell-init)\"  {MARKER}"),
            format!("# playbook launchers (ccc/ccd)  {MARKER}"),
        ];

        // Act
        ensure(&home, Shell::Zsh).unwrap();

        // Assert
        assert!(
            rcfile::retired_rc_entries(&home).is_empty(),
            "{:?}",
            rcfile::retired_rc_entries(&home)
        );
        for line in retired {
            fs::write(zshrc(&home), format!("{line}\n")).unwrap();
            assert!(
                rcfile::retired_rc_entries(&home).is_empty(),
                "a line ending with the marker was flagged: {line}"
            );
        }
        for shell in [Shell::Bash, Shell::Fish] {
            assert!(
                !stub_text(shell).contains(rcfile::SOURCE_LINE),
                "{shell:?} stub names the retired launcher line"
            );
        }
    }

    #[test]
    fn status_follows_what_ensure_wrote_and_what_was_changed_after() {
        // Arrange
        let home = home("completion-status");
        let before = status(&home, Shell::Zsh);
        ensure(&home, Shell::Zsh).unwrap();
        let after = status(&home, Shell::Zsh);
        let rc = zshrc(&home);
        let original = read(&rc);

        // Act
        fs::write(&rc, format!("{original}{original}")).unwrap();
        let twice = status(&home, Shell::Zsh);
        fs::write(&rc, EDITED_RC_LINE).unwrap();
        let edited = status(&home, Shell::Zsh);
        fs::write(&rc, USER_LINES).unwrap();
        let no_line = status(&home, Shell::Zsh);
        fs::write(&rc, &original).unwrap();
        fs::write(loader_path(&home), THEIR_LOADER).unwrap();
        let foreign = status(&home, Shell::Zsh);
        fs::remove_file(loader_path(&home)).unwrap();
        let no_loader = status(&home, Shell::Zsh);

        // Assert
        assert_eq!(before, Status::Missing);
        assert_eq!(after, Status::Ok);
        assert_eq!(twice, Status::LineDuplicated);
        assert_eq!(edited, Status::LineEdited);
        assert_eq!(no_line, Status::Missing);
        assert_eq!(foreign, Status::StubNotOurs);
        assert_eq!(no_loader, Status::Missing);
    }

    #[test]
    fn strip_removes_every_exact_line_after_a_backup_that_is_never_overwritten() {
        // Arrange
        let home = home("completion-strip");
        let rc = zshrc(&home);
        let original = format!("{USER_LINES}{0}{0}", rc_line_text());
        fs::write(&rc, &original).unwrap();
        let backup = home.join(format!("{ZSHRC}{BACKUP_INFIX}{STAMP}"));
        fs::write(&backup, "an earlier backup\n").unwrap();

        // Act
        let dry = strip(&home, effective_uid(), STAMP, true);
        let dry_text = read(&rc);
        let done = strip(&home, effective_uid(), STAMP, false);
        let again = strip(&home, effective_uid(), STAMP, false);

        // Assert
        assert_eq!(dry.len(), 1);
        assert_eq!(dry_text, original);
        assert_eq!(read(&rc), USER_LINES);
        assert_eq!(done[0].backup.as_ref(), Some(&backup));
        assert_eq!(read(&backup), "an earlier backup\n");
        assert!(again.is_empty());
    }

    #[test]
    fn strip_takes_a_backup_with_the_original_when_none_exists() {
        // Arrange
        let home = home("completion-strip-backup");
        let rc = zshrc(&home);
        let original = format!("{USER_LINES}{}", rc_line_text());
        fs::write(&rc, &original).unwrap();

        // Act
        let done = strip(&home, effective_uid(), STAMP, false);

        // Assert
        let backup = done[0].backup.as_ref().expect("a backup");
        assert_eq!(read(backup), original);
        assert_eq!(read(&rc), USER_LINES);
    }

    #[test]
    fn strip_leaves_an_rc_owned_by_another_user_alone() {
        // Arrange
        let home = home("completion-strip-foreign");
        let rc = zshrc(&home);
        fs::write(&rc, USER_LINES).unwrap();
        ensure(&home, Shell::Zsh).unwrap();
        let before = read(&rc);
        let other = effective_uid() + 1;

        // Act
        let out = strip(&home, other, STAMP, false);

        // Assert
        assert_eq!(read(&rc), before);
        assert!(backups(&home).is_empty(), "{:?}", backups(&home));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].error.as_deref(), Some(SKIP_OTHER_OWNER));
        assert_eq!(out[0].backup, None);
    }

    #[test]
    fn status_of_an_unknown_shell_is_missing() {
        // Arrange
        let home = home("completion-status-other");

        // Act
        let found = status(&home, Shell::Other);

        // Assert
        assert_eq!(found, Status::Missing);
    }

    #[test]
    fn the_fish_stub_sources_only_a_successful_non_empty_script() {
        // Arrange
        let text = stub_text(Shell::Fish);

        // Act
        let capture = text.find("set -l script (playbook completions fish 2>/dev/null)");
        let check = text.find("test $status -eq 0; and test -n \"$script\"");
        let source = text.find("printf '%s\\n' $script | source");

        // Assert
        assert!(
            capture.is_some() && check.is_some() && source.is_some(),
            "{text}"
        );
        assert!(capture < check && check < source, "{text}");
        assert!(!text.contains("fish 2>/dev/null | source"), "{text}");
    }

    #[test]
    fn strip_keeps_an_edited_marker_line() {
        // Arrange
        let home = home("completion-strip-edited");
        fs::write(zshrc(&home), EDITED_RC_LINE).unwrap();

        // Act
        let out = strip(&home, effective_uid(), STAMP, false);

        // Assert
        assert!(out.is_empty());
        assert_eq!(read(zshrc(&home)), EDITED_RC_LINE);
        assert!(backups(&home).is_empty(), "{:?}", backups(&home));
    }

    fn legacy_block() -> String {
        format!(
            "{MARKER}\n{}\n{}\n",
            LEGACY_BLOCK_REST[0], LEGACY_BLOCK_REST[1]
        )
    }

    fn write_legacy_stub(home: &Path, text: &str) -> PathBuf {
        let stub = home.join(LEGACY_STUB);
        fs::create_dir_all(stub.parent().unwrap()).unwrap();
        fs::write(&stub, text).unwrap();
        stub
    }

    fn legacy_stub_text() -> String {
        format!("#compdef playbook pb\n{STUB_MARKER}\n# old body\n")
    }

    #[test]
    fn the_legacy_block_is_the_pre_loader_text() {
        assert_eq!(
            LEGACY_BLOCK_REST[0],
            r#"fpath=("$HOME/.local/share/zsh/site-functions" $fpath)"#
        );
        assert!(LEGACY_BLOCK_REST[1].starts_with("if (( $+functions[compdef] ))"));
        assert!(LEGACY_BLOCK_REST[1].ends_with("else autoload -Uz compinit && compinit; fi"));
    }

    #[test]
    fn setup_migrates_the_old_block_and_stub_to_the_loader_with_a_backup() {
        // Arrange
        let home = home("completion-migrate");
        let original = format!("{USER_LINES}{}export B=2\n", legacy_block());
        fs::write(zshrc(&home), &original).unwrap();
        let stub = write_legacy_stub(&home, &legacy_stub_text());

        // Act
        let out = ensure(&home, Shell::Zsh).unwrap();

        // Assert
        assert_eq!(
            read(zshrc(&home)),
            format!("{USER_LINES}export B=2\n{}", rc_line_text())
        );
        assert!(!stub.exists());
        assert!(!stub.parent().unwrap().exists(), "the empty dir stays");
        assert_eq!(read(loader_path(&home)), loader_text());
        let saved = backups(&home);
        assert_eq!(saved.len(), 1, "{saved:?}");
        assert_eq!(read(home.join(&saved[0])), original);
        assert!(out.changed.contains(&zshrc(&home)), "{out:?}");
        assert_eq!(out.removed, [stub], "{out:?}");
        assert_eq!(status(&home, Shell::Zsh), Status::Ok);
    }

    #[test]
    fn a_legacy_stub_without_our_marker_and_an_edited_old_block_are_kept() {
        // Arrange
        let home = home("completion-migrate-kept");
        let theirs = write_legacy_stub(&home, "#compdef playbook\n# mine\n");
        let edited = format!("{MARKER}\n# my own fpath\n{}\n", LEGACY_BLOCK_REST[1]);
        fs::write(zshrc(&home), &edited).unwrap();

        // Act
        let out = ensure(&home, Shell::Zsh).unwrap();

        // Assert
        assert_eq!(read(&theirs), "#compdef playbook\n# mine\n");
        assert_eq!(read(zshrc(&home)), edited);
        assert!(out.kept.contains(&zshrc(&home)), "{out:?}");
    }

    #[test]
    fn a_legacy_block_beside_the_new_line_is_dropped_without_a_second_line() {
        // Arrange
        let home = home("completion-migrate-both");
        fs::write(
            zshrc(&home),
            format!("{}{USER_LINES}{}", rc_line_text(), legacy_block()),
        )
        .unwrap();

        // Act
        ensure(&home, Shell::Zsh).unwrap();

        // Assert
        assert_eq!(
            read(zshrc(&home)),
            format!("{}{USER_LINES}", rc_line_text())
        );
    }

    #[test]
    fn strip_removes_the_legacy_block_with_a_backup() {
        // Arrange
        let home = home("completion-strip-legacy");
        let original = format!("{USER_LINES}{}", legacy_block());
        fs::write(zshrc(&home), &original).unwrap();

        // Act
        let done = strip(&home, effective_uid(), STAMP, false);

        // Assert
        assert_eq!(read(zshrc(&home)), USER_LINES);
        assert_eq!(read(done[0].backup.as_ref().unwrap()), original);
    }

    #[test]
    fn the_legacy_block_is_reported_as_not_set_up() {
        // Arrange
        let home = home("completion-status-legacy");
        ensure(&home, Shell::Zsh).unwrap();
        fs::write(zshrc(&home), legacy_block()).unwrap();

        // Act
        let found = status(&home, Shell::Zsh);

        // Assert
        assert_eq!(found, Status::Missing);
    }
}
