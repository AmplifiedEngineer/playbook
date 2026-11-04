// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! One server for one call: the trust gate, the workspace lock, server
//! discovery, start and `initialize`. A call that cannot get a server returns
//! why, and the caller falls back to the index.

use super::client::{Client, ClientConfig, ClientError, ConfigurationReply};
use super::discover::{self, BadPath};
use super::pyenv::{self, Interpreter};
use super::registry::{self, ServerSpec};
use super::uri;
use crate::host::{self, LspSettings};
use serde_json::{json, Value};
use std::ffi::OsString;
use std::fmt;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

/// Everything a session reads from its surroundings.
pub struct Options<'a> {
    pub settings: &'a LspSettings,
    pub path_dirs: &'a [PathBuf],
    /// Where the per-workspace lock file lives.
    pub lock_dir: &'a Path,
    pub probe_timeout: Duration,
    /// Replaces the server's default request timeout.
    pub timeout: Option<Duration>,
    /// Set by a signal handler; every wait then ends with `Interrupted`.
    pub interrupt: Option<Arc<AtomicBool>>,
    /// `--python`: the interpreter Pyright should use.
    pub python_flag: Option<&'a Path>,
    /// The `VIRTUAL_ENV` variable.
    pub virtual_env: Option<&'a Path>,
}

/// How to start one server process.
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub config: ClientConfig,
}

/// Starts a server. Production spawns a process; unit tests hand back a client
/// wired to a scripted fake.
pub trait Launcher {
    fn start(&self, launch: &Launch) -> Result<Client, ClientError>;
}

pub struct ProcessLauncher;

impl Launcher for ProcessLauncher {
    fn start(&self, launch: &Launch) -> Result<Client, ClientError> {
        Client::spawn(&launch.program, &launch.args, &launch.cwd, launch.config)
    }
}

