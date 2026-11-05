// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The language server arm of the code benchmark: a small stdio JSON-RPC
//! client, workspace setup for the three servers, and the per-task queries.
//! It is its own client on purpose, so the numbers do not depend on the code
//! under test. Bytes stand in for tokens, as in the other arms.

#![allow(
    dead_code,
    unreachable_pub,
    reason = "each test crate uses a different part of this shared module"
)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Encodes one JSON-RPC message with its `Content-Length` header.
pub fn frame(message: &Value) -> Vec<u8> {
    let body = message.to_string();
    format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
}

/// Reads one framed message; `None` at end of input or on a bad header.
pub fn read_frame<R: BufRead>(input: &mut R) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; length?];
    input.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

/// One language server process spoken to over stdio.
pub struct Client {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    next_id: u64,
}

impl Client {
    pub fn spawn(program: &Path, args: &[&str], cwd: &Path) -> std::io::Result<Self> {
        let mut child = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().ok_or(std::io::ErrorKind::BrokenPipe)?;
        let stdout = child.stdout.take().ok_or(std::io::ErrorKind::BrokenPipe)?;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Some(message) = read_frame(&mut reader) {
                if tx.send(message).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            rx,
            next_id: 1,
        })
    }

    fn send(&mut self, message: &Value) {
        // A dead server shows up as a timeout on the next request.
        let _ = self.stdin.write_all(&frame(message));
        let _ = self.stdin.flush();
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Sends a request and waits for its answer. Server requests are answered
    /// with an empty result so the server never blocks on the client.
    pub fn request(&mut self, method: &str, params: Value, wait: Duration) -> Option<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.checked_duration_since(Instant::now())?;
            let message = match self.rx.recv_timeout(left) {
                Ok(m) => m,
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return None,
            };
            if message.get("method").is_some() && message.get("id").is_some() {
                let result = if message["method"] == "workspace/configuration" {
                    let items = message["params"]["items"].as_array().map_or(0, Vec::len);
                    json!(vec![Value::Null; items])
                } else {
                    Value::Null
                };
                self.send(&json!({"jsonrpc": "2.0", "id": message["id"], "result": result}));
            } else if message["id"] == json!(id) {
                return Some(message["result"].clone());
            }
        }
    }

    pub fn initialize(&mut self, root: &Path, options: Value) -> bool {
        let uri = file_uri(root);
        let params = json!({
            "processId": std::process::id(),
            "rootUri": uri,
            "workspaceFolders": [{"uri": uri, "name": "fixture"}],
            "capabilities": {
                "workspace": {"configuration": true, "symbol": {}},
                "textDocument": {
                    "definition": {},
                    "references": {},
                    "synchronization": {},
                },
            },
            "initializationOptions": options,
        });
        let ok = self
            .request("initialize", params, Duration::from_secs(120))
            .is_some();
        self.notify("initialized", json!({}));
        ok
    }

    pub fn did_open(&mut self, path: &Path, language: &str) {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": file_uri(path), "languageId": language, "version": 1, "text": text,
            }}),
        );
    }

    /// `shutdown`, `exit`, then a kill if the server lingers.
    pub fn stop(mut self) {
        let _ = self.request("shutdown", Value::Null, Duration::from_secs(5));
        self.notify("exit", Value::Null);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn file_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

/// One place a server reported, with a 1-based line like the fixture truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub path: String,
    pub line: usize,
}

/// Turns `Location`, `Location[]`, `LocationLink[]` or `SymbolInformation[]`
/// into places relative to `root`.
pub fn places(answer: &Value, root: &Path) -> Vec<Place> {
    let items: Vec<&Value> = match answer {
        Value::Array(list) => list.iter().collect(),
        Value::Object(_) => vec![answer],
        _ => vec![],
    };
    let prefix = format!("{}/", file_uri(root));
    items
        .into_iter()
        .filter_map(|item| {
            let loc = item.get("location").unwrap_or(item);
            let uri = loc["uri"].as_str().or_else(|| loc["targetUri"].as_str())?;
            let range = loc
                .get("range")
                .or_else(|| loc.get("targetSelectionRange"))?;
            Some(Place {
                path: uri.strip_prefix(&prefix)?.to_string(),
                line: range["start"]["line"].as_u64()? as usize + 1,
            })
        })
        .collect()
}

/// What the agent would read: one `path:line` per place.
pub fn render(places: &[Place]) -> String {
    places
        .iter()
        .map(|p| format!("{}:{}\n", p.path, p.line))
        .collect()
}

