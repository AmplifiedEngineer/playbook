// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A JSON-RPC client for one language server over a framed transport.
//!
//! Every blocking read and write runs on its own thread, so a hung server
//! cannot hang the caller: each wait is a channel receive under a deadline.

use super::framing::{write_message, FrameError, Limits};
use super::process::{Exit, ServerProcess};
use super::transport::{BoxedWriter, Transport};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Content was modified while the server answered: the request may be retried.
const CONTENT_MODIFIED: i64 = -32801;
/// The server cancelled the request: it may be retried.
const SERVER_CANCELLED: i64 = -32802;
const METHOD_NOT_FOUND: i64 = -32601;
/// How often a wait looks at the interrupt flag.
const POLL_SLICE: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, Copy)]
pub struct ClientConfig {
    pub request_timeout: Duration,
    pub write_timeout: Duration,
    pub shutdown_grace: Duration,
    /// Largest accepted message body.
    pub max_response: usize,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(20),
            write_timeout: Duration::from_secs(5),
            shutdown_grace: Duration::from_secs(2),
            max_response: 8 * 1024 * 1024,
        }
    }
}

#[derive(Debug)]
pub enum ClientError {
    Spawn(io::Error),
    Write(io::Error),
    WriteTimeout,
    Timeout {
        method: String,
    },
    /// The server went away; `detail` says how.
    Crashed {
        detail: String,
    },
    Server {
        code: i64,
        message: String,
    },
    /// The server asked for the request to be repeated.
    Retryable {
        code: i64,
        message: String,
    },
    ResponseLimit {
        limit: usize,
    },
    Protocol(String),
    Interrupted,
}

impl ClientError {
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Retryable { .. })
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "could not start the language server: {e}"),
            Self::Write(e) => write!(f, "write to the language server failed: {e}"),
            Self::WriteTimeout => write!(f, "write to the language server timed out"),
            Self::Timeout { method } => write!(f, "{method} timed out"),
            Self::Crashed { detail } => write!(f, "the language server stopped: {detail}"),
            Self::Server { code, message } => write!(f, "server error {code}: {message}"),
            Self::Retryable { code, message } => {
                write!(f, "server error {code} (retryable): {message}")
            }
            Self::ResponseLimit { limit } => {
                write!(f, "response exceeds the response limit of {limit} bytes")
            }
            Self::Protocol(why) => write!(f, "protocol error: {why}"),
            Self::Interrupted => write!(f, "interrupted"),
        }
    }
}

impl std::error::Error for ClientError {}

/// The unit a position column is counted in, as negotiated at `initialize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16,
}

/// What the reader thread reports.
enum Inbound {
    Message(Value),
    End(Ended),
}

#[derive(Clone)]
enum Ended {
    Closed,
    ResponseLimit(usize),
    Bad(String),
}

struct WriteJob {
    frame: Vec<u8>,
    done: Sender<io::Result<()>>,
}

/// Answers `workspace/configuration`: gets the request items, returns one value per item.
pub type ConfigurationReply = Box<dyn Fn(&Value) -> Value + Send>;

pub struct Client {
    config: ClientConfig,
    inbound: Receiver<Inbound>,
    writer: Option<Sender<WriteJob>>,
    process: Option<ServerProcess>,
    next_id: i64,
    responses: HashMap<i64, Value>,
    notifications: Vec<Value>,
    ended: Option<Ended>,
    interrupt: Option<Arc<AtomicBool>>,
    configuration: ConfigurationReply,
    encoding: Encoding,
    capabilities: Value,
    quiescent: Option<bool>,
}

impl Client {
    /// Starts the server process and connects to its stdio.
    pub fn spawn(
        program: &Path,
        args: &[OsString],
        cwd: &Path,
        config: ClientConfig,
    ) -> Result<Self, ClientError> {
        let (process, transport) =
            ServerProcess::spawn(program, args, cwd).map_err(ClientError::Spawn)?;
        let mut client = Self::with_transport(transport, config);
        client.process = Some(process);
        Ok(client)
    }

