// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Binary tests for `playbook code lsp`. Each test gets a scratch git repo with
//! a fake `origin`, a scratch `HOME` and a scratch `PATH` that holds only git.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    repo: PathBuf,
    home: PathBuf,
    bin: PathBuf,
}

fn which_git() -> PathBuf {
    for dir in ["/usr/bin", "/opt/homebrew/bin", "/usr/local/bin", "/bin"] {
        let candidate = Path::new(dir).join("git");
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from("git")
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!(
            "playbook-code-lsp-{tag}-{}-{n}",
            playbook::testing::run_id()
        ));
        let repo = base.join("repo");
        let home = base.join("home");
        let bin = base.join("bin");
        for dir in [&repo, &home, &bin] {
            fs::create_dir_all(dir).expect("scratch dir");
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(which_git(), bin.join("git")).expect("git link");
        let fx = Self {
            repo: repo.canonicalize().expect("repo"),
            home,
            bin,
        };
        fx.git(&["init", "--quiet"]);
        fx.git(&[
            "remote",
            "add",
            "origin",
            "https://github.com/test-owner/test-repo.git",
        ]);
        fx
    }

    fn git(&self, args: &[&str]) {
        let status = Command::new(which_git())
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env_clear()
            .env("HOME", &self.home)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?}");
    }

    fn playbook(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(args)
            .current_dir(&self.repo)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", &self.bin)
            .output()
            .expect("playbook runs")
    }
}

fn json_of(out: &Output) -> serde_json::Value {
    let text = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("not one JSON document ({e}): {text}"))
}

#[test]
fn status_with_only_git_on_path_lists_four_servers_not_found() {
    // Arrange
    let fx = Fixture::new("status-empty");

    // Act
    let out = fx.playbook(&["code", "lsp", "status", "--json"]);

    // Assert
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc = json_of(&out);
    assert_eq!(doc["command"], "lsp status");
    let servers = doc["results"]["servers"].as_array().unwrap();
    assert_eq!(servers.len(), 4);
    assert!(servers.iter().all(|s| s["found"] == false), "{servers:?}");
    assert_eq!(doc["results"]["trust"]["workspace"], false);
}

#[test]
fn list_prints_every_server_with_its_license() {
    // Arrange
    let fx = Fixture::new("list");

    // Act
    let out = fx.playbook(&["code", "lsp", "list"]);

    // Assert
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for id in [
        "typescript-language-server",
        "rust-analyzer",
        "pyright",
        "gopls",
    ] {
        assert!(text.contains(id), "{text}");
    }
    assert!(text.contains("BSD-3-Clause"), "{text}");
}

#[cfg(unix)]
#[test]
fn a_server_set_through_config_shows_source_config_and_the_scripts_version() {
    // Arrange
    use std::os::unix::fs::PermissionsExt;
    let fx = Fixture::new("status-config");
    let script = fx.home.join("fake-ra");
    fs::write(&script, "#!/bin/sh\necho rust-analyzer 1.2.3\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let set = fx.playbook(&[
        "config",
        "set",
        "--global",
        "code.lsp.rust-analyzer.path",
        script.to_str().unwrap(),
    ]);
    assert!(
        set.status.success(),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );

    // Act
    let out = fx.playbook(&["code", "lsp", "status", "--json"]);

    // Assert
    assert!(out.status.success());
    let doc = json_of(&out);
    let ra = doc["results"]["servers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "rust-analyzer")
        .unwrap();
    assert_eq!(ra["source"], "config");
    assert_eq!(ra["version"], "rust-analyzer 1.2.3");
}

#[test]
fn status_shows_trust_and_its_tier_once_the_repo_is_trusted() {
    // Arrange
    let fx = Fixture::new("status-trust");
    let set = fx.playbook(&["config", "set", "code.lsp.trustWorkspace", "true"]);
    assert!(
        set.status.success(),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );

    // Act
    let out = fx.playbook(&["code", "lsp", "status", "--json"]);

    // Assert
    let doc = json_of(&out);
    assert_eq!(doc["results"]["trust"]["workspace"], true);
    assert_eq!(doc["results"]["trust"]["tier"], "repo");
}

#[cfg(unix)]
mod semantic {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    const SERVER: &str = "typescript-language-server";