/// The language a fixture query name belongs to (`rs_fn_3_0` gives `rs`).
pub fn lang_of(query: &str) -> Option<&str> {
    let prefix = query.split("_fn_").next()?;
    ["rs", "ts", "js", "py"]
        .into_iter()
        .find(|l| *l == prefix && query.contains("_fn_"))
}

/// The server for a language: program, arguments and the extensions it owns.
pub struct Spec {
    pub name: &'static str,
    pub program: PathBuf,
    pub args: Vec<&'static str>,
    pub exts: Vec<&'static str>,
    pub options: Value,
}

fn on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(program))
        .find(|p| p.is_file())
}

/// The `tsserver.js` beside the `tsc` on `PATH`; the language server needs a
/// `typescript` package and an npm global install may not carry one.
fn tsserver_path() -> Option<String> {
    if let Ok(path) = std::env::var("BENCH_CODE_TSSERVER") {
        return Some(path);
    }
    let tsc = on_path("tsc")?.canonicalize().ok()?;
    let lib = tsc.parent()?.parent()?.join("lib/tsserver.js");
    lib.is_file().then(|| lib.display().to_string())
}

/// Servers found on `PATH`; nothing is installed.
pub fn discover() -> Vec<Spec> {
    let mut found = Vec::new();
    if let Some(program) = on_path("rust-analyzer") {
        found.push(Spec {
            name: "rust-analyzer",
            program,
            args: vec![],
            exts: vec!["rs"],
            options: json!({
                "checkOnSave": false,
                "cargo": {"buildScripts": {"enable": false}},
                "procMacro": {"enable": false},
            }),
        });
    }
    if let Some(program) = on_path("typescript-language-server") {
        let options = tsserver_path().map_or(json!({}), |path| json!({"tsserver": {"path": path}}));
        found.push(Spec {
            name: "typescript-language-server",
            program,
            args: vec!["--stdio"],
            exts: vec!["ts", "js"],
            options,
        });
    }
    if let Some(program) = on_path("pyright-langserver") {
        found.push(Spec {
            name: "pyright",
            program,
            args: vec!["--stdio"],
            exts: vec!["py"],
            options: json!({}),
        });
    }
    found
}

fn language_id(ext: &str) -> &'static str {
    match ext {
        "rs" => "rust",
        "ts" => "typescript",
        "js" => "javascript",
        _ => "python",
    }
}

/// Writes the project files each server needs to see the fixture as one
/// project. The sources are untouched.
pub fn prepare_workspace(root: &Path, files: usize) {
    let mut lib = String::new();
    for file in (0..files).filter(|f| f % 4 == 0) {
        lib.push_str(&format!(
            "#[path = \"m{}/f{file}.rs\"]\npub mod f{file};\n",
            file % 20
        ));
    }
    let _ = std::fs::write(root.join("src/lib.rs"), lib);
    let _ = std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    );
    let _ = std::fs::write(
        root.join("tsconfig.json"),
        r#"{"compilerOptions":{"allowJs":true,"noEmit":true,"target":"es2020","module":"esnext"},"include":["src/**/*.ts","src/**/*.js"]}"#,
    );
}

/// A started server that has answered a first real question.
pub struct Ready {
    pub client: Client,
    pub cold_s: f64,
    pub spec_name: &'static str,
}

/// Starts the server and waits until `workspace/symbol` finds `probe`, which
/// is when a call could first be answered. Returns `None` if it never does
/// within `limit`.
pub fn start(
    spec: &Spec,
    root: &Path,
    first_file: &str,
    probe: &str,
    limit: Duration,
) -> Option<Ready> {
    let began = Instant::now();
    let mut client = Client::spawn(&spec.program, &spec.args, root).ok()?;
    if !client.initialize(root, spec.options.clone()) {
        client.stop();
        return None;
    }
    let ext = Path::new(first_file).extension()?.to_str()?;
    client.did_open(&root.join(first_file), language_id(ext));
    while began.elapsed() < limit {
        let left = limit.saturating_sub(began.elapsed());
        let answer = client.request(
            "workspace/symbol",
            json!({"query": probe}),
            left.min(Duration::from_secs(30)),
        );
        if answer.is_some_and(|a| !named(&places_named(&a), probe).is_empty()) {
            return Some(Ready {
                client,
                cold_s: began.elapsed().as_secs_f64(),
                spec_name: spec.name,
            });
        }
        thread::sleep(Duration::from_millis(250));
    }
    client.stop();
    None
}

fn places_named(answer: &Value) -> Vec<(String, Value)> {
    answer.as_array().map_or(vec![], |list| {
        list.iter()
            .filter_map(|s| Some((s["name"].as_str()?.to_string(), s.clone())))
            .collect()
    })
}