    /// Connects to a server that is already running behind `transport`.
    pub fn with_transport(transport: Transport, config: ClientConfig) -> Self {
        let (reader, writer) = transport.into_parts();
        let (inbound_tx, inbound) = channel();
        let limits = Limits {
            max_body: config.max_response,
            ..Limits::default()
        };
        let mut reader = reader;
        std::thread::spawn(move || loop {
            let step = match super::framing::read_message(&mut reader, limits) {
                Ok(body) => match serde_json::from_slice(&body) {
                    Ok(value) => Inbound::Message(value),
                    Err(e) => Inbound::End(Ended::Bad(format!("invalid JSON: {e}"))),
                },
                Err(FrameError::Closed) => Inbound::End(Ended::Closed),
                Err(FrameError::BodyLimit { limit, .. }) => {
                    Inbound::End(Ended::ResponseLimit(limit))
                }
                Err(e) => Inbound::End(Ended::Bad(e.to_string())),
            };
            let last = matches!(step, Inbound::End(_));
            if inbound_tx.send(step).is_err() || last {
                return;
            }
        });
        let (job_tx, jobs) = channel::<WriteJob>();
        let mut sink: BoxedWriter = writer;
        std::thread::spawn(move || {
            for job in jobs {
                let _ = job.done.send(write_message(&mut sink, &job.frame));
            }
        });
        Self {
            config,
            inbound,
            writer: Some(job_tx),
            process: None,
            next_id: 1,
            responses: HashMap::new(),
            notifications: Vec::new(),
            ended: None,
            interrupt: None,
            configuration: Box::new(|params| {
                let items = params["items"].as_array().map_or(0, Vec::len);
                Value::Array(vec![Value::Null; items])
            }),
            encoding: Encoding::Utf16,
            capabilities: Value::Null,
            quiescent: None,
        }
    }

    /// Makes every wait return `Interrupted` once `flag` is set.
    pub fn set_interrupt(&mut self, flag: Arc<AtomicBool>) {
        self.interrupt = Some(flag);
    }

    pub fn set_configuration_reply(&mut self, reply: ConfigurationReply) {
        self.configuration = reply;
    }

    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    /// The `capabilities` object the server returned from `initialize`.
    pub fn capabilities(&self) -> &Value {
        &self.capabilities
    }

    /// Whether the server last said it has finished loading the project
    /// (`experimental/serverStatus`). `None` until it says anything.
    pub fn quiescent(&self) -> Option<bool> {
        self.quiescent
    }

