// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A scripted language server for tests. It runs on a thread over an
//! in-process transport, touches no network or file, and never sleeps: a delay
//! is a reply held until the test releases a channel.

use super::framing::Limits;
use super::transport::Transport;
use serde_json::{json, Value};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// One thing the server does, in order.
pub(crate) enum Step {
    /// Waits for a request named `method` and answers it with `result`.
    Reply { method: &'static str, result: Value },
    /// Waits for a request named `method` and answers it with a JSON-RPC error.
    ReplyError {
        method: &'static str,
        code: i64,
        message: &'static str,
    },
    /// Waits for a request named `method`, then answers only after `release` fires.
    Hold {
        method: &'static str,
        result: Value,
        release: Receiver<()>,
    },
    /// Waits for any message named `method` (a request or a notification).
    Expect(&'static str),
    /// Reads the next `n` client messages without answering.
    Read(usize),
    /// Answers request `id` with `result` without reading anything first.
    ReplyId { id: i64, result: Value },
    /// Sends a notification to the client.
    Notify { method: &'static str, params: Value },
    /// Sends a request to the client and does not wait for the answer.
    Request {
        id: i64,
        method: &'static str,
        params: Value,
    },
    /// Sends bytes that are not a valid frame.
    Raw(Vec<u8>),
    /// Tells the test the script reached this point.
    Signal(Sender<()>),
    /// Closes the connection.
    Close,
}

/// A running fake and what it has received so far.
pub(crate) struct Fake {
    log: Arc<Mutex<Vec<Value>>>,
    thread: JoinHandle<()>,
}

impl Fake {
    /// Starts the script on `transport`. The script ends at `Close` or at the
    /// end of the steps, and either way the connection drops.
    pub(crate) fn spawn(transport: Transport, script: Vec<Step>) -> Self {
        let log = Arc::new(Mutex::new(Vec::new()));
        let thread_log = Arc::clone(&log);
        let thread = std::thread::spawn(move || run(transport, script, &thread_log));
        Self { log, thread }
    }

    /// Waits for the script to end and returns every message the client sent.
    pub(crate) fn finish(self) -> Vec<Value> {
        self.thread.join().expect("the fake server thread panicked");
        let log = self.log.lock().expect("the fake server log is poisoned");
        log.clone()
    }
}

fn run(mut transport: Transport, script: Vec<Step>, log: &Mutex<Vec<Value>>) {
    for step in script {
        let sent = match step {
            Step::Reply { method, result } => answer(
                &mut transport,
                log,
                method,
                |id| json!({"jsonrpc": "2.0", "id": id, "result": result}),
            ),
            Step::ReplyError {
                method,
                code,
                message,
            } => answer(
                &mut transport,
                log,
                method,
                |id| json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
            ),
            Step::Hold {
                method,
                result,
                release,
            } => answer(&mut transport, log, method, |id| {
                let _ = release.recv();
                json!({"jsonrpc": "2.0", "id": id, "result": result})
            }),
            Step::Expect(method) => receive(&mut transport, log, method).is_some(),
            Step::Read(n) => (0..n).all(|_| next(&mut transport, log).is_some()),
            Step::ReplyId { id, result } => send(
                &mut transport,
                &json!({"jsonrpc": "2.0", "id": id, "result": result}),
            ),
            Step::Notify { method, params } => send(
                &mut transport,
                &json!({"jsonrpc": "2.0", "method": method, "params": params}),
            ),
            Step::Request { id, method, params } => send(
                &mut transport,
                &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
            ),
            Step::Raw(bytes) => transport.write_raw(&bytes).is_ok(),
            Step::Signal(tx) => {
                let _ = tx.send(());
                true
            }
            Step::Close => return,
        };
        if !sent {
            return;
        }
    }
}

fn send(transport: &mut Transport, message: &Value) -> bool {
    transport.write(message.to_string().as_bytes()).is_ok()
}

/// Logs messages until one is named `method`; `None` when the client hangs up.
fn receive(transport: &mut Transport, log: &Mutex<Vec<Value>>, method: &str) -> Option<Value> {
    loop {
        let message = next(transport, log)?;
        if message["method"] == method {
            return Some(message);
        }
    }
}

fn next(transport: &mut Transport, log: &Mutex<Vec<Value>>) -> Option<Value> {
    let body = transport.read(Limits::default()).ok()?;
    let message: Value = serde_json::from_slice(&body).ok()?;
    log.lock().ok()?.push(message.clone());
    Some(message)
}

fn answer(
    transport: &mut Transport,
    log: &Mutex<Vec<Value>>,
    method: &str,
    reply: impl FnOnce(&Value) -> Value,
) -> bool {
    match receive(transport, log, method) {
        Some(request) => send(transport, &reply(&request["id"])),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::framing::FrameError;
    use crate::lsp::transport::pipe_pair;

    fn call(client: &mut Transport, id: i64, method: &str) -> Value {
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method});
        client.write(request.to_string().as_bytes()).unwrap();
        let body = client.read(Limits::default()).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[test]
    fn the_script_replies_in_order_and_the_log_holds_what_the_client_sent() {
        // Arrange
        let (mut client, server) = pipe_pair().unwrap();
        let fake = Fake::spawn(
            server,
            vec![
                Step::Reply {
                    method: "initialize",
                    result: json!({"capabilities": {}}),
                },
                Step::Reply {
                    method: "shutdown",
                    result: Value::Null,
                },
            ],
        );

        // Act
        let first = call(&mut client, 1, "initialize");
        let second = call(&mut client, 2, "shutdown");
        let log = fake.finish();

        // Assert
        assert_eq!(first["id"], 1);
        assert_eq!(first["result"], json!({"capabilities": {}}));
        assert_eq!(second["id"], 2);
        let methods: Vec<_> = log.iter().map(|m| m["method"].clone()).collect();
        assert_eq!(methods, [json!("initialize"), json!("shutdown")]);
    }

    #[test]
    fn a_close_step_gives_the_client_end_of_input() {
        // Arrange
        let (mut client, server) = pipe_pair().unwrap();
        let fake = Fake::spawn(server, vec![Step::Close]);

        // Act
        let got = client.read(Limits::default());
        fake.finish();

        // Assert
        assert!(matches!(got, Err(FrameError::Closed)), "{got:?}");
    }

    #[test]
    fn notifications_and_requests_reach_the_client_and_expect_waits_for_a_named_message() {
        // Arrange
        let (mut client, server) = pipe_pair().unwrap();
        let (tx, reached) = std::sync::mpsc::channel();
        let fake = Fake::spawn(
            server,
            vec![
                Step::Notify {
                    method: "window/logMessage",
                    params: json!({"message": "hi"}),
                },
                Step::Request {
                    id: 9,
                    method: "workspace/configuration",
                    params: json!({}),
                },
                Step::Expect("exit"),
                Step::Signal(tx),
            ],
        );

        // Act
        let notice: Value =
            serde_json::from_slice(&client.read(Limits::default()).unwrap()).unwrap();
        let ask: Value = serde_json::from_slice(&client.read(Limits::default()).unwrap()).unwrap();
        client
            .write(br#"{"jsonrpc":"2.0","method":"exit"}"#)
            .unwrap();
        reached.recv().unwrap();
        let log = fake.finish();

        // Assert
        assert_eq!(notice["method"], "window/logMessage");
        assert_eq!(ask["id"], 9);
        assert_eq!(log.len(), 1);
        assert_eq!(log[0]["method"], "exit");
    }

    #[test]
    fn an_error_step_answers_with_a_json_rpc_error() {
        // Arrange
        let (mut client, server) = pipe_pair().unwrap();
        let fake = Fake::spawn(
            server,
            vec![Step::ReplyError {
                method: "textDocument/hover",
                code: -32801,
                message: "content modified",
            }],
        );

        // Act
        let reply = call(&mut client, 3, "textDocument/hover");
        fake.finish();

        // Assert
        assert_eq!(reply["error"]["code"], -32801);
    }

    #[test]
    fn a_held_reply_waits_for_its_release_and_raw_bytes_are_not_a_frame() {
        // Arrange
        let (mut client, server) = pipe_pair().unwrap();
        let (release, held) = std::sync::mpsc::channel();
        let fake = Fake::spawn(
            server,
            vec![
                Step::Hold {
                    method: "textDocument/hover",
                    result: json!("late"),
                    release: held,
                },
                Step::Raw(b"not a frame\r\n\r\n".to_vec()),
            ],
        );
        let request = json!({"jsonrpc": "2.0", "id": 7, "method": "textDocument/hover"});
        client.write(request.to_string().as_bytes()).unwrap();

        // Act
        release.send(()).unwrap();
        let reply: Value =
            serde_json::from_slice(&client.read(Limits::default()).unwrap()).unwrap();
        let bad = client.read(Limits::default());
        fake.finish();

        // Assert
        assert_eq!(reply["result"], "late");
        assert!(matches!(bad, Err(FrameError::MissingLength)), "{bad:?}");
    }
}
