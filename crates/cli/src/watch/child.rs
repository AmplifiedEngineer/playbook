// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Running a child process that can be stopped. The child starts in its own
//! process group, so stopping it also stops the helpers it started. No
//! `unsafe` and no `libc`: the group is stopped by running `kill`.

use std::io;

/// Seconds between looks at the kill switch while a child runs.
pub const CHECK_EVERY: i64 = 5;

/// A running child.
pub trait Child {
    /// `Some(success)` once the child has exited.
    fn try_wait(&mut self) -> io::Result<Option<bool>>;
    /// Stops the child and everything in its process group.
    fn kill_group(&mut self) -> io::Result<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    Exited {
        success: bool,
    },
    /// The kill switch was on, so the child was stopped.
    Killed,
}

/// Waits for `child`. Before each wait it asks `killed`, and when that says
/// yes it stops the child's group. `sleep` waits [`CHECK_EVERY`] seconds, so
/// tests drive it from a fake clock.
pub fn supervise(
    child: &mut dyn Child,
    killed: &dyn Fn() -> bool,
    sleep: &mut dyn FnMut(i64),
) -> io::Result<Ended> {
    loop {
        if killed() {
            child.kill_group()?;
            return Ok(Ended::Killed);
        }
        if let Some(success) = child.try_wait()? {
            return Ok(Ended::Exited { success });
        }
        sleep(CHECK_EVERY);
    }
}

#[cfg(unix)]
pub use os::{OsChild, OsSpawner};

#[cfg(unix)]
mod os {
    use std::io::{self, Read, Write};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::thread::JoinHandle;

    use super::Child;
    use crate::watch::claude::Call;

    /// Starts programs in their own process group.
    pub struct OsSpawner {
        program: String,
        kill_program: String,
    }

    impl OsSpawner {
        pub fn new(program: &str) -> Self {
            Self::with_kill_program(program, "kill")
        }

        pub fn with_kill_program(program: &str, kill_program: &str) -> Self {
            Self {
                program: program.to_string(),
                kill_program: kill_program.to_string(),
            }
        }

        /// Starts the program with the call's arguments, environment and stdin.
        pub fn spawn(&self, call: &Call) -> io::Result<OsChild> {
            let mut command = Command::new(&self.program);
            command.args(&call.argv).envs(call.env.iter().cloned());
            if let Some(dir) = &call.cwd {
                command.current_dir(dir);
            }
            let mut child = command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()?;
            let mut stdin = child.stdin.take();
            let input = call.stdin.clone().into_bytes();
            // Written on its own thread, so a child that does not read cannot
            // stall the caller.
            std::thread::spawn(move || {
                if let Some(pipe) = stdin.as_mut() {
                    let _ = pipe.write_all(&input);
                }
            });
            let mut stdout = child.stdout.take();
            let reader = std::thread::spawn(move || {
                let mut text = String::new();
                if let Some(pipe) = stdout.as_mut() {
                    let _ = pipe.read_to_string(&mut text);
                }
                text
            });
            Ok(OsChild {
                child,
                reader,
                kill_program: self.kill_program.clone(),
            })
        }
    }

    pub struct OsChild {
        child: std::process::Child,
        reader: JoinHandle<String>,
        kill_program: String,
    }

    impl OsChild {
        /// What the child wrote to standard output.
        pub fn output(self) -> String {
            self.reader.join().unwrap_or_default()
        }
    }

    impl Child for OsChild {
        fn try_wait(&mut self) -> io::Result<Option<bool>> {
            Ok(self.child.try_wait()?.map(|status| status.success()))
        }

