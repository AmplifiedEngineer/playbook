// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A language server child process: spawn it as the leader of its own process
//! group, keep its last stderr bytes, and tear down the whole group.

use super::transport::Transport;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{self, Read};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The most recent bytes of a stream, up to a fixed capacity.
#[derive(Clone)]
pub struct StderrRing {
    capacity: usize,
    bytes: Arc<Mutex<VecDeque<u8>>>,
}

impl StderrRing {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            bytes: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    pub fn push(&self, chunk: &[u8]) {
        let Ok(mut bytes) = self.bytes.lock() else {
            return;
        };
        bytes.extend(chunk);
        let excess = bytes.len().saturating_sub(self.capacity);
        bytes.drain(..excess);
    }

    pub fn contents(&self) -> Vec<u8> {
        self.bytes
            .lock()
            .map(|b| b.iter().copied().collect())
            .unwrap_or_default()
    }
}

/// How a server process ended.
#[derive(Debug)]
pub struct Exit {
    pub status: Option<ExitStatus>,
    /// True when the server outlived its grace period and was killed.
    pub killed: bool,
}

pub struct ServerProcess {
    child: Child,
    /// Set once the leader has been reaped by `exit_within`.
    reaped: Option<ExitStatus>,
    stderr: StderrRing,
}

/// Capacity of the stderr ring kept for crash reports.
const STDERR_CAPACITY: usize = 64 * 1024;

impl ServerProcess {
    /// Starts `program` with `args` in `cwd`, no shell. Call this from the main
    /// thread: the group leader is tied to the spawning thread on some systems.
    pub fn spawn(program: &Path, args: &[OsString], cwd: &Path) -> io::Result<(Self, Transport)> {
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().ok_or_else(missing_pipe)?;
        let stdout = child.stdout.take().ok_or_else(missing_pipe)?;
        let mut err = child.stderr.take().ok_or_else(missing_pipe)?;
        let stderr = StderrRing::new(STDERR_CAPACITY);
        let sink = stderr.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while let Ok(n @ 1..) = err.read(&mut chunk) {
                sink.push(&chunk[..n]);
            }
        });
        Ok((
            Self {
                child,
                reaped: None,
                stderr,
            },
            Transport::new(stdout, stdin),
        ))
    }

    pub fn stderr_tail(&self) -> String {
        String::from_utf8_lossy(&self.stderr.contents()).into_owned()
    }

    /// Waits up to `grace` for the leader to exit on its own, then kills the
    /// whole group and reaps the leader. The group is killed even after a
    /// clean exit, because a grandchild may ignore stdin closing.
    pub fn terminate(&mut self, grace: Duration) -> Exit {
        let deadline = Instant::now() + grace;
        let mut exited = self.reaped.is_some() || group::has_exited(&mut self.child);
        while !exited && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
            exited = group::has_exited(&mut self.child);
        }
        group::kill(&mut self.child);
        let status = self.reaped.or_else(|| self.child.wait().ok());
        Exit {
            status,
            killed: !exited,
        }
    }

    /// The exit status if the leader ends within `grace`, without killing it.
    /// This reaps the leader, so call it only once the server is already lost.
    pub fn exit_within(&mut self, grace: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + grace;
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                self.reaped = Some(status);
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn missing_pipe() -> io::Error {
    io::Error::other("a piped standard stream was not available")
}

#[cfg(unix)]
mod group {
    use rustix::process::{kill_process_group, waitid, Pid, Signal, WaitId, WaitIdOptions};
    use std::process::Child;

    /// True once the leader has exited, without reaping it, so the group id
    /// stays reserved until `kill` has run.
    pub(super) fn has_exited(child: &mut Child) -> bool {
        let Some(pid) = pid_of(child) else {
            return true;
        };
        let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
        matches!(waitid(WaitId::Pid(pid), options), Ok(Some(_)))
    }

    /// SIGKILL to every member of the group the leader created. A group with
    /// no members left is already gone, which is the goal.
    pub(super) fn kill(child: &mut Child) {
        if let Some(pid) = pid_of(child) {
            let _ = kill_process_group(pid, Signal::KILL);
        }
        let _ = child.kill();
    }

    fn pid_of(child: &Child) -> Option<Pid> {
        i32::try_from(child.id()).ok().and_then(Pid::from_raw)
    }
}

#[cfg(not(unix))]
mod group {
    use std::process::Child;

    pub(super) fn has_exited(child: &mut Child) -> bool {
        matches!(child.try_wait(), Ok(Some(_)))
    }

    pub(super) fn kill(child: &mut Child) {
        let _ = child.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stderr_ring_keeps_the_last_bytes_up_to_its_capacity() {
        for (written, kept) in [(1023usize, 1023usize), (1024, 1024), (1025, 1024)] {
            // Arrange
            let ring = StderrRing::new(1024);
            let data: Vec<u8> = (0..written).map(|i| (i % 251) as u8).collect();

            // Act
            ring.push(&data);

            // Assert
            assert_eq!(ring.contents(), data[written - kept..], "{written} bytes");
        }
    }

    #[test]
    fn the_ring_drops_the_oldest_bytes_across_pushes() {
        // Arrange
        let ring = StderrRing::new(4);

        // Act
        ring.push(b"abc");
        ring.push(b"def");

        // Assert
        assert_eq!(ring.contents(), b"cdef");
    }
}