    fn frame(body: &str) -> String {
        format!("Content-Length: {}\r\n\r\n{body}", body.len())
    }

    fn initialize_reply(capabilities: &str) -> String {
        frame(&format!(
            r#"{{"jsonrpc":"2.0","id":1,"result":{{"capabilities":{capabilities}}}}}"#
        ))
    }

    fn shutdown_reply() -> String {
        frame(r#"{"jsonrpc":"2.0","id":3,"result":null}"#)
    }

    impl Fixture {
        fn source(&self) -> PathBuf {
            let path = self.repo.join("a.ts");
            fs::write(&path, "export const answer = 42;\n").unwrap();
            path
        }

        /// Installs a scripted server and points the config at it. `tail`
        /// runs after the canned frames are written.
        fn scripted_server(&self, frames: &str, tail: &str, pidfile: Option<&Path>) {
            let data = self.home.join("frames.bin");
            fs::write(&data, frames).unwrap();
            let script = self.home.join("fake-ts-server");
            let record =
                pidfile.map_or(String::new(), |p| format!("echo $$ > '{}'\n", p.display()));
            fs::write(
                &script,
                format!(
                    "#!/bin/sh\ncase \"$1\" in --version) echo ts-fake 1.0; exit 0;; esac\n{record}/bin/cat '{}'\n{tail}\n",
                    data.display()
                ),
            )
            .unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
            self.config(
                &format!("code.lsp.{SERVER}.path"),
                script.to_str().unwrap(),
                true,
            );
        }

        fn config(&self, key: &str, value: &str, global: bool) {
            let mut args = vec!["config", "set"];
            if global {
                args.push("--global");
            }
            args.extend([key, value]);
            let out = self.playbook(&args);
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        fn trust(&self) {
            self.config("code.lsp.trustWorkspace", "true", false);
        }
    }

    fn lsp_of(out: &Output) -> serde_json::Value {
        json_of(out)["lsp"].clone()
    }

    #[test]
    fn definition_prints_the_location_a_trusted_scripted_server_returns() {
        // Arrange
        let fx = Fixture::new("def-ok");
        let file = fx.source();
        let uri = format!("file://{}", file.display());
        let location = frame(&format!(
            r#"{{"jsonrpc":"2.0","id":2,"result":[{{"uri":"{uri}","range":{{"start":{{"line":0,"character":13}},"end":{{"line":0,"character":19}}}}}}]}}"#
        ));
        let frames =
            initialize_reply(r#"{"definitionProvider":true}"#) + &location + &shutdown_reply();
        fx.scripted_server(&frames, "/bin/cat >/dev/null", None);
        fx.trust();

        // Act
        let out = fx.playbook(&["code", "definition", "a.ts:1:14", "--json"]);

        // Assert
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let doc = json_of(&out);
        assert_eq!(doc["command"], "definition");
        assert_eq!(doc["lsp"]["state"], "ok", "{doc}");
        assert_eq!(doc["lsp"]["source"], "lsp");
        assert_eq!(doc["results"][0]["path"], "a.ts");
        assert_eq!(doc["results"][0]["line"], 1);
        assert_eq!(doc["results"][0]["column"], 14);
    }

    #[test]
    fn untrusted_prints_json_with_the_state_and_names_the_config_command_on_stderr() {
        // Arrange
        let fx = Fixture::new("def-untrusted");
        fx.source();

        // Act
        let out = fx.playbook(&["code", "definition", "a.ts:1:14", "--json"]);
        let text = fx.playbook(&["code", "definition", "a.ts:1:14"]);

        // Assert
        assert!(out.status.success());
        let lsp = lsp_of(&out);
        assert_eq!(lsp["state"], "untrusted");
        assert_eq!(lsp["source"], "tree_sitter_fallback");
        let stderr = String::from_utf8_lossy(&text.stderr);
        assert!(
            stderr.contains("config set code.lsp.trustWorkspace true"),
            "{stderr}"
        );
    }

    /// Runs one call so the lock file exists, then holds it.
    fn hold_workspace_lock(fx: &Fixture) -> fs::File {
        let first = fx.playbook(&["code", "definition", "a.ts:1:14", "--json"]);
        assert!(first.status.success());
        let lock = find_file(&fx.home, "lsp.lock").expect("the first call creates the lock file");
        let file = fs::OpenOptions::new().write(true).open(lock).unwrap();
        file.try_lock().unwrap();
        file
    }

    fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
        for entry in fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = find_file(&path, name) {
                    return Some(found);
                }
            } else if path.file_name().is_some_and(|n| n == name) {
                return Some(path);
            }
        }
        None
    }

