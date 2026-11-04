// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The one lock that covers a whole gc run. A lock folder holds an owner token
//! (pid and a random nonce). The holder refreshes the folder's mtime after each
//! item and re-reads the token before each destructive call, so a lock that was
//! stolen after a long sleep aborts the run instead of racing the thief.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::guard::checked_remove_lock;
use super::{Clock, RUN_LOCK_STALE};
use crate::common::atomic::acquire_dir_lock_aged;
use crate::common::time::{epoch_secs_of, now_nanos};

/// The lock folder name under the gc folder.
pub const LOCK_DIR: &str = ".lock";
/// A lock folder that a taker renamed away before removing it starts with this.
pub const STALE_CLAIM_PREFIX: &str = ".lock.stale-";
/// The token file inside the lock folder.
pub const TOKEN_FILE: &str = "owner";
/// The stale age handed to the shared acquire helper. It never fires, because
/// staleness is judged here from the injected clock.
const NEVER_STALE: Duration = Duration::MAX;
/// Pause between attempts while waiting for a busy lock.
const RETRY_DELAY: Duration = Duration::from_millis(25);

static NONCE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Why a lock call failed.
#[derive(Debug)]
pub enum LockError {
    Io(io::Error),
    /// The token is missing or belongs to another holder.
    Lost,
}

impl fmt::Display for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "gc lock error: {err}"),
            Self::Lost => write!(f, "gc lock was taken by another run"),
        }
    }
}

impl std::error::Error for LockError {}

impl From<io::Error> for LockError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

/// Runs between the staleness check and the removal of a stale lock, so a test
/// can interleave a second taker.
pub trait LockHook {
    fn after_staleness_check(&self, _lock: &Path) {}
}

/// The hook the binary uses. It does nothing.
pub struct NoHook;

impl LockHook for NoHook {}

/// Proof that the caller holds the run lock. Every destructive gc call takes
/// one and re-checks the token before it acts.
pub struct RunGuard<'a> {
    lock: PathBuf,
    token: String,
    clock: &'a dyn Clock,
}

impl RunGuard<'_> {
    pub fn now(&self) -> i64 {
        self.clock.now()
    }

    pub fn lock_path(&self) -> &Path {
        &self.lock
    }

    /// Fails with [`LockError::Lost`] unless the lock still holds this run's token.
    pub fn verify(&self) -> Result<(), LockError> {
        match fs::read_to_string(self.lock.join(TOKEN_FILE)) {
            Ok(found) if found == self.token => Ok(()),
            Ok(_) => Err(LockError::Lost),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Err(LockError::Lost),
            Err(err) => Err(err.into()),
        }
    }

    /// Sets the lock folder's mtime to now, or the token's where a folder
    /// mtime cannot be set.
    pub fn touch(&self) -> Result<(), LockError> {
        let stamp = UNIX_EPOCH + Duration::from_secs(u64::try_from(self.now()).unwrap_or(0));
        if set_mtime(&self.lock, stamp).is_ok() {
            return Ok(());
        }
        let token = fs::OpenOptions::new()
            .write(true)
            .open(self.lock.join(TOKEN_FILE))?;
        token.set_modified(stamp)?;
        Ok(())
    }
}

fn set_mtime(path: &Path, stamp: SystemTime) -> io::Result<()> {
    fs::File::open(path)?.set_modified(stamp)
}

/// What `with_run_lock` did.
#[derive(Debug, PartialEq, Eq)]
pub enum RunOutcome<T> {
    /// Another run holds the lock; the closure did not run.
    AlreadyRunning,
    Ran(T),
}

/// Removes this run's lock on drop, unless another run now owns it.
struct Release<'a> {
    gc_dir: &'a Path,
    lock: PathBuf,
    token: String,
}

impl Drop for Release<'_> {
    fn drop(&mut self) {
        let ours = fs::read_to_string(self.lock.join(TOKEN_FILE)).is_ok_and(|t| t == self.token);
        if !ours {
            return;
        }
        if let Err(err) = checked_remove_lock(self.gc_dir, &self.lock) {
            eprintln!("playbook: gc could not release its lock: {err}");
        }
    }
}