/// Why no server answered. The caller turns each into a labelled fallback.
#[derive(Debug)]
pub enum Unavailable {
    Untrusted,
    Busy,
    /// No registered server handles this file extension.
    NoServerFor(String),
    /// No binary was found for the server `id`.
    NotInstalled(&'static str),
    BadConfiguredPath(BadPath),
    Unreadable(String),
    Failed(ClientError),
}

impl fmt::Display for Unavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Untrusted => write!(
                f,
                "this repo is not trusted to run language servers; after reviewing it, run \
                 `{} config set code.lsp.trustWorkspace true`",
                host::PROGRAM
            ),
            Self::Busy => write!(f, "another language server call is running in this repo"),
            Self::NoServerFor(extension) => {
                write!(f, "no language server handles .{extension} files")
            }
            Self::NotInstalled(id) => write!(
                f,
                "no {id} server found; install it (see `{}`) or set code.lsp.{id}.path",
                host::command("lsp status")
            ),
            Self::BadConfiguredPath(bad) => {
                write!(f, "the configured server path is unusable: {bad}")
            }
            Self::Unreadable(why) => write!(f, "{why}"),
            Self::Failed(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Unavailable {}

/// The advisory lock that keeps a second call from starting another server in
/// the same workspace. Dropping it unlocks explicitly.
pub struct WorkspaceLock(File);

impl WorkspaceLock {
    pub fn acquire(dir: &Path) -> Result<Self, Unavailable> {
        host::ensure_private_dir(dir).map_err(|e| {
            Unavailable::Unreadable(format!("could not create {}: {e}", dir.display()))
        })?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("lsp.lock"))
            .map_err(|e| Unavailable::Unreadable(format!("could not open the server lock: {e}")))?;
        match file.try_lock() {
            Ok(()) => Ok(Self(file)),
            Err(TryLockError::WouldBlock) => Err(Unavailable::Busy),
            Err(TryLockError::Error(e)) => Err(Unavailable::Unreadable(format!(
                "could not take the server lock: {e}"
            ))),
        }
    }
}

impl Drop for WorkspaceLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// A document the server has been told about.
pub struct Opened {
    pub uri: String,
    /// The text the server saw, so positions are computed against it.
    pub text: String,
}

pub struct Session {
    client: Client,
    spec: &'static ServerSpec,
    root: PathBuf,
    python: Option<Interpreter>,
    // Held until the session ends so the lock outlives the server.
    _lock: WorkspaceLock,
}

impl Session {
    /// Gets a ready server for `file` in the repo at `root`. The trust check
    /// comes first and nothing is started or locked before it passes.
    pub fn for_file(
        root: &Path,
        file: &Path,
        options: &Options<'_>,
        launcher: &dyn Launcher,
    ) -> Result<Self, Unavailable> {
        if !options.settings.trust_workspace {
            return Err(Unavailable::Untrusted);
        }
        let extension = file.extension().and_then(|e| e.to_str()).unwrap_or("");
        let spec = registry::for_extension(extension)
            .ok_or_else(|| Unavailable::NoServerFor(extension.to_string()))?;
        let lock = WorkspaceLock::acquire(options.lock_dir)?;
        let found = discover::find(
            spec,
            options.settings,
            options.path_dirs,
            options.probe_timeout,
        )
        .map_err(Unavailable::BadConfiguredPath)?
        .ok_or(Unavailable::NotInstalled(spec.id))?;
        let launch = Launch {
            program: found.path,
            args: spec.args.iter().map(OsString::from).collect(),
            cwd: root.to_path_buf(),
            config: ClientConfig {
                request_timeout: options.timeout.unwrap_or(spec.request_timeout),
                ..ClientConfig::default()
            },
        };
        let mut client = launcher.start(&launch).map_err(Unavailable::Failed)?;
        if let Some(flag) = &options.interrupt {
            client.set_interrupt(Arc::clone(flag));
        }
        let python = (spec.id == "pyright").then(|| {
            pyenv::discover(&pyenv::Inputs {
                root,
                flag: options.python_flag,
                configured: options.settings.python_interpreter.as_deref(),
                virtual_env: options.virtual_env,
            })
        });
        if let Some(Interpreter::Found { path, .. }) = &python {
            client.set_configuration_reply(python_configuration(path));
        }
        client
            .initialize(&uri::from_path(root), initialization_options(spec, root))
            .map_err(Unavailable::Failed)?;
        Ok(Self {
            client,
            spec,
            root: root.to_path_buf(),
            python,
            _lock: lock,
        })
    }

    pub fn client(&mut self) -> &mut Client {
        &mut self.client
    }

    pub fn spec(&self) -> &'static ServerSpec {
        self.spec
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The Python environment chosen for Pyright; `None` for other servers.
    pub fn python(&self) -> Option<&Interpreter> {
        self.python.as_ref()
    }

    /// Sends `didOpen` with the file's current text from disk.
    pub fn open(&mut self, file: &Path) -> Result<Opened, Unavailable> {
        let text = std::fs::read_to_string(file).map_err(|e| {
            Unavailable::Unreadable(format!("could not read {}: {e}", file.display()))
        })?;
        let extension = file.extension().and_then(|e| e.to_str()).unwrap_or("");
        let uri = uri::from_path(file);
        self.client
            .notify(
                "textDocument/didOpen",
                json!({"textDocument": {
                    "uri": uri,
                    "languageId": registry::language_id(extension),
                    "version": 1,
                    "text": text,
                }}),
            )
            .map_err(Unavailable::Failed)?;
        Ok(Opened { uri, text })
    }

