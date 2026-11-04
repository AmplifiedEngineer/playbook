// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The collector's lifetime lock, and how starters find, start and stop it.
//!
//! The collector takes an exclusive lock on `collector.lock` before it binds
//! and keeps it until it exits. The OS drops the lock on any exit, so a held
//! lock always means a live collector and there is no stale state to clean up.
//! The holder writes `<pid> <version>\n` into the file through the locked
//! handle, because `collector.json` is written late in startup and keeps a
//! dead pid after a crash.
//!
//! Starters (`ensure`) check the lock with one open and one shared
//! `try_lock`. A free lock spawns a detached collector, throttled by the
//! respawn stamp `collector.spawned`. A holder from another version is asked
//! to stop and replaced. `stop` sends the authenticated shutdown request and,
//! when allowed, falls back to `SIGKILL` after checking the pid is really the
//! collector.

use super::collector::{pid_alive, state_path};
use super::run::Paths;
use crate::util::atomic::ensure_private_dir;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const LOCK_FILE: &str = "collector.lock";
/// Owner only, like the rest of the data folder.
#[cfg(unix)]
const LOCK_MODE: u32 = 0o600;
/// A starter may hold a shared probe lock for a moment, so a busy lock is
/// retried a few times before it counts as a live collector.
const QUICK_RETRIES: u32 = 3;
const RETRY_PAUSE: Duration = Duration::from_millis(50);
/// How long to keep retrying while the holder line names no live process:
/// a descheduled starter's probe must not make a lone new collector give up.
const INCOMPLETE_HOLDER_WAIT: Duration = Duration::from_secs(1);

/// What to do when telemetry points at the collector and none runs: the
/// commands and the session hook start it (`ensure`), so no service is needed.
pub const START_HINT: &str = "run any playbook command or start a session";

/// Test-only: the respawn stamp window in seconds, so a test can let every
/// starter spawn. Read only in debug builds.
pub const STAMP_SECS_VAR: &str = "PLAYBOOK_COLLECTOR_STAMP_SECS";
/// Test-only: the version a collector writes in its holder line, so a test
/// can stage an upgrade. Read only in debug builds.
pub const TEST_VERSION_VAR: &str = "PLAYBOOK_COLLECTOR_TEST_VERSION";
/// Test-only: connection threads read the request and never reply, so a
/// test can stage a hung collector. Read only in debug builds.
pub const TEST_HANG_VAR: &str = "PLAYBOOK_COLLECTOR_TEST_HANG";
/// The test-only variables, which the real spawner never passes on.
pub const TEST_VARS: [&str; 3] = [STAMP_SECS_VAR, TEST_VERSION_VAR, TEST_HANG_VAR];

/// A test-only variable. A release build never reads one, so a stray
/// variable cannot change how an installed collector behaves.
#[cfg(debug_assertions)]
fn test_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

#[cfg(not(debug_assertions))]
fn test_var(_name: &str) -> Option<String> {
    None
}

/// Whether connection threads must hang instead of replying (tests only).
pub fn test_hang() -> bool {
    test_var(TEST_HANG_VAR).is_some_and(|v| v != "0")
}

/// The version this process writes as the lock holder.
fn holder_version() -> String {
    test_var(TEST_VERSION_VAR).unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

/// The result of asking for the lifetime lock.
#[derive(Debug)]
pub enum Lock {
    /// This process holds the lock until the file is dropped.
    Held(File),
    /// Another collector holds it.
    Busy,
    /// The lock file cannot be opened or locked.
    Unavailable(std::io::Error),
}

/// `collector.lock`, next to `collector.json`.
pub fn lock_path(paths: &Paths) -> PathBuf {
    state_path(paths).with_file_name(LOCK_FILE)
}

/// What a collector writes into the lock file.
fn holder_line() -> String {
    format!("{} {}\n", std::process::id(), holder_version())
}

/// Who holds the lock, from its holder line. Both are `None` while the
/// holder is still starting (no whole line yet) or the line is unreadable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Holder {
    pub pid: Option<u32>,
    pub version: Option<String>,
}

/// A whole `<pid> <version>\n` line. Without the newline the holder is still
/// starting, or the file is only held by a shared probe.
fn parse_holder(text: &str) -> Holder {
    let whole = text
        .strip_suffix('\n')
        .and_then(|line| line.split_once(' '))
        .filter(|(_, version)| !version.is_empty())
        .and_then(|(pid, version)| Some((pid.parse().ok()?, version)));
    match whole {
        Some((pid, version)) => Holder {
            pid: Some(pid),
            version: Some(version.to_string()),
        },
        None => Holder::default(),
    }
}

fn holder_pid(text: &str) -> Option<u32> {
    parse_holder(text).pid
}

/// Whether the lock file names a running collector. A dead collector leaves
/// a whole line behind, so a dead pid counts like a partial line.
fn holder_is_live(text: &str) -> bool {
    holder_pid(text).is_some_and(pid_alive)
}

fn holder_text(mut file: &File) -> String {
    let mut text = String::new();
    if file.rewind().is_ok() {
        let _ = file.read_to_string(&mut text);
    }
    text
}

/// Creates `path`'s folder only when it is missing: an existing one keeps
/// its mode, so a read-only folder fails at the open instead of being repaired.
fn ensure_parent(path: &Path) -> std::io::Result<()> {
    match path.parent().filter(|p| !p.exists()) {
        Some(parent) => ensure_private_dir(parent),
        None => Ok(()),
    }
}

/// Read and write, created owner-only, never truncated on open.
fn open_private(path: &Path) -> std::io::Result<File> {
    ensure_parent(path)?;
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(LOCK_MODE);
    }
    options.open(path)
}

/// Replaces the file's contents with `text` in one write, so a reader sees
/// either nothing or the whole line. The rewind matters: `set_len` does not
/// move the cursor a read left behind.
fn rewrite(mut file: &File, text: &str) -> std::io::Result<()> {
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(text.as_bytes())
}

/// Takes the lifetime lock and records this process as the holder.
pub fn acquire(paths: &Paths) -> Lock {
    let file = match open_private(&lock_path(paths)) {
        Ok(f) => f,
        Err(e) => return Lock::Unavailable(e),
    };
    let started = Instant::now();
    let mut tries = 0;
    loop {
        match file.try_lock() {
            Ok(()) => break,
            Err(TryLockError::Error(e)) => return Lock::Unavailable(e),
            Err(TryLockError::WouldBlock) => {
                tries += 1;
                let settled = tries > QUICK_RETRIES
                    && (holder_is_live(&holder_text(&file))
                        || started.elapsed() >= INCOMPLETE_HOLDER_WAIT);
                if settled {
                    return Lock::Busy;
                }
                std::thread::sleep(RETRY_PAUSE);
            }
        }
    }
    match rewrite(&file, &holder_line()) {
        Ok(()) => Lock::Held(file),
        Err(e) => Lock::Unavailable(e),
    }
}

/// What a starter sees of the lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockState {
    /// No collector holds it (or the file does not exist yet).
    Free,
    /// A collector holds it.
    Held(Holder),
    /// The lock file exists but cannot be opened or locked.
    Unavailable,
}