/// Takes the run lock in `gc_dir`, waiting up to `wait`, and runs `f` with
/// proof that it holds it. Returns `AlreadyRunning` without calling `f` when it
/// cannot get the lock. This is the only way to take the gc lock.
pub fn with_run_lock<T>(
    gc_dir: &Path,
    wait: Duration,
    clock: &dyn Clock,
    hook: &dyn LockHook,
    f: impl FnOnce(&RunGuard) -> T,
) -> Result<RunOutcome<T>, LockError> {
    fs::create_dir_all(gc_dir)?;
    let lock = gc_dir.join(LOCK_DIR);
    let deadline = Instant::now() + wait;
    loop {
        if acquire_dir_lock_aged(&lock, 1, Duration::ZERO, NEVER_STALE) {
            break;
        }
        if claim_stale(gc_dir, &lock, clock, hook)? {
            continue;
        }
        if Instant::now() >= deadline {
            return Ok(RunOutcome::AlreadyRunning);
        }
        thread::sleep(RETRY_DELAY);
    }
    let token = new_token();
    let release = Release {
        gc_dir,
        lock: lock.clone(),
        token: token.clone(),
    };
    fs::write(lock.join(TOKEN_FILE), &token)?;
    let guard = RunGuard { lock, token, clock };
    let result = f(&guard);
    drop(release);
    Ok(RunOutcome::Ran(result))
}

