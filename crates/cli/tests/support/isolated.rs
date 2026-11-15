// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The one way a test starts the built `playbook` without a home of its
//! own: in a fresh scratch home, with no `PLAYBOOK_*`, `CLAUDE_*` or `XDG_*`
//! variables from the caller. A test that inherits the real `HOME` would run
//! the first-run heal against the real home. `test_isolation.rs` fails when
//! a test file starts the binary without this helper or its own `HOME`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

/// A fresh, empty scratch home.
pub(crate) fn fresh_home() -> PathBuf {
    let n = N.fetch_add(1, Ordering::Relaxed);
    let home = std::env::temp_dir().join(format!("pb-it-home-{}-{n}", playbook::testing::run_id()));
    std::fs::create_dir_all(&home).expect("scratch home");
    home
}

/// The built `playbook` in a fresh scratch home.
// The root of this chain: tests that pass their own home call `playbook_in` or
// `isolate` directly, so the helpers it reaches are dead in those binaries.
#[allow(dead_code)]
pub(crate) fn playbook() -> Command {
    playbook_in(&fresh_home())
}

/// The built `playbook` with `home` as its home.
pub(crate) fn playbook_in(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_playbook"));
    isolate(&mut cmd, home);
    cmd
}

/// Points every home `cmd` could read at `home` and drops the caller's
/// `PLAYBOOK_*`, `CLAUDE_*` and `XDG_*` variables. Set a variable after this
/// call to give it to the child.
pub(crate) fn isolate(cmd: &mut Command, home: &Path) {
    for (key, _) in std::env::vars_os() {
        let drop = key.to_str().is_some_and(|k| {
            ["PLAYBOOK_", "CLAUDE_", "XDG_"]
                .iter()
                .any(|p| k.starts_with(p))
        });
        if drop {
            cmd.env_remove(&key);
        }
    }
    cmd.env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
        // No real `claude plugin update` from a test.
        .env("PLAYBOOK_SKIP_PLUGIN_UPDATE", "1");
}

/// Call after [`isolate`]: no host git config and a `gh` that always fails
/// first on `PATH`, so a stray lookup reads as a failed one and never reaches
/// the host or the network.
// Only the tests that run the session reader use it.
#[allow(dead_code)]
pub(crate) fn go_offline(cmd: &mut Command, home: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let bin = home.join("fake-bin");
    std::fs::create_dir_all(&bin).expect("fake bin dir");
    let gh = bin.join("gh");
    std::fs::write(&gh, "#!/bin/sh\nexit 1\n").expect("fake gh");
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).expect("chmod fake gh");
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .expect("PATH");
    cmd.env("PATH", path)
        .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1");
}