        fn kill_group(&mut self) -> io::Result<()> {
            let group = format!("-{}", self.child.id());
            let stopped = Command::new(&self.kill_program)
                .args(["-KILL", "--", &group])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if !stopped {
                // The `kill` program is missing or refused: stop the child alone.
                self.child.kill()?;
            }
            self.child.wait().map(drop)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::clock::{Clock, ManualClock};
    use std::cell::{Cell, RefCell};

    /// Exits after `polls` calls to `try_wait`.
    struct Fake {
        polls: Cell<u32>,
        exit_after: u32,
        killed: bool,
    }

    impl Child for Fake {
        fn try_wait(&mut self) -> io::Result<Option<bool>> {
            self.polls.set(self.polls.get() + 1);
            Ok((self.polls.get() > self.exit_after).then_some(true))
        }
        fn kill_group(&mut self) -> io::Result<()> {
            self.killed = true;
            Ok(())
        }
    }

    #[test]
    fn a_child_that_exits_ends_the_wait_without_a_kill() {
        let mut child = Fake {
            polls: Cell::new(0),
            exit_after: 2,
            killed: false,
        };
        let ended = supervise(&mut child, &|| false, &mut |_| {}).unwrap();
        assert_eq!(ended, Ended::Exited { success: true });
        assert!(!child.killed);
    }

    #[test]
    fn the_kill_switch_is_checked_at_least_every_five_clock_seconds_and_stops_the_group() {
        // Arrange: the switch flips 12 clock seconds in, the child never exits.
        let clock = ManualClock::new(0);
        let looks = RefCell::new(Vec::new());
        let switch = || {
            looks.borrow_mut().push(clock.now());
            clock.now() >= 12
        };
        let mut child = Fake {
            polls: Cell::new(0),
            exit_after: u32::MAX,
            killed: false,
        };

        // Act
        let ended = supervise(&mut child, &switch, &mut |s| clock.advance(s)).unwrap();

        // Assert
        assert_eq!(ended, Ended::Killed);
        assert!(child.killed);
        let times = looks.borrow();
        assert!(times.windows(2).all(|w| w[1] - w[0] <= 5), "{times:?}");
        assert_eq!(times.last(), Some(&15));
    }

    #[cfg(unix)]
    mod real {
        use super::*;
        use crate::watch::claude::Call;

        fn call(argv: &[&str], stdin: &str) -> Call {
            Call {
                argv: argv.iter().map(|s| s.to_string()).collect(),
                env: Vec::new(),
                stdin: stdin.to_string(),
                cwd: None,
            }
        }

        fn wait_for_exit(child: &mut OsChild) {
            for _ in 0..200 {
                if child.try_wait().unwrap().is_some() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            panic!("the child did not stop");
        }

        #[test]
        fn a_real_child_gets_stdin_and_returns_its_output() {
            let mut child = OsSpawner::new("cat").spawn(&call(&[], "hello")).unwrap();
            let ended = supervise(&mut child, &|| false, &mut |_| {
                std::thread::sleep(std::time::Duration::from_millis(10));
            })
            .unwrap();
            assert_eq!(ended, Ended::Exited { success: true });
            assert_eq!(child.output(), "hello");
        }

        #[test]
        fn a_real_child_runs_in_the_directory_of_the_call() {
            // Arrange
            let dir = std::env::temp_dir().canonicalize().unwrap();
            let call = Call {
                cwd: Some(dir.display().to_string()),
                ..call(&[], "")
            };

            // Act
            let mut child = OsSpawner::new("pwd").spawn(&call).unwrap();
            supervise(&mut child, &|| false, &mut |_| {
                std::thread::sleep(std::time::Duration::from_millis(10));
            })
            .unwrap();

            // Assert
            assert_eq!(child.output().trim(), dir.display().to_string());
        }

        #[test]
        fn the_group_kill_stops_a_running_child() {
            let mut child = OsSpawner::new("sleep").spawn(&call(&["30"], "")).unwrap();
            child.kill_group().unwrap();
            wait_for_exit(&mut child);
        }

        #[test]
        fn a_missing_kill_program_falls_back_to_killing_the_child() {
            let spawner = OsSpawner::with_kill_program("sleep", "no-such-kill-program-4f2a");
            let mut child = spawner.spawn(&call(&["30"], "")).unwrap();
            child.kill_group().unwrap();
            wait_for_exit(&mut child);
        }
    }
}
