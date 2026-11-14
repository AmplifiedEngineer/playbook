// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Shared harness for the statusline integration tests: a scratch directory,
//! a run of the real binary with a clean environment, and an escape stripper.
//! Each test binary uses a subset of it.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

/// A throwaway directory removed on drop.
pub(crate) struct Scratch(pub(crate) PathBuf);

impl Scratch {
    pub(crate) fn new() -> Self {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let p =
            std::env::temp_dir().join(format!("statusline-{}-{n}", playbook::testing::run_id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Scratch(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ =
            std::fs::set_permissions(&self.0, std::os::unix::fs::PermissionsExt::from_mode(0o700));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_playbook")
}

/// Whether `tool` exists on the PATH the runs get, so a skip matches what would run.
pub(crate) fn have(tool: &str) -> bool {
    ["/usr/bin", "/bin"]
        .iter()
        .any(|d| Path::new(d).join(tool).is_file())
}

/// A `gh` that always fails, first on PATH, so no run touches the network.
fn stub_dir(root: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = root.join("stub");
    std::fs::create_dir_all(&dir).unwrap();
    let gh = dir.join("gh");
    std::fs::write(&gh, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

fn path_for(root: &Path) -> String {
    let bin_dir = Path::new(bin()).parent().unwrap().display().to_string();
    format!("{}:{bin_dir}:/usr/bin:/bin", stub_dir(root).display())
}

/// Runs `cmd` with a cleared environment. `HOME` and `CLAUDE_CONFIG_DIR` both
/// point at `home` so no run reads the developer's real `~/.claude.json`, and
/// `COLUMNS` is unset unless `env` sets it.
pub(crate) fn run(
    mut cmd: Command,
    home: &Path,
    root: &Path,
    payload: &str,
    env: &[(&str, &str)],
) -> Vec<u8> {
    use std::io::Write;
    cmd.env_clear()
        .env("HOME", home)
        .env("CLAUDE_CONFIG_DIR", home)
        .env("PWD", home)
        .env("PATH", path_for(root))
        .env("STATUSLINE_PR_CACHE_TTL", "9999999")
        .env("STATUSLINE_CI_CACHE_TTL", "9999999")
        .envs(env.iter().copied())
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap().stdout
}

/// `text` without its ANSI colour escapes (`ESC [ ... m`).
pub(crate) fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            for d in chars.by_ref() {
                if d == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// A `PATH` whose first entry holds a `gh` that always exits 1, so a pull
/// request lookup reads "no information" whatever `gh` the host has. The stub
/// lives under `root`.
pub(crate) fn path_with_failing_gh(root: &Path) -> std::ffi::OsString {
    use std::os::unix::fs::PermissionsExt;
    let dir = root.join("failing-gh-bin");
    std::fs::create_dir_all(&dir).unwrap();
    let gh = dir.join("gh");
    std::fs::write(&gh, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut paths = vec![dir];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(paths).unwrap()
}