fn named(symbols: &[(String, Value)], name: &str) -> Vec<Value> {
    symbols
        .iter()
        .filter(|(n, _)| n == name)
        .map(|(_, v)| v.clone())
        .collect()
}

/// The answer to one task from a warm server.
pub struct Answer {
    pub places: Vec<Place>,
    pub ms: f64,
    /// Whether the position `definition` round trip landed on the declaration.
    pub definition_ok: Option<bool>,
}

/// `workspace/symbol` for the declaration: exact name matches only.
pub fn declaration(r: &mut Ready, root: &Path, name: &str) -> Answer {
    let began = Instant::now();
    let answer = r
        .client
        .request(
            "workspace/symbol",
            json!({"query": name}),
            Duration::from_secs(30),
        )
        .unwrap_or(Value::Null);
    let exact = named(&places_named(&answer), name);
    Answer {
        places: places(&Value::Array(exact), root),
        ms: began.elapsed().as_secs_f64() * 1000.0,
        definition_ok: None,
    }
}

/// `workspace/symbol` to find the declaration, then `textDocument/references`
/// at the name, then `definition` at the first reference as a round trip.
pub fn references(r: &mut Ready, root: &Path, name: &str, limit: usize) -> Answer {
    let began = Instant::now();
    let decl = declaration(r, root, name).places;
    let Some(first) = decl.first() else {
        return Answer {
            places: vec![],
            ms: began.elapsed().as_secs_f64() * 1000.0,
            definition_ok: None,
        };
    };
    let path = root.join(&first.path);
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let column = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| t.lines().nth(first.line - 1).and_then(|l| l.find(name)))
        .unwrap_or(0);
    r.client.did_open(&path, language_id(ext));
    let at = |p: &Path, line: usize, col: usize| {
        json!({
            "textDocument": {"uri": file_uri(p)},
            "position": {"line": line - 1, "character": col},
            "context": {"includeDeclaration": false},
        })
    };
    let found = r
        .client
        .request(
            "textDocument/references",
            at(&path, first.line, column),
            Duration::from_secs(60),
        )
        .unwrap_or(Value::Null);
    let mut refs = places(&found, root);
    refs.truncate(limit);
    let ms = began.elapsed().as_secs_f64() * 1000.0;
    // The round trip is outside the timed call: the agent asks it separately.
    let definition_ok = refs.first().and_then(|site| {
        let site_path = root.join(&site.path);
        let ext = site_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let col = std::fs::read_to_string(&site_path)
            .ok()?
            .lines()
            .nth(site.line - 1)?
            .find(name)?;
        r.client.did_open(&site_path, language_id(ext));
        let def = r.client.request(
            "textDocument/definition",
            at(&site_path, site.line, col),
            Duration::from_secs(60),
        )?;
        Some(places(&def, root).contains(first))
    });
    Answer {
        places: refs,
        ms,
        definition_ok,
    }
}

/// Reduces `answered` places to the share of `expected` that appear.
pub fn recall(expected: &[(String, usize)], found: &[Place]) -> (usize, usize) {
    let have: HashMap<(&str, usize), ()> = found
        .iter()
        .map(|p| ((p.path.as_str(), p.line), ()))
        .collect();
    let hit = expected
        .iter()
        .filter(|(p, l)| have.contains_key(&(p.as_str(), *l)))
        .count();
    (hit, expected.len())
}

#[test]
fn a_frame_round_trips_through_the_reader() {
    // Arrange
    let message = json!({"jsonrpc": "2.0", "id": 7, "result": {"ok": true}});
    let bytes = frame(&message);

    // Act
    let back = read_frame(&mut BufReader::new(bytes.as_slice()));

    // Assert
    assert_eq!(back, Some(message));
}

#[test]
fn symbol_answers_become_one_based_places_under_the_root() {
    // Arrange
    let root = Path::new("/r");
    let answer = json!([{
        "name": "f", "kind": 12,
        "location": {"uri": "file:///r/src/a.rs", "range": {
            "start": {"line": 4, "character": 0}, "end": {"line": 6, "character": 1}}},
    }]);

    // Act
    let found = places(&answer, root);

    // Assert
    assert_eq!(
        found,
        vec![Place {
            path: "src/a.rs".into(),
            line: 5
        }]
    );
    assert_eq!(render(&found), "src/a.rs:5\n");
}

#[test]
fn a_query_name_maps_to_its_language() {
    assert_eq!(lang_of("ts_fn_12_0"), Some("ts"));
    assert_eq!(lang_of("parse_config"), None);
    assert_eq!(lang_of("zq phrase 3 9"), None);
}