    /// Shuts the server down and releases the lock.
    pub fn finish(self) {
        self.client.shutdown();
    }
}

/// Answers Pyright's `workspace/configuration` requests with the interpreter.
fn python_configuration(interpreter: &Path) -> ConfigurationReply {
    let python = interpreter.display().to_string();
    Box::new(move |params| {
        let sections = params["items"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|item| match item["section"].as_str() {
                Some("python") => json!({"pythonPath": python}),
                Some("python.analysis") => json!({}),
                _ => Value::Null,
            });
        Value::Array(sections.collect())
    })
}

/// Options sent at `initialize`. The TypeScript server uses the repo's own
/// TypeScript when it has one.
fn initialization_options(spec: &ServerSpec, root: &Path) -> Value {
    if spec.id == "typescript-language-server" {
        let tsserver = root.join("node_modules/typescript/lib/tsserver.js");
        if tsserver.is_file() {
            return json!({"tsserver": {"path": tsserver.display().to_string()}});
        }
    }
    json!({})
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::lsp::fake::{Fake, Step};
    use crate::lsp::testkit::{initialize_script, options, scratch, setup, trusted, FakeLauncher};
    use crate::lsp::transport::pipe_pair;
    use std::sync::atomic::Ordering;

    #[test]
    fn an_untrusted_workspace_never_calls_the_launcher() {
        // Arrange
        let s = setup("sess-untrusted");
        let launcher = FakeLauncher::new(None);
        let settings = LspSettings::default();
        let dirs = [s.bin.clone()];

        // Act
        let got = Session::for_file(&s.root, &s.file, &options(&s, &settings, &dirs), &launcher);

        // Assert
        assert!(matches!(got, Err(Unavailable::Untrusted)));
        assert_eq!(launcher.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn no_installed_server_names_the_install_and_the_config_key() {
        // Arrange
        let s = setup("sess-missing");
        let launcher = FakeLauncher::new(None);
        let settings = trusted();
        let dirs = [scratch("sess-empty-bin")];

        // Act
        let got = Session::for_file(&s.root, &s.file, &options(&s, &settings, &dirs), &launcher);

        // Assert
        let err = got.err().unwrap();
        assert!(matches!(err, Unavailable::NotInstalled(_)));
        let text = err.to_string();
        assert!(
            text.contains("install") && text.contains("code.lsp.typescript-language-server.path"),
            "{text}"
        );
        assert_eq!(launcher.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_file_no_server_handles_is_reported_before_anything_is_locked() {
        // Arrange
        let s = setup("sess-nolang");
        let launcher = FakeLauncher::new(None);
        let settings = trusted();
        let dirs = [s.bin.clone()];

        // Act
        let got = Session::for_file(
            &s.root,
            &s.root.join("notes.txt"),
            &options(&s, &settings, &dirs),
            &launcher,
        );

        // Assert
        assert!(matches!(got, Err(Unavailable::NoServerFor(ext)) if ext == "txt"));
    }

    #[test]
    fn a_held_lock_makes_a_second_call_busy_and_dropping_it_frees_the_workspace() {
        // Arrange
        let s = setup("sess-busy");
        let settings = trusted();
        let dirs = [s.bin.clone()];
        let guard = WorkspaceLock::acquire(&s.locks).unwrap();
        let blocked = FakeLauncher::new(None);

        // Act
        let while_held =
            Session::for_file(&s.root, &s.file, &options(&s, &settings, &dirs), &blocked);
        drop(guard);
        let after = WorkspaceLock::acquire(&s.locks);

        // Assert
        assert!(matches!(while_held, Err(Unavailable::Busy)));
        assert_eq!(blocked.calls.load(Ordering::SeqCst), 0);
        assert!(after.is_ok());
    }

    #[test]
    fn the_workspace_typescript_is_passed_to_initialize_when_present_and_omitted_when_absent() {
        for (present, tag) in [(true, "sess-ts-yes"), (false, "sess-ts-no")] {
            // Arrange
            let s = setup(tag);
            let tsserver = s.root.join("node_modules/typescript/lib/tsserver.js");
            if present {
                std::fs::create_dir_all(tsserver.parent().unwrap()).unwrap();
                std::fs::write(&tsserver, "").unwrap();
            }
            let (client_side, server) = pipe_pair().unwrap();
            let fake = Fake::spawn(server, initialize_script(json!({})));
            let launcher = FakeLauncher::new(Some(client_side));
            let settings = trusted();
            let dirs = [s.bin.clone()];

            // Act
            let session =
                Session::for_file(&s.root, &s.file, &options(&s, &settings, &dirs), &launcher)
                    .unwrap();
            drop(session);
            let log = fake.finish();

            // Assert
            let init_options = &log[0]["params"]["initializationOptions"];
            if present {
                assert_eq!(
                    init_options["tsserver"]["path"],
                    tsserver.display().to_string()
                );
            } else {
                assert!(init_options.get("tsserver").is_none(), "{init_options}");
            }
        }
    }

    #[test]
    fn open_sends_the_current_disk_text_with_the_language_id() {
        // Arrange
        let s = setup("sess-open");
        let (client_side, server) = pipe_pair().unwrap();
        let mut script = initialize_script(json!({}));
        script.push(Step::Expect("textDocument/didOpen"));
        let fake = Fake::spawn(server, script);
        let launcher = FakeLauncher::new(Some(client_side));
        let settings = trusted();
        let dirs = [s.bin.clone()];
        let mut session =
            Session::for_file(&s.root, &s.file, &options(&s, &settings, &dirs), &launcher).unwrap();

        // Act
        let opened = session.open(&s.file).unwrap();
        drop(session);
        let log = fake.finish();

        // Assert
        assert_eq!(opened.text, "export const a = 1;\n");
        let doc = &log.last().unwrap()["params"]["textDocument"];
        assert_eq!(doc["languageId"], "typescript");
        assert_eq!(doc["uri"], uri::from_path(&s.file));
        assert_eq!(doc["text"], "export const a = 1;\n");
    }

    #[test]
    fn pyright_gets_the_resolved_interpreter_through_workspace_configuration() {
        // Arrange
        let s = setup("sess-py");
        let python = s.root.join(".venv/bin/python");
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        std::fs::write(&python, "").unwrap();
        let file = s.root.join("m.py");
        std::fs::write(&file, "x = 1\n").unwrap();
        crate::lsp::testkit::install_server(&s.bin, "pyright-langserver");
        let (client_side, server) = pipe_pair().unwrap();
        let mut steps = initialize_script(json!({}));
        steps.push(Step::Request {
            id: 10,
            method: "workspace/configuration",
            params: json!({"items": [{"section": "python"}, {"section": "python.analysis"}, {"section": "other"}]}),
        });
        steps.push(Step::Reply {
            method: "ping",
            result: Value::Null,
        });
        // Keep the connection open until the client's answer to the request arrives.
        steps.push(Step::Read(1));
        let fake = Fake::spawn(server, steps);
        let launcher = FakeLauncher::new(Some(client_side));
        let settings = trusted();
        let dirs = [s.bin.clone()];

        // Act
        let mut session =
            Session::for_file(&s.root, &file, &options(&s, &settings, &dirs), &launcher).unwrap();
        session.client().request("ping", json!({})).unwrap();
        let found = session.python().cloned();
        drop(session);
        let log = fake.finish();

        // Assert
        assert!(matches!(found, Some(Interpreter::Found { .. })));
        let answer = log
            .iter()
            .find(|m| m["id"] == 10 && m.get("method").is_none())
            .unwrap();
        assert_eq!(
            answer["result"],
            json!([{"pythonPath": python.display().to_string()}, {}, null])
        );
    }

    #[test]
    fn other_servers_have_no_python_environment() {
        // Arrange
        let s = setup("sess-nopy");
        let (client_side, server) = pipe_pair().unwrap();
        let fake = Fake::spawn(server, initialize_script(json!({})));
        let launcher = FakeLauncher::new(Some(client_side));
        let settings = trusted();
        let dirs = [s.bin.clone()];

        // Act
        let session =
            Session::for_file(&s.root, &s.file, &options(&s, &settings, &dirs), &launcher).unwrap();
        let python = session.python().cloned();
        drop(session);
        fake.finish();

        // Assert
        assert_eq!(python, None);
    }
}
