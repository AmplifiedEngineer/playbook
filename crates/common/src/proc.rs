// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Deadline-bounded subprocess execution, standing in for python's
//! `subprocess.run(..., timeout=N)` (the retired shell original,
//! the retired shell originals).
//! `std::process::Command` has no built-in timeout, so an unbounded `git` or
//! `bash` call can hang a hook forever; `session-init` in particular runs on
//! every session start, so a hang there blocks the session from ever
//! beginning.
//!
//! No extra crate: the deadline is enforced by spawning the child, then
//! polling `try_wait()` against `std::time::Instant` on a short sleep. A child
//! still running at the deadline is asked to stop with SIGTERM, so a program
//! such as `git` can remove its own lock files, and is killed only if it is
//! still there a moment later.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Pause between polls of the child: fine-grained while it is young, so a
/// quick `git` call is caught within half a millisecond, coarse once it runs long.
const POLL_FAST: Duration = Duration::from_micros(500);
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const FAST_WINDOW: Duration = Duration::from_millis(200);

/// Pause before the next poll of a child that is `age` old.
fn poll_pause(age: Duration) -> Duration {
    if age < FAST_WINDOW {
        POLL_FAST
    } else {
        POLL_INTERVAL
    }
}

/// How long a child gets to exit after SIGTERM before it is killed. Windows
/// has no SIGTERM, so it kills at once and does not need this.
#[cfg(unix)]
const TERM_GRACE: Duration = Duration::from_secs(1);

/// Reads a pipe to the end on its own thread, so a child that writes more
/// than the pipe buffer holds never blocks waiting for us to read.
fn drain<R: Read + Send + 'static>(mut pipe: R) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        let _ = tx.send(bytes);
    });
    rx
}

fn collect_by(rx: &mpsc::Receiver<Vec<u8>>, deadline: Instant) -> Option<Vec<u8>> {
    rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .ok()
}

/// Run `command` to completion, capturing stdout and stderr, but give up and
/// stop the child if it has not exited within `timeout`. Returns `None` on
/// spawn failure or timeout, mirroring python's "an exception becomes an
/// empty value" contract; callers still inspect `Output::status` themselves
/// for a non-zero exit, exactly as they did with `Command::output()`. Output
/// is drained while the child runs, so a chatty child (a git hook, a push)
/// cannot stall on a full pipe. Never panics.
pub fn run_with_timeout(command: &mut Command, timeout: Duration) -> Option<Output> {
    run_bounded(command, None, timeout).ok()
}

/// Why a bounded run gave no output.
#[derive(Debug)]
pub enum RunFailure {
    /// The child could not be started.
    Spawn(std::io::Error),
    /// The child started but did not finish, or its pipes failed.
    Incomplete,
}

/// [`run_with_timeout`] that tells a spawn failure from a run that did not
/// finish.
pub fn try_run_with_timeout(
    command: &mut Command,
    timeout: Duration,
) -> Result<Output, RunFailure> {
    run_bounded(command, None, timeout)
}

/// Like [`run_with_timeout`], with `input` written to the child's standard
/// input, then closed. The write happens on its own thread, so a child that
/// does not read it cannot stall the deadline.
pub fn run_with_input(command: &mut Command, input: &[u8], timeout: Duration) -> Option<Output> {
    run_bounded(command, Some(input.to_vec()), timeout).ok()
}

