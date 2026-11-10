// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Helpers for the integration tests under `tests/`. Not part of the public
//! interface.

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

/// How long the first launch of a freshly written executable may take. On
/// macOS the first exec of a new file waits while the system scans it: under
/// load, with many test processes writing stubs at once, that wait passes the
/// 2 s that `pbtop --version` gets, and a script placed directly in the temp
/// root waits about 50 s. A test that measures nothing about time gives that
/// launch this long, once, before the timed call.
pub const FIRST_LAUNCH_LIMIT: Duration = Duration::from_secs(180);

/// The permission bits of a test stub: owner may write, everyone may run.
#[cfg(unix)]
const STUB_MODE: u32 = 0o755;

/// An id that is unique to this process run, for naming scratch directories.
///
/// The process id alone is not: ids are reused, and scratch directories from
/// earlier runs are never removed, so a new process with a reused id walks
/// into a stale repository or config left by an old one (a fresh `git remote
/// add origin` then fails because the old run already added it). The start
/// time in nanoseconds makes the id differ between runs.
pub fn run_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        format!("{}-{nanos}", std::process::id())
    })
}

/// Runs `path` once with no arguments and drops the result, so the system
/// scans it now and the timed call that follows starts it at once. The run
/// is bounded by [`FIRST_LAUNCH_LIMIT`] and its outcome does not matter: a
/// stub that exits non-zero without arguments still gets scanned. The run
/// has an empty environment and an empty `PATH`, so a stub that forwards to
/// another program, such as `exec claude`, cannot start the real one.
pub fn warm_executable(path: &Path) {
    let mut command = std::process::Command::new(path);
    command.env_clear().env("PATH", "");
    let _ = crate::proc::run_with_timeout(&mut command, FIRST_LAUNCH_LIMIT);
}

/// Writes `text` to `path` as an executable script, flushes it to disk and
/// closes it, then warms it with [`warm_executable`]. A stub written with a
/// plain `fs::write` can still be open when a sibling thread forks, which
/// makes its first exec fail as busy, and its first run stalls while the
/// system scans it.
#[cfg(unix)]
pub fn write_stub(path: &Path, text: &str) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(STUB_MODE)
        .open(path)
        .expect("create the stub");
    file.write_all(text.as_bytes()).expect("write the stub");
    file.sync_all().expect("flush the stub");
    drop(file);
    warm_executable(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_id_is_stable_within_a_run_and_carries_the_process_id() {
        assert_eq!(run_id(), run_id());
        assert!(run_id().starts_with(&format!("{}-", std::process::id())));
    }

    #[cfg(unix)]
    #[test]
    fn a_written_stub_is_executable_and_runs() {
        let dir = std::env::temp_dir().join(format!("stub-{}", run_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stub = dir.join("tool");

        write_stub(&stub, "#!/bin/sh\necho ready\n");
        let out = std::process::Command::new(&stub).output().unwrap();

        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ready");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
