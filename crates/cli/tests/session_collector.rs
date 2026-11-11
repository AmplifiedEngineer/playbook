// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The SessionStart hook keeps the telemetry collector running for plain
//! `claude` sessions, end to end with the real binary in a scratch HOME.
//! `collector_daemon.rs` covers the daemon's done bars; this file covers the
//! path through `setup --only telemetry`.

#[path = "support/collector_guard.rs"]
mod collector_guard;

use playbook::cc::telemetry::TEST_SPAWN_VAR;
use playbook::usage::run::Paths;
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Home(PathBuf);

impl Home {
    fn new(tag: &str) -> Home {
        let dir = std::env::temp_dir().join(format!(
            "pb-session-collector-{tag}-{}",
            playbook::testing::run_id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".claude")).unwrap();
        Home(dir)
    }

    fn cmd(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_playbook"));
        cmd.env("HOME", &self.0)
            .env("SHELL", "/bin/zsh")
            .env("CLAUDE_PLUGIN_ROOT", env!("PLAYBOOK_REPO_ROOT"))
            .env_remove("CLAUDE_CONFIG_DIR")
            // CI sets `CI=true`, which makes the hook headless.
            .env_remove("CI")
            .env_remove("PLAYBOOK_HEADLESS")
            .env(TEST_SPAWN_VAR, "1")
            .current_dir(&self.0);
        for (name, _) in std::env::vars() {
            // A Claude Code tool shell passes `IS_DEMO` and the entrypoint to its
            // children; either one changes which telemetry line the hook prints.
            if name == "CLAUDE_CODE_ENABLE_TELEMETRY"
                || name == "IS_DEMO"
                || name == "CLAUDE_CODE_ENTRYPOINT"
                || name.starts_with("OTEL_")
            {
                cmd.env_remove(name);
            }
        }
        cmd
    }

    /// Runs `init --only telemetry` with a port that is free now. Init would
    /// pick the shared default 47318 for every test that runs at the same
    /// moment, and only one collector can hold it, so the others never come
    /// up. The port is not held, so another process could still take it, but
    /// that is rare next to the shared default.
    fn init_telemetry(&self) -> std::process::Output {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let dir = self.0.join(".config/playbook/data");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("collector.json"),
            format!(r#"{{"pid":0,"port":{port},"token":"test-token"}}"#),
        )
        .unwrap();
        let out = self
            .cmd()
            .args(["setup", "--only", "telemetry"])
            .output()
            .unwrap();
        assert_eq!(
            self.state().map(|s| s["port"].clone()),
            Some(Value::from(port)),
            "init must keep the port chosen for this test"
        );
        out
    }

    fn session_start(&self) {
        let mut child = self
            .cmd()
            .args(["hook", "session-init"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(br#"{"session_id":"s1","source":"startup"}"#)
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "",
            "the hook is quiet"
        );
    }

    fn state(&self) -> Option<Value> {
        let text = fs::read_to_string(self.0.join(".config/playbook/data/collector.json")).ok()?;
        serde_json::from_str(&text).ok()
    }

    fn wait_for_live_collector(&self) -> Option<(u32, u16)> {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if let Some(s) = self.state() {
                let pid = u32::try_from(s["pid"].as_u64().unwrap_or(0)).unwrap_or(0);
                let port = u16::try_from(s["port"].as_u64().unwrap_or(0)).unwrap_or(0);
                if pid != 0 && std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                    return Some((pid, port));
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        None
    }
}

impl Drop for Home {
    /// Kills the collector that holds the lock, and any late one, so no
    /// test leaves a collector running.
    fn drop(&mut self) {
        let settled = collector_guard::kill_all(&Paths::from_home(&self.0));
        let _ = fs::remove_dir_all(&self.0);
        if !std::thread::panicking() {
            assert!(settled, "a collector kept the lock after SIGKILL");
        }
    }
}

#[test]
fn a_session_start_brings_up_the_collector_and_a_second_one_reuses_it() {
    let home = Home::new("start");
    let init = home.init_telemetry();
    assert!(init.status.success());

    home.session_start();
    let first = home.wait_for_live_collector();
    home.session_start();
    let second = home.wait_for_live_collector();

    assert!(first.is_some(), "no collector came up");
    assert_eq!(first, second, "a second session must reuse the collector");
}

#[test]
fn a_session_start_without_telemetry_set_up_starts_nothing() {
    let home = Home::new("none");

    home.session_start();

    assert!(home.state().is_none());
}

#[test]
fn two_sessions_starting_at_once_bring_up_one_collector() {
    let home = Home::new("race");
    let init = home.init_telemetry();
    assert!(init.status.success());
    let spawn = |home: &Home| {
        home.cmd()
            .args(["hook", "session-init"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut a = spawn(&home);
    let mut b = spawn(&home);
    for child in [&mut a, &mut b] {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(br#"{"session_id":"s","source":"startup"}"#)
            .unwrap();
    }

    let _ = a.wait();
    let _ = b.wait();
    let live = home.wait_for_live_collector();

    let log =
        fs::read_to_string(home.0.join(".config/playbook/data/collector.log")).unwrap_or_default();
    assert!(live.is_some(), "no collector came up");
    assert_eq!(log.matches("listening on").count(), 1, "{log}");
}

impl Home {
    /// The hook's stdout for a session that starts with `env` added.
    fn session_start_stdout(&self, env: &[(&str, &str)]) -> String {
        let mut cmd = self.cmd();
        cmd.args(["hook", "session-init"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(br#"{"session_id":"s1","source":"startup"}"#)
            .unwrap();
        String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).into_owned()
    }
}

const MISSING: &str = "without the OpenTelemetry keys";

#[test]
fn a_session_that_started_without_the_keys_gets_one_warning_line() {
    let home = Home::new("warn");
    assert!(home.init_telemetry().status.success());

    let out = home.session_start_stdout(&[]);

    assert_eq!(out.matches(MISSING).count(), 1, "{out}");
}

#[test]
fn a_session_that_has_the_keys_or_no_setup_hears_nothing() {
    let home = Home::new("quiet");
    let none = home.session_start_stdout(&[]);
    assert!(!none.contains(MISSING), "{none}");

    assert!(home.init_telemetry().status.success());
    let port = home.state().unwrap()["port"].as_u64().unwrap();
    let endpoint = format!("http://127.0.0.1:{port}");
    let fine = home.session_start_stdout(&[("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint.as_str())]);
    assert!(!fine.contains(MISSING), "{fine}");

    let headless = home.session_start_stdout(&[("PLAYBOOK_HEADLESS", "1")]);
    assert!(!headless.contains(MISSING), "{headless}");
}