    #[test]
    fn every_failure_state_falls_back_with_exit_0_json_and_its_own_lsp_state() {
        let commands = [
            ("definition", "definitionProvider"),
            ("hover", "hoverProvider"),
            ("refs", "referencesProvider"),
        ];
        let rows = [
            "untrusted",
            "busy",
            "server_missing",
            "capability_missing",
            "timeout",
            "server_crashed",
            "error",
        ];
        for (command, capability) in commands {
            for state in rows {
                // Arrange
                let fx = Fixture::new(&format!("matrix-{command}-{state}"));
                fx.source();
                let mut extra: Vec<&str> = Vec::new();
                let mut _held = None;
                if state != "untrusted" {
                    fx.trust();
                }
                let capable = initialize_reply(&format!(r#"{{"{capability}":true}}"#));
                match state {
                    "busy" => _held = Some(hold_workspace_lock(&fx)),
                    "capability_missing" => {
                        fx.scripted_server(&initialize_reply("{}"), "/bin/cat >/dev/null", None);
                    }
                    "timeout" => {
                        fx.scripted_server(&capable, "exec /bin/sleep 300", None);
                        extra = vec!["--timeout", "300"];
                    }
                    "server_crashed" => fx.scripted_server(&capable, "exit 0", None),
                    "error" => {
                        let failure = frame(
                            r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32603,"message":"boom"}}"#,
                        );
                        let frames = capable + &failure + &shutdown_reply();
                        fx.scripted_server(&frames, "/bin/cat >/dev/null", None);
                    }
                    _ => {}
                }
                let mut args = vec!["code", command, "a.ts:1:14", "--json"];
                args.extend(extra);

                // Act
                let out = fx.playbook(&args);

                // Assert
                let case = format!("{command} {state}");
                assert_eq!(
                    out.status.code(),
                    Some(0),
                    "{case}: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                let lsp = lsp_of(&out);
                assert_eq!(lsp["state"], state, "{case}");
                assert_eq!(lsp["source"], "tree_sitter_fallback", "{case}");
            }
        }
    }

    #[test]
    fn diagnostics_with_no_usable_server_exit_0_with_an_empty_list_and_the_state() {
        for state in ["untrusted", "server_missing", "server_crashed"] {
            // Arrange
            let fx = Fixture::new(&format!("diag-{state}"));
            fx.source();
            if state != "untrusted" {
                fx.trust();
            }
            if state == "server_crashed" {
                fx.scripted_server(&initialize_reply("{}"), "exit 0", None);
            }

            // Act
            let out = fx.playbook(&["code", "diagnostics", "a.ts", "--json"]);

            // Assert
            assert_eq!(out.status.code(), Some(0), "{state}");
            let doc = json_of(&out);
            assert_eq!(doc["command"], "diagnostics");
            assert_eq!(doc["results"], serde_json::json!([]), "{state}");
            assert_eq!(doc["lsp"]["state"], state);
        }
    }

    #[test]
    fn refs_at_a_position_lists_the_references_a_scripted_server_resolves() {
        // Arrange
        let fx = Fixture::new("refs-ok");
        let file = fx.source();
        let uri = format!("file://{}", file.display());
        let at = |start: u32, end: u32| {
            format!(
                r#"{{"uri":"{uri}","range":{{"start":{{"line":0,"character":{start}}},"end":{{"line":0,"character":{end}}}}}}}"#
            )
        };
        let answer = frame(&format!(
            r#"{{"jsonrpc":"2.0","id":2,"result":[{},{}]}}"#,
            at(13, 19),
            at(0, 6)
        ));
        let frames =
            initialize_reply(r#"{"referencesProvider":true}"#) + &answer + &shutdown_reply();
        fx.scripted_server(&frames, "/bin/cat >/dev/null", None);
        fx.trust();

        // Act
        let out = fx.playbook(&["code", "refs", "a.ts:1:14", "--json"]);

        // Assert
        let doc = json_of(&out);
        assert_eq!(doc["lsp"]["state"], "ok");
        let results = doc["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert!(
            results.iter().all(|r| r["confidence"] == "resolved"),
            "{results:?}"
        );
    }

    #[test]
    fn hover_and_diagnostics_print_json_from_a_scripted_server() {
        // Arrange
        let fx = Fixture::new("hover-diag-ok");
        let file = fx.source();
        let hover = frame(
            r#"{"jsonrpc":"2.0","id":2,"result":{"contents":{"kind":"markdown","value":"const answer: 42"}}}"#,
        );
        fx.scripted_server(
            &(initialize_reply(r#"{"hoverProvider":true}"#) + &hover + &shutdown_reply()),
            "/bin/cat >/dev/null",
            None,
        );
        fx.trust();

        // Act
        let hovered = fx.playbook(&["code", "hover", "a.ts:1:14", "--json"]);

        // Assert
        let doc = json_of(&hovered);
        assert_eq!(doc["lsp"]["state"], "ok");
        assert_eq!(doc["results"]["text"], "const answer: 42");

        // Arrange
        let uri = format!("file://{}", file.display());
        let published = frame(&format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{{"uri":"{uri}","diagnostics":[{{"range":{{"start":{{"line":0,"character":13}},"end":{{"line":0,"character":19}}}},"severity":1,"message":"bad","source":"ts","code":2304}}]}}}}"#
        ));
        let closing = frame(r#"{"jsonrpc":"2.0","id":2,"result":null}"#);
        fx.scripted_server(
            &(initialize_reply("{}") + &published + &closing),
            "/bin/cat >/dev/null",
            None,
        );

        // Act
        let checked = fx.playbook(&["code", "diagnostics", "a.ts", "--json"]);

        // Assert
        let doc = json_of(&checked);
        assert_eq!(doc["lsp"]["state"], "ok", "{doc}");
        assert_eq!(doc["results"][0]["severity"], "error");
        assert_eq!(doc["results"][0]["message"], "bad");
        assert_eq!(doc["results"][0]["line"], 1);
    }

    #[test]
    fn hover_falls_back_the_same_way_when_untrusted() {
        // Arrange
        let fx = Fixture::new("hover-untrusted");
        fx.source();

        // Act
        let out = fx.playbook(&["code", "hover", "a.ts:1:14", "--json"]);

        // Assert
        assert!(out.status.success());
        assert_eq!(json_of(&out)["command"], "hover");
        assert_eq!(lsp_of(&out)["state"], "untrusted");
    }

    #[test]
    fn a_position_outside_the_repo_is_an_argument_error() {
        // Arrange
        let fx = Fixture::new("def-outside");
        fx.source();

        // Act
        let out = fx.playbook(&["code", "definition", "../elsewhere.ts:1:1"]);

        // Assert
        assert_eq!(out.status.code(), Some(1));
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("outside repo") || err.contains("no such file"),
            "{err}"
        );
    }

    #[test]
    fn sigterm_stops_the_server_and_exits_143() {
        // Arrange
        let fx = Fixture::new("def-sigterm");
        fx.source();
        let pidfile = fx.home.join("server.pid");
        let frames = initialize_reply(r#"{"definitionProvider":true}"#);
        fx.scripted_server(&frames, "exec /bin/sleep 300", Some(&pidfile));
        fx.trust();
        let mut child = Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(["code", "definition", "a.ts:1:14", "--json"])
            .current_dir(&fx.repo)
            .env_clear()
            .env("HOME", &fx.home)
            .env("PATH", &fx.bin)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let server_pid = wait_for_pid(&pidfile);

        // Act
        Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap();
        let status = child.wait().unwrap();

        // Assert
        assert_eq!(status.code(), Some(143));
        assert!(process_gone(server_pid), "the server outlived the CLI");
    }

    fn wait_for_pid(file: &Path) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if let Some(pid) = fs::read_to_string(file)
                .ok()
                .and_then(|t| t.trim().parse().ok())
            {
                return pid;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the scripted server never started");
    }

    fn process_gone(pid: i32) -> bool {
        let pid = rustix::process::Pid::from_raw(pid).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if rustix::process::test_kill_process(pid).is_err() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }
}