/// Trimmed stdout of `program args` run in `dir` when it exits zero within
/// `timeout`. `None` on spawn failure, timeout or a non-zero exit. Standard
/// input is null and standard error is captured and dropped.
pub fn text_in(program: &str, args: &[&str], dir: &Path, timeout: Duration) -> Option<String> {
    let mut command = Command::new(program);
    command.args(args).current_dir(dir);
    let out = run_with_timeout(&mut command, timeout)?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Whether `program args` run in `dir` exits zero within `timeout`.
pub fn succeeds_in(program: &str, args: &[&str], dir: &Path, timeout: Duration) -> bool {
    let mut command = Command::new(program);
    command.args(args).current_dir(dir);
    run_with_timeout(&mut command, timeout).is_some_and(|o| o.status.success())
}

/// Asks `child` to stop with SIGTERM and waits for it to go, then kills it if
/// it is still running after [`TERM_GRACE`].
fn stop(child: &mut Child) {
    #[cfg(unix)]
    {
        if let Some(pid) = i32::try_from(child.id())
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
        }
        let until = Instant::now() + TERM_GRACE;
        while Instant::now() < until {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// What a pipe holds before a write to it blocks, at the smallest size Linux
/// and macOS use.
const PIPE_CAPACITY: usize = 64 * 1024;

/// How often and how long a spawn waits out a busy executable.
const BUSY_TRIES: u32 = 20;
const BUSY_PAUSE: Duration = Duration::from_millis(50);

/// Spawns `command`, trying again while the OS reports the executable busy.
/// A script written a moment ago stays busy while a sibling thread's fork
/// still holds its write descriptor, and the busy error clears once that
/// child runs its own exec.
fn spawn_retrying_busy(command: &mut Command) -> std::io::Result<Child> {
    let mut tries = 1;
    loop {
        match command.spawn() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < BUSY_TRIES => {
                tries += 1;
                std::thread::sleep(BUSY_PAUSE);
            }
            result => return result,
        }
    }
}

fn run_bounded(
    command: &mut Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<Output, RunFailure> {
    // The `playbook` binary restores the default SIGPIPE action so that
    // `| head` ends quietly. A child that exits before the writer thread runs,
    // or never reads its input, would then make a plain write end this whole
    // process. Input that fits the pipe goes through a pipe whose read end this
    // side keeps open until the write is done, so that write cannot hit
    // `EPIPE`. Larger input uses the child's own pipe, where the child has to
    // read it.
    let mut kept = None;
    let mut plain = None;
    match input {
        None => {
            command.stdin(Stdio::null());
        }
        Some(bytes) if bytes.len() <= PIPE_CAPACITY => {
            let (reader, writer) = std::io::pipe().map_err(|_| RunFailure::Incomplete)?;
            command.stdin(Stdio::from(
                reader.try_clone().map_err(|_| RunFailure::Incomplete)?,
            ));
            kept = Some((reader, writer, bytes));
        }
        Some(bytes) => {
            command.stdin(Stdio::piped());
            plain = Some(bytes);
        }
    }
    let mut child = spawn_retrying_busy(command.stdout(Stdio::piped()).stderr(Stdio::piped()))
        .map_err(RunFailure::Spawn)?;
    // Drop the command's own copy of the child's read end.
    command.stdin(Stdio::null());
    if let Some((reader, mut writer, bytes)) = kept {
        std::thread::spawn(move || {
            let _ = writer.write_all(&bytes);
            drop(writer);
            drop(reader);
        });
    } else if let (Some(bytes), Some(mut pipe)) = (plain, child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = pipe.write_all(&bytes);
        });
    }
    let (Some(out_pipe), Some(err_pipe)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(RunFailure::Incomplete);
    };
    let out_rx = drain(out_pipe);
    let err_rx = drain(err_pipe);

    let started = Instant::now();
    let deadline = started + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(_) => {
                stop(&mut child);
                return Err(RunFailure::Incomplete);
            }
        }
        if Instant::now() >= deadline {
            stop(&mut child);
            return Err(RunFailure::Incomplete);
        }
        std::thread::sleep(poll_pause(started.elapsed()));
    };
    // A grandchild that keeps a pipe open past the child's exit must not hang
    // us either: the same deadline bounds the final read.
    let stdout = collect_by(&out_rx, deadline).ok_or(RunFailure::Incomplete)?;
    let stderr = collect_by(&err_rx, deadline).ok_or(RunFailure::Incomplete)?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_stdout_of_a_process_that_finishes_in_time() {
        // Arrange
        let mut command = Command::new("echo");
        command.arg("hello");

        // Act
        let got = run_with_timeout(&mut command, Duration::from_secs(5));

        // Assert
        let output = got.expect("echo should finish well within the deadline");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hello");
    }

    #[test]
    fn a_young_child_is_polled_faster_than_the_poll_interval() {
        // The wall-clock version of this test (20 quick children under 400ms)
        // failed whenever a busy host stretched process spawns. The guarded
        // behavior is the pause the loop picks, so assert that directly.

        // Act
        let at_start = poll_pause(Duration::ZERO);
        let just_before_window = poll_pause(FAST_WINDOW - Duration::from_millis(1));
        let at_window = poll_pause(FAST_WINDOW);

        // Assert
        assert!(at_start < POLL_INTERVAL, "a fresh child is polled finely");
        assert_eq!(just_before_window, at_start);
        assert_eq!(at_window, POLL_INTERVAL, "an old child is polled coarsely");
    }

    #[cfg(unix)]
    #[test]
    fn a_child_past_the_deadline_gets_sigterm_before_it_is_killed() {
        let marker = std::env::temp_dir().join(format!("proc-term-{}", crate::testing::run_id()));
        let script = format!(
            "trap 'echo stopped > {}; exit 0' TERM; while :; do sleep 0.05; done",
            marker.display()
        );
        // A SIGTERM that lands before the shell has installed its trap kills it
        // with no marker. That only happens when the machine is too busy to
        // start the shell within the deadline, so a miss retries with a longer
        // deadline instead of failing on host load.
        let mut handled = false;
        for deadline_ms in [300, 1_000, 3_000] {
            let _ = std::fs::remove_file(&marker);
            let mut command = Command::new("sh");
            command.args(["-c", &script]);

            let got = run_with_timeout(&mut command, Duration::from_millis(deadline_ms));

            assert!(got.is_none(), "a timeout reports no output");
            if std::fs::read_to_string(&marker).unwrap_or_default().trim() == "stopped" {
                handled = true;
                break;
            }
        }
        let _ = std::fs::remove_file(&marker);
        assert!(handled, "the child ran its SIGTERM handler");
    }

    #[cfg(unix)]
    #[test]
    fn a_child_that_ignores_sigterm_is_killed_after_the_grace_period() {
        let mut command = Command::new("sh");
        command.args(["-c", "trap '' TERM; while :; do sleep 0.05; done"]);
        let started = Instant::now();

        let got = run_with_timeout(&mut command, Duration::from_millis(200));

        assert!(got.is_none());
        let waited = started.elapsed();
        assert!(
            waited >= TERM_GRACE,
            "it was given the grace period: {waited:?}"
        );
        assert!(
            waited < Duration::from_secs(10),
            "and then killed: {waited:?}"
        );
    }

    #[test]
    fn run_with_input_feeds_standard_input_to_the_child() {
        let mut command = Command::new("cat");

        let got = run_with_input(&mut command, b"from stdin", Duration::from_secs(5));

        let output = got.expect("cat should finish well within the deadline");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "from stdin");
    }

    #[test]
    fn text_in_returns_trimmed_stdout_and_none_on_failure_or_timeout() {
        let dir = std::env::temp_dir();

        assert_eq!(
            text_in("echo", &["hi"], &dir, Duration::from_secs(5)).as_deref(),
            Some("hi")
        );
        assert_eq!(text_in("false", &[], &dir, Duration::from_secs(5)), None);
        let started = Instant::now();
        assert_eq!(
            text_in("sleep", &["5"], &dir, Duration::from_millis(50)),
            None
        );
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn succeeds_in_is_false_for_a_hung_child() {
        let dir = std::env::temp_dir();

        assert!(succeeds_in("true", &[], &dir, Duration::from_secs(5)));
        assert!(!succeeds_in(
            "sleep",
            &["5"],
            &dir,
            Duration::from_millis(50)
        ));
    }

    #[test]
    fn returns_none_when_the_child_outlives_the_deadline() {
        // Arrange
        let mut command = Command::new("sleep");
        command.arg("2");
        let started = Instant::now();

        // Act
        let got = run_with_timeout(&mut command, Duration::from_millis(50));

        // Assert
        assert!(got.is_none());
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "should give up at the deadline instead of waiting for the child"
        );
    }

    #[test]
    fn returns_none_on_spawn_failure() {
        // Arrange
        let mut command = Command::new("playbook-proc-test-binary-that-does-not-exist");

        // Act
        let got = run_with_timeout(&mut command, Duration::from_secs(1));

        // Assert
        assert!(got.is_none());
    }

    #[test]
    fn a_child_writing_far_more_than_the_pipe_buffer_does_not_stall() {
        // Arrange: 300 KB on each stream, several times the 64 KB pipe buffer.
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "head -c 300000 /dev/zero | tr '\\0' a; head -c 300000 /dev/zero | tr '\\0' b >&2",
        ]);
        let started = Instant::now();

        // Act
        let got = run_with_timeout(&mut command, Duration::from_secs(10));

        // Assert
        let output = got.expect("the child finishes once its output is drained");
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 300_000);
        assert_eq!(output.stderr.len(), 300_000);
        assert!(output.stdout.iter().all(|&b| b == b'a'));
        assert!(output.stderr.iter().all(|&b| b == b'b'));
        assert!(started.elapsed() < Duration::from_secs(8));
    }
}
