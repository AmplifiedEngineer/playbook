// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Shared setup for tests that run a session against the scripted fake.

use super::client::{Client, ClientError};
use super::fake::Step;
use super::session::{Launch, Launcher, Options};
use super::transport::Transport;
use crate::host::LspSettings;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

pub(crate) fn scratch(tag: &str) -> PathBuf {
    let dir = playbook_common::test_support::scratch_dir(tag);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(unix)]
fn install_ts_server(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("typescript-language-server");
    std::fs::write(&path, "#!/bin/sh\necho 6.0.1\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(not(unix))]
fn install_ts_server(_dir: &Path) {}

/// Hands back one prepared client and counts how often it was asked.
pub(crate) struct FakeLauncher {
    pub calls: AtomicUsize,
    transport: Mutex<Option<Transport>>,
}

impl FakeLauncher {
    pub(crate) fn new(transport: Option<Transport>) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            transport: Mutex::new(transport),
        }
    }
}

impl Launcher for FakeLauncher {
    fn start(&self, launch: &Launch) -> Result<Client, ClientError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let transport = self.transport.lock().unwrap().take().expect("one launch");
        Ok(Client::with_transport(transport, launch.config))
    }
}

/// A scratch repo with one TypeScript file, a `PATH` directory holding a
/// `typescript-language-server` and a lock directory.
pub(crate) struct Setup {
    pub root: PathBuf,
    pub file: PathBuf,
    pub bin: PathBuf,
    pub locks: PathBuf,
}

pub(crate) fn setup(tag: &str) -> Setup {
    let root = scratch(tag).canonicalize().unwrap();
    let file = root.join("a.ts");
    std::fs::write(&file, "export const a = 1;\n").unwrap();
    let bin = scratch(&format!("{tag}-bin"));
    install_ts_server(&bin);
    Setup {
        root,
        file,
        bin,
        locks: scratch(&format!("{tag}-lock")),
    }
}

pub(crate) fn trusted() -> LspSettings {
    LspSettings {
        trust_workspace: true,
        ..LspSettings::default()
    }
}

pub(crate) fn options<'a>(
    setup: &'a Setup,
    settings: &'a LspSettings,
    path_dirs: &'a [PathBuf],
) -> Options<'a> {
    Options {
        settings,
        path_dirs,
        lock_dir: &setup.locks,
        probe_timeout: Duration::from_secs(10),
        timeout: Some(Duration::from_secs(10)),
        interrupt: None,
    }
}

/// The steps that answer `initialize` with `capabilities` and read `initialized`.
pub(crate) fn initialize_script(capabilities: Value) -> Vec<Step> {
    vec![
        Step::Reply {
            method: "initialize",
            result: json!({"capabilities": capabilities}),
        },
        Step::Expect("initialized"),
    ]
}