fn new_token() -> String {
    format!(
        "{} {:x}-{:x}",
        std::process::id(),
        now_nanos(),
        NONCE_SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// What a taker sees of a lock folder: the newest mtime of the folder and its
/// token, and the token text.
#[derive(Debug, PartialEq, Eq)]
struct LockState {
    mtime: i64,
    token: Option<String>,
}

fn read_state(lock: &Path) -> Option<LockState> {
    let dir = epoch_secs_of(fs::metadata(lock).ok()?.modified().ok()?)?;
    let token_path = lock.join(TOKEN_FILE);
    let token_time = fs::metadata(&token_path)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(epoch_secs_of);
    Some(LockState {
        mtime: token_time.map_or(dir, |t| t.max(dir)),
        token: fs::read_to_string(token_path).ok(),
    })
}

fn is_stale(state: &LockState, now: i64) -> bool {
    let stale_secs = i64::try_from(RUN_LOCK_STALE.as_secs()).unwrap_or(i64::MAX);
    now.saturating_sub(state.mtime) >= stale_secs
}

/// Claims a stale lock by renaming it away, then removes it. Returns whether
/// this call removed one, so the caller retries the acquire. A lock that is not
/// stale, that changed since the check, or whose rename lost a race counts as
/// busy.
fn claim_stale(
    gc_dir: &Path,
    lock: &Path,
    clock: &dyn Clock,
    hook: &dyn LockHook,
) -> Result<bool, LockError> {
    let Some(first) = read_state(lock) else {
        return Ok(false);
    };
    if !is_stale(&first, clock.now()) {
        return Ok(false);
    }
    hook.after_staleness_check(lock);
    let unchanged = read_state(lock).is_some_and(|again| again == first);
    if !unchanged {
        return Ok(false);
    }
    let claimed = gc_dir.join(format!(
        "{STALE_CLAIM_PREFIX}{}",
        new_token().replace(' ', "-")
    ));
    if fs::rename(lock, &claimed).is_err() {
        return Ok(false);
    }
    checked_remove_lock(gc_dir, &claimed)
        .map_err(|err| LockError::Io(io::Error::other(err.to_string())))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gc::FixedClock;
    use std::cell::Cell;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    /// Set at first use to the real time, because a fresh lock folder carries a real mtime.
    static NOW: std::sync::LazyLock<i64> = std::sync::LazyLock::new(crate::common::time::now_secs);
    const STALE_SECS: i64 = 3600;

    fn gc_dir(tag: &str) -> PathBuf {
        let dir = crate::common::test_support::scratch_dir(tag).join("gc");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn age_lock(lock: &Path, age_secs: i64) {
        let stamp = UNIX_EPOCH + Duration::from_secs(u64::try_from(*NOW - age_secs).unwrap());
        let token = lock.join(TOKEN_FILE);
        if token.exists() {
            set_mtime(&token, stamp).unwrap();
        }
        set_mtime(lock, stamp).unwrap();
    }

    fn plant_lock(gc: &Path, age_secs: i64, with_token: bool) -> PathBuf {
        let lock = gc.join(LOCK_DIR);
        fs::create_dir_all(&lock).unwrap();
        if with_token {
            fs::write(lock.join(TOKEN_FILE), "999 dead-beef").unwrap();
        }
        age_lock(&lock, age_secs);
        lock
    }

    fn try_lock(gc: &Path, clock: &FixedClock) -> RunOutcome<()> {
        with_run_lock(gc, Duration::ZERO, clock, &NoHook, |_| ()).unwrap()
    }

    #[test]
    fn lock_returns_already_running() {
        let gc = gc_dir("lock-busy");
        let clock = FixedClock(crate::common::time::now_secs());
        let ran = Cell::new(false);

        let outer = with_run_lock(&gc, Duration::ZERO, &clock, &NoHook, |_| {
            with_run_lock(&gc, Duration::ZERO, &clock, &NoHook, |_| ran.set(true)).unwrap()
        })
        .unwrap();

        assert_eq!(outer, RunOutcome::Ran(RunOutcome::AlreadyRunning));
        assert!(!ran.get());
    }

    #[test]
    fn sequential_runs_both_acquire() {
        let gc = gc_dir("lock-seq");
        let clock = FixedClock(*NOW);

        let first = try_lock(&gc, &clock);
        let second = try_lock(&gc, &clock);

        assert_eq!(first, RunOutcome::Ran(()));
        assert_eq!(second, RunOutcome::Ran(()));
        assert!(!gc.join(LOCK_DIR).exists());
    }

    #[test]
    fn lock_stale_boundary_uses_injected_clock() {
        let gc = gc_dir("lock-boundary");
        let clock = FixedClock(*NOW);
        plant_lock(&gc, STALE_SECS - 1, true);

        let under = try_lock(&gc, &clock);
        plant_lock(&gc, STALE_SECS, true);
        let at = try_lock(&gc, &clock);

        assert_eq!(under, RunOutcome::AlreadyRunning);
        assert_eq!(at, RunOutcome::Ran(()));
    }

    #[test]
    fn stale_lock_with_token_is_removed() {
        let gc = gc_dir("lock-stale-token");
        plant_lock(&gc, STALE_SECS + 10, true);

        let outcome = try_lock(&gc, &FixedClock(*NOW));

        assert_eq!(outcome, RunOutcome::Ran(()));
        let leftovers = fs::read_dir(&gc).unwrap().count();
        assert_eq!(leftovers, 0);
    }

    struct Refresh;

    impl LockHook for Refresh {
        fn after_staleness_check(&self, lock: &Path) {
            age_lock(lock, 0);
        }
    }

    #[test]
    fn stale_removal_rechecks_before_removing() {
        let gc = gc_dir("lock-recheck");
        let lock = plant_lock(&gc, STALE_SECS + 10, true);

        let outcome =
            with_run_lock(&gc, Duration::ZERO, &FixedClock(*NOW), &Refresh, |_| ()).unwrap();

        assert_eq!(outcome, RunOutcome::AlreadyRunning);
        assert!(lock.exists());
    }

    #[test]
    fn holder_aborts_when_token_gone() {
        let gc = gc_dir("lock-token-gone");
        let clock = FixedClock(*NOW);

        let verdict = with_run_lock(&gc, Duration::ZERO, &clock, &NoHook, |guard| {
            let token = guard.lock_path().join(TOKEN_FILE);
            fs::rename(&token, guard.lock_path().join("moved-away")).unwrap();
            guard.verify()
        })
        .unwrap();

        assert!(matches!(verdict, RunOutcome::Ran(Err(LockError::Lost))));
    }

    #[test]
    fn release_after_steal_keeps_thief_lock() {
        let gc = gc_dir("lock-steal");
        let clock = FixedClock(*NOW);

        with_run_lock(&gc, Duration::ZERO, &clock, &NoHook, |guard| {
            fs::write(guard.lock_path().join(TOKEN_FILE), "1 thief").unwrap();
        })
        .unwrap();

        let token = fs::read_to_string(gc.join(LOCK_DIR).join(TOKEN_FILE)).unwrap();
        assert_eq!(token, "1 thief");
    }

    #[test]
    fn touch_sets_the_folder_mtime_from_the_clock() {
        let gc = gc_dir("lock-touch");
        let clock = FixedClock(*NOW);

        let mtime = with_run_lock(&gc, Duration::ZERO, &clock, &NoHook, |guard| {
            age_lock(guard.lock_path(), 500);
            guard.touch().unwrap();
            read_state(guard.lock_path()).unwrap().mtime
        })
        .unwrap();

        assert_eq!(mtime, RunOutcome::Ran(*NOW));
    }

    #[test]
    fn third_taker_after_claim_is_serialized() {
        let gc = gc_dir("lock-third");
        plant_lock(&gc, STALE_SECS + 10, true);
        let clock = Arc::new(FixedClock(*NOW));
        let holding = Arc::new(AtomicBool::new(false));
        let ran = Arc::new(AtomicU64::new(0));

        let handles: Vec<_> = (0..2)
            .map(|_| {
                let (gc, clock, holding, ran) =
                    (gc.clone(), clock.clone(), holding.clone(), ran.clone());
                thread::spawn(move || {
                    with_run_lock(&gc, Duration::ZERO, &*clock, &NoHook, |guard| {
                        // A taker that renamed a live lock away still runs, but its
                        // victim fails `verify`, so only verified holders count.
                        thread::sleep(Duration::from_millis(50));
                        if guard.verify().is_ok() {
                            assert!(!holding.swap(true, Ordering::SeqCst), "two holders at once");
                            thread::sleep(Duration::from_millis(50));
                            if guard.verify().is_ok() {
                                ran.fetch_add(1, Ordering::SeqCst);
                            }
                            holding.store(false, Ordering::SeqCst);
                        }
                    })
                    .unwrap()
                })
            })
            .collect();
        let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        let winners = outcomes
            .iter()
            .filter(|o| matches!(o, RunOutcome::Ran(())))
            .count();
        assert!(winners >= 1);
        assert!((1..=winners as u64).contains(&ran.load(Ordering::SeqCst)));
    }

    struct Barrier {
        first_renamed: Arc<AtomicBool>,
    }

    impl LockHook for Barrier {
        fn after_staleness_check(&self, _lock: &Path) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !self.first_renamed.load(Ordering::SeqCst) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
        }
    }

    #[test]
    fn two_takers_one_stale_lock() {
        let gc = gc_dir("lock-two-takers");
        plant_lock(&gc, STALE_SECS + 10, true);
        let clock = FixedClock(*NOW);
        let first_renamed = Arc::new(AtomicBool::new(false));
        let waiting_hook = Barrier {
            first_renamed: first_renamed.clone(),
        };

        let second = thread::scope(|scope| {
            let second = scope.spawn(|| {
                with_run_lock(&gc, Duration::ZERO, &clock, &waiting_hook, |_| ()).unwrap()
            });
            thread::sleep(Duration::from_millis(50));
            let first = with_run_lock(&gc, Duration::ZERO, &clock, &NoHook, |_| {
                first_renamed.store(true, Ordering::SeqCst);
            })
            .unwrap();
            assert_eq!(first, RunOutcome::Ran(()));
            second.join().unwrap()
        });

        assert!(matches!(
            second,
            RunOutcome::Ran(()) | RunOutcome::AlreadyRunning
        ));
        assert!(!gc.join(LOCK_DIR).exists());
    }
}
