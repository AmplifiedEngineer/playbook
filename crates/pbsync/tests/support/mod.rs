// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A scripted fake HTTP server on 127.0.0.1 that records what it receives.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

/// Placeholder in a reply body, replaced by the received Authorization header.
pub(crate) const ECHO_AUTH: &str = "{auth}";

pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) body: String,
}

impl Reply {
    pub(crate) fn new(status: u16, body: &str) -> Self {
        Self {
            status,
            body: body.to_string(),
        }
    }
}

pub(crate) struct Received {
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: String,
}

impl Received {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

pub(crate) struct FakeServer {
    pub(crate) url: String,
    requests: Arc<Mutex<Vec<Received>>>,
}

impl FakeServer {
    /// Replies follow the script in order; the last one repeats.
    pub(crate) fn start(script: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        thread::spawn(move || {
            for (turn, stream) in listener.incoming().flatten().enumerate() {
                let reply = &script[turn.min(script.len() - 1)];
                serve(stream, reply, &seen);
            }
        });
        Self { url, requests }
    }

    pub(crate) fn requests(&self) -> std::sync::MutexGuard<'_, Vec<Received>> {
        self.requests.lock().unwrap()
    }
}

fn serve(stream: TcpStream, reply: &Reply, seen: &Mutex<Vec<Received>>) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let mut headers = Vec::new();
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        let Some((name, value)) = line.trim_end().split_once(':') else {
            break;
        };
        headers.push((name.to_ascii_lowercase(), value.trim().to_string()));
    }
    let length = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let received = Received {
        headers,
        body: String::from_utf8(body).unwrap(),
    };
    let auth = received.header("authorization").unwrap_or("").to_string();
    seen.lock().unwrap().push(received);
    let body = reply.body.replace(ECHO_AUTH, &auth);
    let text = format!(
        "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        reply.status,
        body.len(),
        body
    );
    reader.get_mut().write_all(text.as_bytes()).unwrap();
}