/// One open and one shared `try_lock`, released at once. A shared probe never
/// blocks another probe, and a collector's `acquire` retries past it. The
/// file is opened without `create`, so `doctor` and `--version` never make it.
pub fn lock_state(paths: &Paths) -> LockState {
    let file = match File::open(lock_path(paths)) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return LockState::Free,
        Err(_) => return LockState::Unavailable,
    };
    match file.try_lock_shared() {
        Ok(()) => LockState::Free,
        Err(TryLockError::WouldBlock) => {
            // Two equal reads, so a line being rewritten is not taken for a
            // holder: it then reads as starting.
            let (first, second) = (holder_text(&file), holder_text(&file));
            LockState::Held(if first == second {
                parse_holder(&first)
            } else {
                Holder::default()
            })
        }
        Err(TryLockError::Error(_)) => LockState::Unavailable,
    }
}

const STAMP_FILE: &str = "collector.spawned";
/// How long one spawn holds off the next, so a crash loop does not spawn on
/// every prompt.
pub const SPAWN_WINDOW: Duration = Duration::from_secs(10);
/// How long a collector that found its port owned by another program holds
/// off the next spawn: each one would fail the same way.
pub const PORT_BUSY_WINDOW: Duration = Duration::from_secs(300);
/// The reason a plain spawn writes; `stamp_reason` reports only other ones.
const SPAWN_REASON: &str = "spawned";

/// `collector.spawned`, next to the lock.
pub fn stamp_path(paths: &Paths) -> PathBuf {
    state_path(paths).with_file_name(STAMP_FILE)
}

/// `<epoch> <window seconds> <reason>`.
struct Stamp {
    at: i64,
    window: i64,
    reason: String,
}

impl Stamp {
    fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim_end_matches('\n').splitn(3, ' ');
        Some(Self {
            at: parts.next()?.parse().ok()?,
            window: parts.next()?.parse().ok()?,
            reason: parts.next().unwrap_or_default().to_string(),
        })
    }

    fn line(at: i64, window: Duration, reason: &str) -> String {
        format!("{at} {} {reason}\n", window.as_secs())
    }

    /// A stamp from the future (the clock stepped back) is stale, so it
    /// cannot hold spawns off for longer than its window.
    fn fresh(&self, now: i64) -> bool {
        self.at <= now && now < self.at.saturating_add(self.window)
    }
}

/// The stamp window a starter uses: `SPAWN_WINDOW`, or the test override.
#[cfg(any(unix, test))]
fn spawn_window() -> Duration {
    test_var(STAMP_SECS_VAR)
        .and_then(|v| v.parse().ok())
        .map_or(SPAWN_WINDOW, Duration::from_secs)
}

/// Takes the right to spawn now. False while a stamp is fresh, or while
/// another starter is deciding: the exclusive `try_lock` on the stamp file
/// makes the check and the rewrite one step, so concurrent starters spawn once.
#[cfg(any(unix, test))]
fn take_stamp(paths: &Paths, window: Duration, now: i64) -> bool {
    let Ok(file) = open_private(&stamp_path(paths)) else {
        return false;
    };
    if file.try_lock().is_err() {
        return false;
    }
    if Stamp::parse(&holder_text(&file)).is_some_and(|s| s.fresh(now)) {
        return false;
    }
    rewrite(&file, &Stamp::line(now, window, SPAWN_REASON)).is_ok()
}

/// Holds off spawns for `window` and records why, for doctor. A collector
/// calls this when it cannot bind its recorded port.
pub fn hold_off(paths: &Paths, window: Duration, reason: &str) -> std::io::Result<()> {
    let file = open_private(&stamp_path(paths))?;
    file.lock()?;
    rewrite(
        &file,
        &Stamp::line(crate::util::time::now_secs(), window, reason),
    )
}

/// The reason a fresh stamp holds spawns off, when it is not a plain spawn.
pub fn stamp_reason(paths: &Paths) -> Option<String> {
    stamp_reason_at(paths, crate::util::time::now_secs())
}

fn stamp_reason_at(paths: &Paths, now: i64) -> Option<String> {
    let text = std::fs::read_to_string(stamp_path(paths)).ok()?;
    Stamp::parse(&text)
        .filter(|s| s.fresh(now) && s.reason != SPAWN_REASON && !s.reason.is_empty())
        .map(|s| s.reason)
}

/// The text a collector records when another program owns its port.
pub fn port_busy_reason(port: u16) -> String {
    format!("port {port} in use by another program")
}

/// What `ensure` found and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ensured {
    /// Telemetry is not set up to use the playbook collector (the CLI gates
    /// answer this; `ensure` itself never does).
    NotConfigured,
    /// A collector at this version holds the lock, or one is starting.
    Running,
    /// This call started one.
    Started,
    /// This call stopped a collector from another version, or a hung one,
    /// and started a new one.
    Replaced,
    /// None runs and none was started: the stamp holds spawns off, the old
    /// one did not stop in time, or the new one did not come up in `wait`.
    Down,
    /// This platform has no daemon support.
    NotSupported,
}

/// How hard `ensure` tries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnsureOpts {
    /// How long to wait for a spawned collector to hold the lock and listen;
    /// zero returns right after the spawn.
    pub wait: Duration,
    /// Whether to ask a running collector for its header, to find a hung one.
    pub probe: bool,
    /// How long `stop` waits for a replaced collector to let go.
    pub stop_budget: Duration,
    /// Whether `stop` may `SIGKILL` a collector that ignores the shutdown.
    pub may_kill: bool,
}

/// What `stop` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// It stopped after the shutdown request.
    Stopped,
    /// It ignored the shutdown and was killed.
    Killed,
    /// No collector holds the lock.
    NotRunning,
    /// It still holds the lock after the budget (and was not killed).
    StillRunning,
    /// The lock file cannot be read.
    Unavailable,
}

#[cfg(unix)]
pub use unix::{
    ensure_with, stop_with, Connector, Deps, ProcessCheck, RealProcess, RealSpawner, Spawner,
    TcpConnector,
};

/// Makes sure a collector at this version runs. Never panics.
#[cfg(unix)]
pub fn ensure(paths: &Paths, exe: &Path, opts: EnsureOpts) -> Ensured {
    ensure_with(paths, exe, opts, &Deps::real())
}

#[cfg(not(unix))]
pub fn ensure(_paths: &Paths, _exe: &Path, _opts: EnsureOpts) -> Ensured {
    Ensured::NotSupported
}

/// Stops the collector that holds the lock, waiting up to `budget`; with
/// `may_kill`, a holder that ignores the request is killed after the checks.
#[cfg(unix)]
pub fn stop(paths: &Paths, budget: Duration, may_kill: bool) -> Stopped {
    stop_with(paths, budget, may_kill, &Deps::real())
}

#[cfg(not(unix))]
pub fn stop(paths: &Paths, _budget: Duration, _may_kill: bool) -> Stopped {
    // Nothing can signal a holder here, so a held lock is reported, not hidden.
    match lock_state(paths) {
        LockState::Free => Stopped::NotRunning,
        LockState::Held(_) => Stopped::StillRunning,
        LockState::Unavailable => Stopped::Unavailable,
    }
}

#[cfg(unix)]
mod unix {
    use super::{
        lock_state, spawn_window, take_stamp, EnsureOpts, Ensured, LockState, Paths, Stopped,
        TEST_VARS,
    };
    use crate::collector::PROBE_DEADLINE;
    use crate::collector::{pid_alive, probe_stream, read_state, state_path, Probe, State};
    use crate::receiver::SHUTDOWN_PATH;
    use crate::util::within;
    use rustix::process::{Pid, Signal};
    use std::io::Write;
    use std::net::{Ipv4Addr, SocketAddr, TcpStream};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    /// How long the lock may take to free after a `SIGKILL`.
    const KILL_WAIT: Duration = Duration::from_secs(1);
    /// Connect deadline for the shutdown request and the liveness checks;
    /// the collector listens on this host.
    const CONNECT_DEADLINE: Duration = Duration::from_millis(300);
    /// The executable file name of a collector.
    const EXE_NAME: &str = "playbook";
    /// Linux appends this to `/proc/<pid>/exe` once the binary is replaced,
    /// which an upgrade does under a running collector.
    #[cfg(target_os = "linux")]
    const DELETED_SUFFIX: &str = " (deleted)";
    /// Retries while a just-written binary reports ETXTBSY for a moment.
    const BUSY_RETRIES: u32 = 10;
    const BUSY_PAUSE: Duration = Duration::from_millis(50);