    /// Notifications received so far, oldest first.
    pub fn take_notifications(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.notifications)
    }

    /// The oldest notification not yet taken, waiting up to `timeout` for one.
    /// `None` means none arrived in time.
    pub fn next_notification(&mut self, timeout: Duration) -> Result<Option<Value>, ClientError> {
        let deadline = Instant::now() + timeout;
        loop {
            if !self.notifications.is_empty() {
                return Ok(Some(self.notifications.remove(0)));
            }
            self.check_interrupt()?;
            if let Some(ended) = self.ended.clone() {
                return Err(self.ended_error(&ended));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            match self.inbound.recv_timeout((deadline - now).min(POLL_SLICE)) {
                Ok(Inbound::Message(message)) => self.route(message)?,
                Ok(Inbound::End(ended)) => self.ended = Some(ended),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => self.ended = Some(Ended::Closed),
            }
        }
    }

    /// The server's recent stderr, for crash reports.
    pub fn stderr_tail(&self) -> String {
        self.process
            .as_ref()
            .map(ServerProcess::stderr_tail)
            .unwrap_or_default()
    }

    /// Sends `initialize` offering UTF-8 then UTF-16 positions, then `initialized`.
    pub fn initialize(&mut self, root_uri: &str, options: Value) -> Result<(), ClientError> {
        let params = json!({
            "processId": std::process::id(),
            "clientInfo": {"name": "playbook"},
            "rootUri": root_uri,
            "capabilities": {
                "general": {"positionEncodings": ["utf-8", "utf-16"]},
                "workspace": {"configuration": true},
                "window": {"workDoneProgress": true},
                "experimental": {"serverStatusNotification": true},
            },
            "initializationOptions": options,
        });
        let result = self.request("initialize", params)?;
        self.encoding = match result["capabilities"]["positionEncoding"].as_str() {
            Some("utf-8") => Encoding::Utf8,
            _ => Encoding::Utf16,
        };
        self.capabilities = result["capabilities"].clone();
        self.notify("initialized", json!({}))
    }

    /// Sends a request and waits for its answer within the request timeout.
    pub fn request(&mut self, method: &str, params: Value) -> Result<Value, ClientError> {
        let id = self.send_request(method, params)?;
        self.wait_for(id, method, self.config.request_timeout)
    }

    /// Sends a request without waiting; pair it with [`Client::wait_for`].
    pub fn send_request(&mut self, method: &str, params: Value) -> Result<i64, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        Ok(id)
    }

    pub fn notify(&mut self, method: &str, params: Value) -> Result<(), ClientError> {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    /// Waits for the answer to request `id`. On timeout it tells the server to
    /// cancel the request.
    pub fn wait_for(
        &mut self,
        id: i64,
        method: &str,
        timeout: Duration,
    ) -> Result<Value, ClientError> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(response) = self.responses.remove(&id) {
                return into_result(&response);
            }
            self.check_interrupt()?;
            if let Some(ended) = self.ended.clone() {
                return Err(self.ended_error(&ended));
            }
            let now = Instant::now();
            if now >= deadline {
                let _ = self.notify("$/cancelRequest", json!({"id": id}));
                return Err(ClientError::Timeout {
                    method: method.to_string(),
                });
            }
            match self.inbound.recv_timeout((deadline - now).min(POLL_SLICE)) {
                Ok(Inbound::Message(message)) => self.route(message)?,
                Ok(Inbound::End(ended)) => self.ended = Some(ended),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => self.ended = Some(Ended::Closed),
            }
        }
    }

    /// Asks the server to shut down, then stops it. Returns how the process
    /// ended, or `None` when the client had no process.
    pub fn shutdown(mut self) -> Option<Exit> {
        let grace = self.config.shutdown_grace;
        if let Ok(id) = self.send_request("shutdown", Value::Null) {
            let _ = self.wait_for(id, "shutdown", grace);
        }
        let _ = self.notify("exit", Value::Null);
        self.stop()
    }

    fn stop(&mut self) -> Option<Exit> {
        self.writer = None;
        let grace = self.config.shutdown_grace;
        self.process
            .take()
            .map(|mut process| process.terminate(grace))
    }

    fn check_interrupt(&self) -> Result<(), ClientError> {
        match &self.interrupt {
            Some(flag) if flag.load(Ordering::SeqCst) => Err(ClientError::Interrupted),
            _ => Ok(()),
        }
    }

    fn route(&mut self, message: Value) -> Result<(), ClientError> {
        let has_method = message.get("method").is_some();
        match (message.get("id").and_then(Value::as_i64), has_method) {
            (Some(_), true) => self.answer_server_request(&message),
            (Some(id), false) => {
                self.responses.insert(id, message);
                Ok(())
            }
            (None, true) => {
                if message["method"] == "experimental/serverStatus" {
                    self.quiescent = message["params"]["quiescent"].as_bool();
                }
                self.notifications.push(message);
                Ok(())
            }
            (None, false) => Ok(()),
        }
    }

    fn answer_server_request(&mut self, request: &Value) -> Result<(), ClientError> {
        let id = &request["id"];
        let reply = match request["method"].as_str() {
            Some("workspace/configuration") => {
                json!({"jsonrpc": "2.0", "id": id, "result": (self.configuration)(&request["params"])})
            }
            Some("window/workDoneProgress/create" | "client/registerCapability") => {
                json!({"jsonrpc": "2.0", "id": id, "result": null})
            }
            _ => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": METHOD_NOT_FOUND, "message": "method not supported by this client"},
            }),
        };
        self.send(&reply)
    }

    fn send(&mut self, message: &Value) -> Result<(), ClientError> {
        if let Some(ended) = self.ended.clone() {
            return Err(self.ended_error(&ended));
        }
        let writer = self.writer.as_ref().ok_or(ClientError::Crashed {
            detail: "the connection is closed".to_string(),
        })?;
        let (done, outcome) = channel();
        let job = WriteJob {
            frame: message.to_string().into_bytes(),
            done,
        };
        if writer.send(job).is_err() {
            return Err(self.crashed());
        }
        match outcome.recv_timeout(self.config.write_timeout) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) if e.kind() == io::ErrorKind::BrokenPipe => Err(self.crashed()),
            Ok(Err(e)) => Err(ClientError::Write(e)),
            Err(RecvTimeoutError::Timeout) => Err(ClientError::WriteTimeout),
            Err(RecvTimeoutError::Disconnected) => Err(self.crashed()),
        }
    }

    fn ended_error(&mut self, ended: &Ended) -> ClientError {
        match ended {
            Ended::Closed => self.crashed(),
            Ended::ResponseLimit(limit) => ClientError::ResponseLimit { limit: *limit },
            Ended::Bad(why) => ClientError::Protocol(why.clone()),
        }
    }

    /// The error for a server that went away, with its exit status when known.
    fn crashed(&mut self) -> ClientError {
        let grace = self.config.shutdown_grace;
        let detail = match self.process.as_mut().and_then(|p| p.exit_within(grace)) {
            Some(status) => format!("it exited with {status}"),
            None => "it closed the connection".to_string(),
        };
        ClientError::Crashed { detail }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The `result` of a response, or its error mapped to a typed variant.
fn into_result(response: &Value) -> Result<Value, ClientError> {
    let Some(error) = response.get("error") else {
        return Ok(response["result"].clone());
    };
    let code = error["code"].as_i64().unwrap_or(0);
    let message = error["message"].as_str().unwrap_or("").to_string();
    Err(match code {
        CONTENT_MODIFIED | SERVER_CANCELLED => ClientError::Retryable { code, message },
        _ => ClientError::Server { code, message },
    })
}

#[cfg(test)]
mod tests;