    /// Spawns `cmd`, retrying while the binary reports ETXTBSY.
    pub(super) fn spawn_retrying(
        cmd: &mut std::process::Command,
    ) -> std::io::Result<std::process::Child> {
        let mut attempt = 0;
        loop {
            match cmd.spawn() {
                Err(e)
                    if e.kind() == std::io::ErrorKind::ExecutableFileBusy
                        && attempt < BUSY_RETRIES =>
                {
                    attempt += 1;
                    std::thread::sleep(BUSY_PAUSE);
                }
                other => return other,
            }
        }
    }
    const LOG_FILE: &str = "collector.log";

    /// Starts a collector process.
    pub trait Spawner: Sync {
        fn spawn(&self, exe: &Path, paths: &Paths) -> std::io::Result<()>;
    }

    /// Every TCP connect `ensure` and `stop` make goes through this.
    pub trait Connector: Sync {
        fn connect(&self, addr: SocketAddr, timeout: Duration) -> std::io::Result<TcpStream>;
    }

    /// Tells whether a pid is a live playbook process, and signals it.
    pub trait ProcessCheck: Sync {
        fn is_playbook(&self, pid: u32) -> bool;

        fn signal(&self, pid: u32, signal: Signal) -> bool {
            // `Pid` rejects 0 and negative ids, which `kill` would take as groups.
            i32::try_from(pid)
                .ok()
                .and_then(Pid::from_raw)
                .is_some_and(|pid| rustix::process::kill_process(pid, signal).is_ok())
        }
    }

    /// What `ensure` and `stop` use to reach the world.
    pub struct Deps<'a> {
        pub spawner: &'a dyn Spawner,
        pub connector: &'a dyn Connector,
        pub process: &'a dyn ProcessCheck,
        /// The respawn stamp window.
        pub window: Duration,
    }

    impl Deps<'static> {
        pub fn real() -> Self {
            Self {
                spawner: &RealSpawner,
                connector: &TcpConnector,
                process: &RealProcess,
                window: spawn_window(),
            }
        }
    }

    pub struct TcpConnector;

    impl Connector for TcpConnector {
        fn connect(&self, addr: SocketAddr, timeout: Duration) -> std::io::Result<TcpStream> {
            TcpStream::connect_timeout(&addr, timeout)
        }
    }

    /// Checks the executable file name of the pid.
    pub struct RealProcess;

    impl ProcessCheck for RealProcess {
        fn is_playbook(&self, pid: u32) -> bool {
            pid_alive(pid) && exe_name(pid).as_deref() == Some(EXE_NAME)
        }
    }

    #[cfg(target_os = "macos")]
    fn exe_path(pid: u32) -> Option<PathBuf> {
        // macOS has no /proc and std has no proc_pidpath, so ask ps, which
        // prints the full path of the executable for `comm`.
        let out = std::process::Command::new("/bin/ps")
            .args(["-o", "comm=", "-p", &pid.to_string()])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        let text = String::from_utf8(out.stdout).ok()?;
        let path = text.trim();
        (out.status.success() && !path.is_empty()).then(|| PathBuf::from(path))
    }

    #[cfg(target_os = "linux")]
    fn exe_path(pid: u32) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/{pid}/exe")).ok()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn exe_path(_pid: u32) -> Option<PathBuf> {
        None
    }

    pub(super) fn exe_name(pid: u32) -> Option<String> {
        let path = exe_path(pid)?;
        let name = path.file_name()?.to_string_lossy().into_owned();
        #[cfg(target_os = "linux")]
        let name = name
            .strip_suffix(DELETED_SUFFIX)
            .map_or(name.clone(), str::to_string);
        Some(name)
    }

    /// Spawns `exe usage collect --idle-seconds 0` in its own process group.
    pub struct RealSpawner;

    impl Spawner for RealSpawner {
        fn spawn(&self, exe: &Path, paths: &Paths) -> std::io::Result<()> {
            let mut cmd = collector_command(exe, paths)?;
            // Not waited on: it outlives this process on purpose.
            spawn_retrying(&mut cmd).map(drop)
        }
    }

    /// The detached collector command: no terminal, stderr appended to
    /// `collector.log`, the test-only variables removed, and its own process group, so a
    /// hook timeout that kills the hook's process group spares it.
    pub(super) fn collector_command(
        exe: &Path,
        paths: &Paths,
    ) -> std::io::Result<std::process::Command> {
        use std::os::unix::process::CommandExt;
        let state = state_path(paths);
        let dir = state.parent().unwrap_or_else(|| Path::new("."));
        let log = {
            let mut options = std::fs::OpenOptions::new();
            options.create(true).append(true);
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, super::LOCK_MODE);
            options.open(dir.join(LOG_FILE))?
        };
        let mut cmd = std::process::Command::new(exe);
        cmd.args(["usage", "collect", "--idle-seconds", "0"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(log)
            .current_dir(dir);
        for var in TEST_VARS {
            cmd.env_remove(var);
        }
        cmd.process_group(0);
        Ok(cmd)
    }

    /// `collector.json` when it names a port and a token.
    fn recorded(paths: &Paths) -> Option<State> {
        read_state(&state_path(paths)).filter(|s| s.port != 0 && !s.token.is_empty())
    }

    fn local(port: u16) -> SocketAddr {
        SocketAddr::from((Ipv4Addr::LOCALHOST, port))
    }

    fn answers(deps: &Deps, port: u16) -> bool {
        deps.connector
            .connect(local(port), CONNECT_DEADLINE)
            .is_ok()
    }

    fn probe(deps: &Deps, port: u16) -> Probe {
        match deps.connector.connect(local(port), PROBE_DEADLINE) {
            Ok(stream) => probe_stream(stream),
            Err(_) => Probe::Silent,
        }
    }

    fn lock_frees_within(paths: &Paths, bound: Duration) -> bool {
        within(bound, || lock_state(paths) == LockState::Free)
    }

    pub fn ensure_with(paths: &Paths, exe: &Path, opts: EnsureOpts, deps: &Deps) -> Ensured {
        match lock_state(paths) {
            LockState::Unavailable => Ensured::Down,
            LockState::Held(holder) => {
                // No whole line yet: a collector is starting, leave it be.
                let Some(version) = holder.version else {
                    return Ensured::Running;
                };
                if version == env!("CARGO_PKG_VERSION") {
                    if !opts.probe {
                        return Ensured::Running;
                    }
                    // A holder that has not written collector.json yet
                    // leaves the last run's port there: probing it would
                    // read a healthy new collector as hung.
                    let Some(state) = recorded(paths).filter(|s| holder.pid == Some(s.pid)) else {
                        return Ensured::Running;
                    };
                    match probe(deps, state.port) {
                        Probe::Collector => return Ensured::Running,
                        // Another program answers on the port: a new
                        // collector could not bind it either.
                        Probe::Foreign => return Ensured::Down,
                        Probe::Silent => {}
                    }
                }
                // A prompt or a command may not kill, so a holder that will
                // not stop would cost the whole budget on every call. The
                // stamp, taken before the stop, makes that once per window.
                let stamped = !opts.may_kill;
                if stamped && !take_stamp(paths, deps.window, crate::util::time::now_secs()) {
                    return Ensured::Down;
                }
                match stop_with(paths, opts.stop_budget, opts.may_kill, deps) {
                    Stopped::Stopped | Stopped::Killed | Stopped::NotRunning => {
                        spawn(paths, exe, opts, deps, Ensured::Replaced, stamped)
                    }
                    Stopped::StillRunning | Stopped::Unavailable => Ensured::Down,
                }
            }
            LockState::Free => spawn(paths, exe, opts, deps, Ensured::Started, false),
        }
    }

    /// Takes the stamp (unless the caller already did), retires a lockless older collector, spawns, and with
    /// a `wait` polls until the new one holds the lock and listens.
    fn spawn(
        paths: &Paths,
        exe: &Path,
        opts: EnsureOpts,
        deps: &Deps,
        done: Ensured,
        stamped: bool,
    ) -> Ensured {
        if !stamped && !take_stamp(paths, deps.window, crate::util::time::now_secs()) {
            return Ensured::Down;
        }
        retire_lockless(paths, opts.stop_budget, deps);
        if deps.spawner.spawn(exe, paths).is_err() {
            return Ensured::Down;
        }
        if opts.wait.is_zero() {
            return done;
        }
        let up = within(opts.wait, || {
            matches!(lock_state(paths), LockState::Held(_))
                && recorded(paths).is_some_and(|s| answers(deps, s.port))
        });
        if up {
            done
        } else {
            Ensured::Down
        }
    }

    /// A collector from 0.38.0 or older holds no lock and has no shutdown
    /// route: when one answers with the header on the recorded port, it gets
    /// `SIGTERM` after the pid checks, and the port gets `budget` to close.
    /// It runs only after a stamp was taken, so once per stamp window.
    fn retire_lockless(paths: &Paths, budget: Duration, deps: &Deps) {
        let Some(state) = recorded(paths).filter(|s| s.pid != 0) else {
            return;
        };
        let lockless = state.pid != std::process::id()
            && pid_alive(state.pid)
            && probe(deps, state.port) == Probe::Collector
            && deps.process.is_playbook(state.pid)
            // A collector that took the lock since is not lockless.
            && lock_state(paths) == LockState::Free;
        if lockless && deps.process.signal(state.pid, Signal::TERM) {
            within(budget, || !answers(deps, state.port));
        }
    }

    pub fn stop_with(paths: &Paths, budget: Duration, may_kill: bool, deps: &Deps) -> Stopped {
        let holder = match lock_state(paths) {
            LockState::Free => return Stopped::NotRunning,
            LockState::Unavailable => return Stopped::Unavailable,
            LockState::Held(h) => h,
        };
        // The token goes only to the port of the pid that holds the lock.
        if let Some(state) = recorded(paths).filter(|s| holder.pid == Some(s.pid)) {
            send_shutdown(deps, &state);
        }
        if lock_frees_within(paths, budget) {
            return Stopped::Stopped;
        }
        // A starting holder (no pid yet) is never signalled.
        let Some(pid) = holder.pid.filter(|_| may_kill) else {
            return Stopped::StillRunning;
        };
        if !may_kill_pid(paths, pid, deps) || !deps.process.signal(pid, Signal::KILL) {
            return Stopped::StillRunning;
        }
        if lock_frees_within(paths, KILL_WAIT) {
            Stopped::Killed
        } else {
            Stopped::StillRunning
        }
    }

    /// The checks before a `SIGKILL`: the lock is still held and its line
    /// still names `pid`, `collector.json` names it too (a new collector
    /// between its lock and its line rewrite would not match), and it is a
    /// live playbook process other than this one.
    fn may_kill_pid(paths: &Paths, pid: u32, deps: &Deps) -> bool {
        let same_holder = matches!(lock_state(paths), LockState::Held(h) if h.pid == Some(pid));
        let same_state = read_state(&state_path(paths)).is_some_and(|s| s.pid == pid);
        same_holder && same_state && pid != std::process::id() && deps.process.is_playbook(pid)
    }

    /// Posts the authenticated shutdown. The reply is not awaited: the lock
    /// says when the collector is gone, and a hung one never replies.
    fn send_shutdown(deps: &Deps, state: &State) {
        let Ok(mut stream) = deps.connector.connect(local(state.port), CONNECT_DEADLINE) else {
            return;
        };
        let _ = stream.set_write_timeout(Some(CONNECT_DEADLINE));
        let request = format!(
            "POST {SHUTDOWN_PATH} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            state.token
        );
        let _ = stream.write_all(request.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::scratch_paths as paths;
    use crate::util::{within, UNLOCK_BOUND};

    fn shared_probe(p: &Paths, hold: Duration) -> std::thread::JoinHandle<()> {
        let path = lock_path(p);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .read(true)
            .open(&path)
            .unwrap();
        file.try_lock_shared().unwrap();
        std::thread::spawn(move || {
            std::thread::sleep(hold);
            drop(file);
        })
    }

    /// How long the probe holds in the seeded file tests: past the quick
    /// retries, well inside the wait.
    const SEEDED_PROBE_HOLD: Duration = Duration::from_millis(300);
    /// How far ahead a stamp from a stepped-back clock is dated.
    const FUTURE_STAMP_SECS: i64 = 100;

    /// `take_stamp` once the stamp file reads as unlocked again. A child
    /// another test spawns holds a copy of the file open for a moment, and
    /// until it execs the file cannot be locked.
    fn take_stamp_soon(p: &Paths, window: Duration, now: i64) -> bool {
        within(UNLOCK_BOUND, || take_stamp(p, window, now))
    }

    /// Whether the lock reads as free once the holder let go. A probe is
    /// polled, not tried once: a child another test spawns holds a copy of the
    /// lock file open until it execs, and the copy keeps the lock.
    fn lock_frees_soon(p: &Paths) -> bool {
        within(UNLOCK_BOUND, || lock_state(p) == LockState::Free)
    }

    /// A stamp taken at `at`, written without the stamp's lock: a lock held
    /// here could outlive this call in a child another test spawns, and then
    /// the starter under test would find the stamp busy.
    fn seed_stamp(p: &Paths, at: i64) {
        let path = stamp_path(p);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, Stamp::line(at, SPAWN_WINDOW, SPAWN_REASON)).unwrap();
    }

    fn seed(p: &Paths, text: &str) {
        let path = lock_path(p);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn a_whole_holder_line_needs_a_pid_a_version_and_the_newline() {
        assert_eq!(holder_pid("123 0.39.0\n"), Some(123));
        assert_eq!(holder_pid("123 0.39.0"), None);
        assert_eq!(holder_pid(""), None);
        assert_eq!(holder_pid("abc\n"), None);
        assert_eq!(holder_pid("abc 1\n"), None);
        assert_eq!(holder_pid("123 \n"), None);
        assert_eq!(
            parse_holder("123 0.39.0\n").version.as_deref(),
            Some("0.39.0")
        );
        assert_eq!(parse_holder("123 0.3"), Holder::default());
    }

    #[cfg(unix)]
    #[test]
    fn a_dead_holder_line_is_waited_out_and_replaced() {
        let p = paths("daemon-dead-holder");
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        seed(&p, &format!("{dead} 0.0.1\n"));
        let probe = shared_probe(&p, SEEDED_PROBE_HOLD);

        assert!(matches!(acquire(&p), Lock::Held(_)));
        probe.join().unwrap();
        assert_eq!(
            std::fs::read_to_string(lock_path(&p)).unwrap(),
            holder_line()
        );
    }

    #[test]
    fn a_partial_line_read_while_waiting_is_overwritten_from_the_start() {
        let p = paths("daemon-partial-rewind");
        seed(&p, "123456789 0.0.1-partial");
        let probe = shared_probe(&p, SEEDED_PROBE_HOLD);

        let Lock::Held(_held) = acquire(&p) else {
            panic!("acquire must succeed once the probe ends");
        };
        probe.join().unwrap();
        assert_eq!(
            std::fs::read(lock_path(&p)).unwrap(),
            holder_line().as_bytes()
        );
    }

    #[test]
    fn a_short_shared_probe_is_waited_out() {
        let p = paths("daemon-probe-short");
        let probe = shared_probe(&p, Duration::from_millis(60));

        assert!(matches!(acquire(&p), Lock::Held(_)));
        probe.join().unwrap();
    }

    #[test]
    fn a_probe_held_past_the_limit_reads_as_busy() {
        let p = paths("daemon-probe-long");
        let probe = shared_probe(&p, INCOMPLETE_HOLDER_WAIT + Duration::from_millis(500));

        assert!(matches!(acquire(&p), Lock::Busy));
        probe.join().unwrap();
    }

    #[test]
    fn a_complete_holder_line_is_busy_without_the_long_wait() {
        let p = paths("daemon-complete-busy");
        let Lock::Held(_held) = acquire(&p) else {
            panic!("first acquire must succeed");
        };
        let started = Instant::now();

        assert!(matches!(acquire(&p), Lock::Busy));
        assert!(started.elapsed() < INCOMPLETE_HOLDER_WAIT);
    }

    #[test]
    fn lock_state_with_no_lock_file_is_free_and_creates_nothing() {
        let p = paths("daemon-state-missing");

        assert_eq!(lock_state(&p), LockState::Free);
        assert!(!lock_path(&p).exists());
    }

    #[test]
    fn lock_state_names_the_holder_and_frees_with_it() {
        let p = paths("daemon-state-held");
        let Lock::Held(held) = acquire(&p) else {
            panic!("acquire must succeed");
        };

        assert_eq!(
            lock_state(&p),
            LockState::Held(Holder {
                pid: Some(std::process::id()),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            })
        );
        drop(held);
        assert!(lock_frees_soon(&p));
    }

    #[test]
    fn a_shared_probe_does_not_block_another_probe() {
        let p = paths("daemon-state-probes");
        let probe = shared_probe(&p, SEEDED_PROBE_HOLD);

        assert_eq!(lock_state(&p), LockState::Free);
        probe.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_lock_file_is_unavailable() {
        use std::os::unix::fs::PermissionsExt;
        let p = paths("daemon-state-unreadable");
        seed(&p, "");
        std::fs::set_permissions(lock_path(&p), std::fs::Permissions::from_mode(0o000)).unwrap();

        assert_eq!(lock_state(&p), LockState::Unavailable);
    }

    #[test]
    fn a_fresh_stamp_blocks_a_spawn_and_an_expired_one_is_replaced() {
        let p = paths("daemon-stamp");
        let now = 1_000;
        let window = SPAWN_WINDOW;
        let secs = i64::try_from(window.as_secs()).unwrap();

        assert!(take_stamp_soon(&p, window, now));
        // Only a fresh stamp may refuse now, not a copy of the file held by a
        // child another test spawned.
        assert!(crate::util::wait_unlocked(&stamp_path(&p)));
        assert!(!take_stamp(&p, window, now + secs - 1));
        assert!(take_stamp_soon(&p, window, now + secs));
        let text = std::fs::read_to_string(stamp_path(&p)).unwrap();
        assert_eq!(text, Stamp::line(now + secs, window, SPAWN_REASON));
    }

    #[test]
    fn a_stamp_from_the_future_is_stale() {
        let p = paths("daemon-stamp-future");
        let now = 1_000;
        let reason = port_busy_reason(4318);
        std::fs::create_dir_all(stamp_path(&p).parent().unwrap()).unwrap();
        std::fs::write(
            stamp_path(&p),
            Stamp::line(now + FUTURE_STAMP_SECS, PORT_BUSY_WINDOW, &reason),
        )
        .unwrap();

        assert_eq!(stamp_reason_at(&p, now), None);
        assert!(take_stamp_soon(&p, SPAWN_WINDOW, now));
    }

    #[test]
    fn a_zero_window_never_holds_off() {
        let p = paths("daemon-stamp-zero");

        assert!(take_stamp_soon(&p, Duration::ZERO, 5));
        assert!(take_stamp_soon(&p, Duration::ZERO, 5));
    }

    #[test]
    fn an_unreadable_stamp_counts_as_expired() {
        let p = paths("daemon-stamp-garbage");
        std::fs::create_dir_all(stamp_path(&p).parent().unwrap()).unwrap();
        std::fs::write(stamp_path(&p), "garbage").unwrap();

        assert!(take_stamp_soon(&p, SPAWN_WINDOW, 5));
    }

    #[test]
    fn stamp_reason_reports_a_fresh_hold_off_only() {
        let p = paths("daemon-stamp-reason");
        assert_eq!(stamp_reason(&p), None);
        let reason = port_busy_reason(4318);

        hold_off(&p, PORT_BUSY_WINDOW, &reason).unwrap();

        assert_eq!(stamp_reason(&p), Some(reason));
        let later =
            crate::util::time::now_secs() + i64::try_from(PORT_BUSY_WINDOW.as_secs()).unwrap() + 1;
        assert_eq!(stamp_reason_at(&p, later), None);
        assert!(take_stamp_soon(&p, SPAWN_WINDOW, later));
        assert_eq!(stamp_reason_at(&p, later), None);
    }

    #[test]
    fn an_unset_test_variable_reads_as_none_and_all_share_the_prefix() {
        // An unset variable reads as nothing in every build.
        assert_eq!(test_var("PLAYBOOK_COLLECTOR_NO_SUCH_TEST_VAR"), None);
        assert!(TEST_VARS
            .iter()
            .all(|v| v.starts_with("PLAYBOOK_COLLECTOR_")));
    }

    #[cfg(unix)]
    mod starter {
        use super::super::*;
        use super::seed_stamp;
        use crate::collector::tests::Running;
        use crate::collector::{read_state, State};
        use crate::receiver::bind;
        use crate::util::scratch_dir;
        use crate::util::scratch_paths as paths;
        use crate::util::within;
        use rustix::process::Signal;
        use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
        use std::process::{Child, Command, Stdio};
        use std::sync::atomic::{AtomicUsize, Ordering};

        /// The helper child's mode; unset in a normal test run.
        const HELPER_VAR: &str = "PLAYBOOK_DAEMON_TEST_HELPER";
        /// The usage database path the helper uses for its paths.
        const HELPER_DB_VAR: &str = "PLAYBOOK_DAEMON_TEST_DB";
        /// The port the helper records in `collector.json` (hold mode).
        const HELPER_PORT_VAR: &str = "PLAYBOOK_DAEMON_TEST_PORT";
        /// Rows the mid-transaction helper inserts and never commits.
        const MIDTX_ROWS: u32 = 10_000;
        const HELPER_TEST: &str = "daemon::tests::starter::helper_process";
        const TOKEN: &str = "tok";
        /// Generous bound for a helper child or a collector to come up.
        const START_BOUND: Duration = Duration::from_secs(10);
        /// A short budget, as prompts and commands use.
        const SHORT_BUDGET: Duration = Duration::from_millis(400);
        /// Slack on top of a budget before a test calls a stop late.
        const SLACK: Duration = Duration::from_secs(1);
        /// The longest a starter may take to find a hung collector, kill it
        /// and spawn a new one.
        const HUNG_BOUND: Duration = Duration::from_secs(5);
        /// The longest `ensure` may take to give up on a holder that ignores
        /// the shutdown, without a kill.
        const IGNORED_BOUND: Duration = Duration::from_millis(600);
        /// Scheduling slack on top of `IGNORED_BOUND`.
        const IGNORED_SLACK: Duration = Duration::from_millis(250);
        /// A stop budget an in-process collector meets even on a busy runner.
        const ROUTE_BUDGET: Duration = Duration::from_secs(3);

        fn opts(probe: bool, may_kill: bool) -> EnsureOpts {
            EnsureOpts {
                wait: Duration::ZERO,
                probe,
                stop_budget: SHORT_BUDGET,
                may_kill,
            }
        }

        #[derive(Default)]
        struct CountingSpawner(AtomicUsize);

        impl Spawner for CountingSpawner {
            fn spawn(&self, _exe: &Path, _paths: &Paths) -> std::io::Result<()> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }

        #[derive(Default)]
        struct CountingConnector(AtomicUsize);

        impl Connector for CountingConnector {
            fn connect(&self, addr: SocketAddr, timeout: Duration) -> std::io::Result<TcpStream> {
                self.0.fetch_add(1, Ordering::SeqCst);
                TcpStream::connect_timeout(&addr, timeout)
            }
        }

        /// Answers `is_playbook` with `yes`, or with the real check when
        /// `yes` is `None`, and records every signal it sends for real.
        #[derive(Default)]
        struct Process {
            yes: Option<bool>,
            sent: std::sync::Mutex<Vec<Signal>>,
        }

        impl Process {
            fn saying(yes: bool) -> Self {
                Self {
                    yes: Some(yes),
                    ..Self::default()
                }
            }

            fn sent(&self) -> Vec<Signal> {
                self.sent.lock().unwrap().clone()
            }
        }

        impl ProcessCheck for Process {
            fn is_playbook(&self, pid: u32) -> bool {
                self.yes.unwrap_or_else(|| RealProcess.is_playbook(pid))
            }

            fn signal(&self, pid: u32, signal: Signal) -> bool {
                self.sent.lock().unwrap().push(signal);
                RealProcess.signal(pid, signal)
            }
        }

        struct Fakes {
            spawner: CountingSpawner,
            connector: CountingConnector,
            process: Process,
        }

        impl Fakes {
            fn new(process: Process) -> Self {
                Self {
                    spawner: CountingSpawner::default(),
                    connector: CountingConnector::default(),
                    process,
                }
            }

            fn deps(&self) -> Deps<'_> {
                Deps {
                    spawner: &self.spawner,
                    connector: &self.connector,
                    process: &self.process,
                    window: SPAWN_WINDOW,
                }
            }

            fn spawns(&self) -> usize {
                self.spawner.0.load(Ordering::SeqCst)
            }

            fn connects(&self) -> usize {
                self.connector.0.load(Ordering::SeqCst)
            }
        }

        fn ensure_now(p: &Paths, o: EnsureOpts, fakes: &Fakes) -> Ensured {
            ensure_with(p, Path::new("playbook"), o, &fakes.deps())
        }

        fn write_state(p: &Paths, pid: u32, port: u16) {
            let state = State {
                pid,
                port,
                token: TOKEN.into(),
                ..State::default()
            };
            let path = crate::collector::state_path(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, serde_json::to_vec(&state).unwrap()).unwrap();
        }

        /// Holds the lock through a file of this process with `text` as the
        /// holder line.
        fn hold_with_line(p: &Paths, text: &str) -> File {
            let file = open_private(&lock_path(p)).unwrap();
            file.try_lock().unwrap();
            rewrite(&file, text).unwrap();
            file
        }

        /// The test binary re-run as a helper process. Without the helper
        /// variable it returns at once, so a normal run passes it.
        #[test]
        fn helper_process() {
            let Ok(mode) = std::env::var(HELPER_VAR) else {
                return;
            };
            let db = PathBuf::from(std::env::var(HELPER_DB_VAR).unwrap());
            let mut p = Paths::from_home(db.parent().unwrap());
            p.db = db;
            let pid = std::process::id();
            match mode.as_str() {
                // Takes the lock and never answers a shutdown.
                "hold" => {
                    let Lock::Held(_held) = acquire(&p) else {
                        std::process::exit(1);
                    };
                    let port = std::env::var(HELPER_PORT_VAR)
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    write_state(&p, pid, port);
                    loop {
                        std::thread::park();
                    }
                }
                // An older collector: no lock, answers with the header.
                "lockless" => {
                    let listener = bind(0).unwrap();
                    write_state(&p, pid, listener.local_addr().unwrap().port());
                    for stream in listener.incoming() {
                        let Ok(mut stream) = stream else { continue };
                        let _ = stream.write_all(
                            b"HTTP/1.1 405 Method Not Allowed\r\nX-Playbook-Collector: 1\r\nContent-Length: 0\r\n\r\n",
                        );
                    }
                }
                // A writer stopped mid-transaction, as SIGTERM finds an old
                // collector that has no handler.
                "midtx" => {
                    let conn = crate::db::open_db(&p.db).unwrap();
                    conn.execute_batch(&format!(
                        "CREATE TABLE IF NOT EXISTS t(x); BEGIN; \
                         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {MIDTX_ROWS}) \
                         INSERT INTO t SELECT i FROM n;"
                    ))
                    .unwrap();
                    write_state(&p, pid, 0);
                    loop {
                        std::thread::park();
                    }
                }
                _ => std::process::exit(2),
            }
        }

        /// A helper child, killed in `Drop` so a failed test leaves none.
        struct Helper(Child);

        impl Helper {
            fn start(exe: &Path, p: &Paths, mode: &str, env: &[(&str, String)]) -> Self {
                let mut cmd = Command::new(exe);
                cmd.args(["--exact", HELPER_TEST, "--nocapture", "--test-threads", "1"])
                    .env(HELPER_VAR, mode)
                    .env(HELPER_DB_VAR, &p.db)
                    .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                // A just-copied binary can report ETXTBSY for a moment.
                let child = unix::spawn_retrying(&mut cmd).unwrap();
                let me = Self(child);
                let pid = me.pid();
                let up = within(START_BOUND, || match mode {
                    "hold" => {
                        matches!(lock_state(p), LockState::Held(h) if h.pid == Some(pid))
                            && read_state(&crate::collector::state_path(p))
                                .is_some_and(|s| s.pid == pid)
                    }
                    _ => read_state(&crate::collector::state_path(p)).is_some_and(|s| s.pid == pid),
                });
                assert!(up, "helper {mode} never came up");
                me
            }

            fn holding(p: &Paths, env: &[(&str, String)]) -> Self {
                Self::start(&std::env::current_exe().unwrap(), p, "hold", env)
            }

            fn pid(&self) -> u32 {
                self.0.id()
            }

            /// Reaps the child when it has exited, so a dead pid reads dead.
            fn exited_within(&mut self, bound: Duration) -> bool {
                within(bound, || matches!(self.0.try_wait(), Ok(Some(_))))
            }
        }

        impl Drop for Helper {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        #[test]
        fn a_held_lock_at_this_version_is_running_with_no_connect() {
            let p = paths("ensure-running");
            let Lock::Held(_held) = acquire(&p) else {
                panic!("acquire must succeed");
            };
            let fakes = Fakes::new(Process::saying(true));

            assert_eq!(ensure_now(&p, opts(false, true), &fakes), Ensured::Running);
            assert_eq!((fakes.connects(), fakes.spawns()), (0, 0));
        }

        #[test]
        fn a_starting_or_unreadable_holder_is_left_alone() {
            for (tag, line) in [("empty", ""), ("garbage", "abc"), ("partial", "123 0.3")] {
                let p = paths(&format!("ensure-starting-{tag}"));
                let _held = hold_with_line(&p, line);
                write_state(&p, 123, 1);
                let fakes = Fakes::new(Process::saying(true));

                assert_eq!(
                    ensure_now(&p, opts(true, true), &fakes),
                    Ensured::Running,
                    "{tag}"
                );
                assert!(fakes.process.sent().is_empty(), "{tag}");
                assert_eq!((fakes.connects(), fakes.spawns()), (0, 0), "{tag}");
            }
        }

        #[test]
        fn a_holder_from_another_version_is_stopped_and_replaced() {
            let p = paths("ensure-handover");
            let _collector = Running::start(&p);
            std::fs::write(lock_path(&p), format!("{} 0.0.1\n", std::process::id())).unwrap();
            let fakes = Fakes::new(Process::saying(true));

            let o = EnsureOpts {
                stop_budget: ROUTE_BUDGET,
                ..opts(false, false)
            };

            assert_eq!(ensure_now(&p, o, &fakes), Ensured::Replaced);
            assert_eq!(fakes.spawns(), 1);
            assert!(fakes.process.sent().is_empty());
            assert_eq!(lock_state(&p), LockState::Free);
        }

        #[test]
        #[cfg_attr(not(debug_assertions), ignore = "reads a debug-only variable")]
        fn a_holder_that_ignores_shutdown_without_may_kill_is_down_and_kept() {
            let p = paths("ensure-ignores");
            // Takes the shutdown through the backlog and never answers it.
            let silent = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = silent.local_addr().unwrap().port().to_string();
            let mut helper = Helper::holding(
                &p,
                &[(TEST_VERSION_VAR, "0.0.1".into()), (HELPER_PORT_VAR, port)],
            );
            assert_eq!(
                lock_state(&p),
                LockState::Held(Holder {
                    pid: Some(helper.pid()),
                    version: Some("0.0.1".into()),
                })
            );
            let fakes = Fakes::new(Process::saying(true));
            let started = Instant::now();

            assert_eq!(ensure_now(&p, opts(false, false), &fakes), Ensured::Down);
            let elapsed = started.elapsed();
            assert!(elapsed < IGNORED_BOUND + IGNORED_SLACK, "{elapsed:?}");
            // The shutdown was sent: one connect, to the recorded port.
            assert_eq!(fakes.connects(), 1);
            assert!(fakes.process.sent().is_empty());
            assert_eq!(fakes.spawns(), 0);
            assert!(!helper.exited_within(Duration::ZERO));
        }

        #[test]
        fn a_holder_not_yet_in_collector_json_is_running_and_never_probed() {
            let p = paths("ensure-holder-starting");
            let me = std::process::id();
            let _held = hold_with_line(&p, &format!("{me} {}\n", env!("CARGO_PKG_VERSION")));
            // The previous run's port, which nothing answers.
            let silent = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            write_state(&p, me + 1, silent.local_addr().unwrap().port());
            let fakes = Fakes::new(Process::saying(true));

            assert_eq!(ensure_now(&p, opts(true, true), &fakes), Ensured::Running);
            assert!(fakes.process.sent().is_empty());
            assert_eq!((fakes.connects(), fakes.spawns()), (0, 0));
            assert!(matches!(lock_state(&p), LockState::Held(h) if h.pid == Some(me)));
        }

        #[test]
        fn stop_sends_no_token_when_collector_json_names_another_pid() {
            let p = paths("stop-token-other-pid");
            let me = std::process::id();
            let _held = hold_with_line(&p, &format!("{me} 0.0.1\n"));
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            write_state(&p, me + 1, listener.local_addr().unwrap().port());
            let fakes = Fakes::new(Process::saying(true));

            assert_eq!(
                stop_with(&p, SHORT_BUDGET, false, &fakes.deps()),
                Stopped::StillRunning
            );
            assert_eq!(fakes.connects(), 0);
            assert!(fakes.process.sent().is_empty());
        }

        #[test]
        fn a_hung_collector_found_by_the_probe_is_killed_and_replaced() {
            let p = paths("ensure-hung");
            // Accepts through the backlog and never answers.
            let silent = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = silent.local_addr().unwrap().port().to_string();
            let mut helper = Helper::holding(&p, &[(HELPER_PORT_VAR, port)]);
            let fakes = Fakes::new(Process::saying(true));
            let started = Instant::now();

            assert_eq!(ensure_now(&p, opts(true, true), &fakes), Ensured::Replaced);
            assert!(started.elapsed() < HUNG_BOUND, "{:?}", started.elapsed());
            assert_eq!(fakes.process.sent(), [Signal::KILL]);
            assert_eq!(fakes.spawns(), 1);
            assert!(helper.exited_within(SLACK));
        }

        #[test]
        #[cfg_attr(not(debug_assertions), ignore = "reads a debug-only variable")]
        fn a_holder_that_is_not_playbook_is_never_signalled() {
            let p = paths("ensure-not-playbook");
            let mut helper = Helper::holding(&p, &[(TEST_VERSION_VAR, "0.0.1".into())]);
            // The real check: the helper is the test binary, not `playbook`.
            let fakes = Fakes::new(Process::default());

            assert_eq!(ensure_now(&p, opts(false, true), &fakes), Ensured::Down);
            assert!(fakes.process.sent().is_empty());
            assert!(!helper.exited_within(Duration::ZERO));

            let yes = Fakes::new(Process::saying(true));
            assert_eq!(ensure_now(&p, opts(false, true), &yes), Ensured::Replaced);
            assert_eq!(yes.process.sent(), [Signal::KILL]);
            assert!(helper.exited_within(SLACK));
        }

        #[test]
        fn stop_kills_a_holder_that_ignores_shutdown_and_the_lock_frees() {
            let p = paths("stop-kill");
            let mut helper = Helper::holding(&p, &[]);
            let fakes = Fakes::new(Process::saying(true));

            assert_eq!(
                stop_with(&p, SHORT_BUDGET, true, &fakes.deps()),
                Stopped::Killed
            );
            assert_eq!(lock_state(&p), LockState::Free);
            assert!(helper.exited_within(SLACK));
        }

        #[test]
        fn stop_needs_collector_json_to_name_the_holder_before_a_kill() {
            let p = paths("stop-stale-json");
            let mut helper = Helper::holding(&p, &[]);
            write_state(&p, helper.pid() + 1, 0);
            let fakes = Fakes::new(Process::saying(true));

            assert_eq!(
                stop_with(&p, SHORT_BUDGET, true, &fakes.deps()),
                Stopped::StillRunning
            );
            assert!(fakes.process.sent().is_empty());
            assert!(!helper.exited_within(Duration::ZERO));
        }

        #[test]
        fn stop_with_no_collector_is_not_running() {
            let p = paths("stop-none");
            let fakes = Fakes::new(Process::saying(true));

            assert_eq!(
                stop_with(&p, SHORT_BUDGET, true, &fakes.deps()),
                Stopped::NotRunning
            );
            assert_eq!(fakes.connects(), 0);
        }

        #[test]
        fn stop_shuts_a_real_collector_down_through_its_route() {
            let p = paths("stop-route");
            let _collector = Running::start(&p);
            let fakes = Fakes::new(Process::saying(true));

            assert_eq!(
                stop_with(&p, ROUTE_BUDGET, true, &fakes.deps()),
                Stopped::Stopped
            );
            assert!(fakes.process.sent().is_empty());
            assert_eq!(lock_state(&p), LockState::Free);
        }

        #[test]
        fn the_real_check_accepts_a_binary_named_playbook() {
            let p = paths("check-real-name");
            let dir = scratch_dir("check-real-bin");
            std::fs::create_dir_all(&dir).unwrap();
            let copy = dir.join("playbook");
            std::fs::copy(std::env::current_exe().unwrap(), &copy).unwrap();
            let helper = Helper::start(&copy, &p, "hold", &[]);
            // Linux names a replaced binary `<path> (deleted)`, as after an upgrade.
            #[cfg(target_os = "linux")]
            std::fs::remove_file(&copy).unwrap();

            assert!(RealProcess.is_playbook(helper.pid()));
            assert!(!RealProcess.is_playbook(std::process::id()));
        }

        #[test]
        fn sigterm_mid_transaction_leaves_the_database_consistent() {
            let p = paths("sigterm-midtx");
            let exe = std::env::current_exe().unwrap();
            let mut helper = Helper::start(&exe, &p, "midtx", &[]);

            assert!(RealProcess.signal(helper.pid(), Signal::TERM));
            assert!(helper.exited_within(START_BOUND));

            let conn = crate::db::open_db(&p.db).unwrap();
            let check: String = conn
                .query_row("PRAGMA integrity_check", [], |r| r.get(0))
                .unwrap();
            assert_eq!(check, "ok");
            let rows: i64 = conn
                .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
                .unwrap();
            assert_eq!(rows, 0);
        }

        #[test]
        fn a_free_lock_with_a_fresh_stamp_spawns_nothing() {
            let p = paths("ensure-fresh-stamp");
            seed_stamp(&p, crate::util::time::now_secs());
            let fakes = Fakes::new(Process::saying(true));

            assert_eq!(ensure_now(&p, opts(false, false), &fakes), Ensured::Down);
            assert_eq!(fakes.spawns(), 0);
        }

        #[test]
        fn a_free_lock_with_an_expired_stamp_spawns_once_and_stamps() {
            let p = paths("ensure-expired-stamp");
            let long_ago =
                crate::util::time::now_secs() - i64::try_from(SPAWN_WINDOW.as_secs()).unwrap() - 1;
            seed_stamp(&p, long_ago);
            let fakes = Fakes::new(Process::saying(true));

            assert_eq!(ensure_now(&p, opts(false, false), &fakes), Ensured::Started);
            assert_eq!(fakes.spawns(), 1);
            let stamp = Stamp::parse(&std::fs::read_to_string(stamp_path(&p)).unwrap()).unwrap();
            assert!(stamp.at > long_ago);
        }

        #[test]
        fn concurrent_starters_on_a_free_lock_spawn_once() {
            const STARTERS: usize = 16;
            let p = paths("ensure-concurrent");
            std::fs::create_dir_all(stamp_path(&p).parent().unwrap()).unwrap();
            let fakes = Fakes::new(Process::saying(true));
            let gate = std::sync::Barrier::new(STARTERS);

            std::thread::scope(|s| {
                for _ in 0..STARTERS {
                    s.spawn(|| {
                        gate.wait();
                        ensure_now(&p, opts(false, false), &fakes)
                    });
                }
            });

            assert_eq!(fakes.spawns(), 1);
        }

        #[test]
        fn a_port_hold_off_is_down_and_names_its_reason() {
            let p = paths("ensure-port-reason");
            let reason = port_busy_reason(4318);
            hold_off(&p, PORT_BUSY_WINDOW, &reason).unwrap();
            let fakes = Fakes::new(Process::saying(true));

            assert_eq!(ensure_now(&p, opts(false, false), &fakes), Ensured::Down);
            assert_eq!(fakes.spawns(), 0);
            assert_eq!(stamp_reason(&p), Some(reason));
        }

        #[test]
        fn a_lockless_older_collector_gets_one_sigterm_then_a_spawn() {
            let p = paths("ensure-lockless");
            let exe = std::env::current_exe().unwrap();
            let mut helper = Helper::start(&exe, &p, "lockless", &[]);
            let fakes = Fakes::new(Process::saying(true));

            assert_eq!(ensure_now(&p, opts(false, false), &fakes), Ensured::Started);
            assert_eq!(fakes.process.sent(), [Signal::TERM]);
            assert_eq!(fakes.spawns(), 1);
            assert!(helper.exited_within(SLACK));
        }

        #[test]
        fn a_read_only_data_folder_is_down_without_a_panic() {
            use std::os::unix::fs::PermissionsExt;
            let p = paths("ensure-read-only");
            let dir = lock_path(&p).parent().unwrap().to_path_buf();
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
            let fakes = Fakes::new(Process::saying(true));

            let got = ensure_now(&p, opts(false, false), &fakes);

            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(got, Ensured::Down);
            assert_eq!(fakes.spawns(), 0);
        }

        #[test]
        fn a_spawn_with_a_wait_reports_down_when_nothing_comes_up() {
            let p = paths("ensure-wait-down");
            let fakes = Fakes::new(Process::saying(true));
            let o = EnsureOpts {
                wait: SHORT_BUDGET,
                ..opts(false, false)
            };

            assert_eq!(ensure_now(&p, o, &fakes), Ensured::Down);
            assert_eq!(fakes.spawns(), 1);
        }

        #[test]
        fn a_probe_of_a_port_that_never_replies_is_silent_in_time() {
            let silent = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = silent.local_addr().unwrap().port();
            let started = Instant::now();

            assert_eq!(
                crate::collector::probe(port),
                crate::collector::Probe::Silent
            );
            assert!(started.elapsed() < crate::collector::PROBE_DEADLINE + SLACK);
        }

        #[test]
        fn the_collector_command_is_detached_and_drops_the_test_variables() {
            use std::os::unix::fs::PermissionsExt;
            let p = paths("spawner-command");
            let dir = crate::collector::state_path(&p)
                .parent()
                .unwrap()
                .to_path_buf();
            std::fs::create_dir_all(&dir).unwrap();
            let script = dir.join("fake-collector");
            std::fs::write(
                &script,
                "#!/bin/sh\necho \"$* $(pwd) $$ $(ps -o pgid= -p $$ | tr -d ' ')\" >&2\n",
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();

            let mut cmd = unix::collector_command(&script, &p).unwrap();
            let removed: Vec<_> = cmd
                .get_envs()
                .filter(|(_, v)| v.is_none())
                .map(|(k, _)| k.to_string_lossy().into_owned())
                .collect();
            assert_eq!(removed.len(), TEST_VARS.len());
            assert!(TEST_VARS.iter().all(|v| removed.iter().any(|r| r == v)));
            cmd.status().unwrap();

            let log = dir.join("collector.log");
            let text = std::fs::read_to_string(&log).unwrap();
            let words: Vec<&str> = text.split_whitespace().collect();
            assert_eq!(&words[..4], ["usage", "collect", "--idle-seconds", "0"]);
            assert_eq!(
                std::fs::canonicalize(words[4]).unwrap(),
                std::fs::canonicalize(&dir).unwrap()
            );
            // process_group(0) makes the child the leader of its own group.
            assert_eq!(words[5], words[6], "{text}");
            let mode = std::fs::metadata(&log).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
