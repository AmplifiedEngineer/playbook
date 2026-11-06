// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A small local OTLP/HTTP receiver for Claude Code telemetry.
//!
//! Hand rolled on `std::net` so no HTTP crate is added. It binds 127.0.0.1
//! only, blocks in `accept` and gives each connection its own thread (capped),
//! with hard limits (header size, body size, content type, request rate, read
//! deadline), requires a bearer token when one is set, and answers
//! `Connection: close` to every request. Only the JSON protocol is supported.
//! `/v1/logs` and `/v1/metrics` are parsed by `otlp` (the attribute allowlist)
//! and stored by the writer thread; `/v1/traces` is accepted and dropped.
//! `POST /v1/playbook/shutdown` with the token stops the collector.

use super::otlp;
use super::writer::{self, Job, Kind};
use playbook_store::Connection;
use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Largest header block read.
pub const MAX_HEADER_BYTES: usize = 16 * 1024;
/// Largest body accepted, plain or after chunk decoding.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;
/// Requests allowed in one `RATE_WINDOW`; more get 429.
pub const MAX_REQUESTS_PER_SEC: u32 = 50;
/// The span `MAX_REQUESTS_PER_SEC` counts over.
pub const RATE_WINDOW: Duration = Duration::from_secs(1);
/// Deadline for reading one whole request.
pub const READ_DEADLINE: Duration = Duration::from_secs(5);
/// Connection threads alive at once; one more is answered 503 and closed.
pub const MAX_LIVE_CONNECTIONS: usize = 64;
/// Stops the collector when posted with the bearer token. It needs no
/// content type, only a body length (`Content-Length: 0` will do).
pub const SHUTDOWN_PATH: &str = "/v1/playbook/shutdown";
/// How long the wake up connect may take; the listener is on this host.
const WAKE_TIMEOUT: Duration = Duration::from_millis(500);

/// How the receiver behaves.
pub struct Config {
    /// Bearer token the client must send, or `None` for no check.
    pub token: Option<String>,
    /// Stop after this long with no request, or `None` to run until stopped.
    pub idle: Option<Duration>,
    /// The writer prunes telemetry rows past these limits every few hours.
    pub prune: Option<super::telemetry_db::Retention>,
}

/// A parsed request: the path (no query) and the body.
#[derive(Debug)]
struct Request {
    path: String,
    body: Vec<u8>,
}

type Headers = [(String, String)];
type Check<'a> = dyn Fn(&str, &str, &Headers) -> Result<(), HttpError> + 'a;

/// A failure that has an HTTP answer.
#[derive(Debug, PartialEq)]
struct HttpError(u16, &'static str);

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn read_more(
    stream: &mut impl Read,
    buf: &mut Vec<u8>,
    deadline: Instant,
) -> Result<(), HttpError> {
    if Instant::now() >= deadline {
        return Err(HttpError(408, "request timeout"));
    }
    let mut chunk = [0u8; 4096];
    match stream.read(&mut chunk) {
        Ok(0) => Err(HttpError(400, "connection closed early")),
        Ok(n) => {
            buf.extend_from_slice(&chunk[..n]);
            Ok(())
        }
        Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
            Err(HttpError(408, "request timeout"))
        }
        Err(_) => Err(HttpError(400, "read failed")),
    }
}

/// Decodes a chunked body from `buf` (bytes after the headers), reading more
/// as needed, never holding more than `MAX_BODY_BYTES`.
fn read_chunked(
    stream: &mut impl Read,
    mut buf: Vec<u8>,
    deadline: Instant,
) -> Result<Vec<u8>, HttpError> {
    let mut body = Vec::new();
    loop {
        let line_end = loop {
            if let Some(i) = find(&buf, b"\r\n") {
                break i;
            }
            if buf.len() > 64 {
                return Err(HttpError(400, "bad chunk size"));
            }
            read_more(stream, &mut buf, deadline)?;
        };
        let size_text = std::str::from_utf8(&buf[..line_end])
            .map_err(|_| HttpError(400, "bad chunk size"))?
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        let size =
            usize::from_str_radix(size_text, 16).map_err(|_| HttpError(400, "bad chunk size"))?;
        buf.drain(..line_end + 2);
        if size == 0 {
            return Ok(body);
        }
        if body.len() + size > MAX_BODY_BYTES {
            return Err(HttpError(413, "body too large"));
        }
        while buf.len() < size + 2 {
            read_more(stream, &mut buf, deadline)?;
        }
        body.extend_from_slice(&buf[..size]);
        if &buf[size..size + 2] != b"\r\n" {
            return Err(HttpError(400, "bad chunk"));
        }
        buf.drain(..size + 2);
    }
}

/// Reads one request, enforcing the limits. `check` sees the method, path and
/// headers before the body is read, so a rejected request costs no body read.
fn read_request(stream: &mut impl Read, check: &Check) -> Result<Request, HttpError> {
    let deadline = Instant::now() + READ_DEADLINE;
    let mut buf = Vec::new();
    let head_end = loop {
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i;
        }
        if buf.len() > MAX_HEADER_BYTES {
            return Err(HttpError(431, "headers too large"));
        }
        read_more(stream, &mut buf, deadline)?;
    };
    if head_end > MAX_HEADER_BYTES {
        return Err(HttpError(431, "headers too large"));
    }
    let head = std::str::from_utf8(&buf[..head_end]).map_err(|_| HttpError(400, "bad headers"))?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let (Some(method), Some(path), Some(version), None) =
        (first.next(), first.next(), first.next(), first.next())
    else {
        return Err(HttpError(400, "bad request line"));
    };
    if !version.starts_with("HTTP/1.") {
        return Err(HttpError(400, "unsupported version"));
    }
    let mut headers = Vec::new();
    for line in lines {
        let (k, v) = line.split_once(':').ok_or(HttpError(400, "bad header"))?;
        headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
    }
    check(method, path, &headers)?;
    let get = |n: &str| {
        headers
            .iter()
            .find(|(k, _)| k == n)
            .map(|(_, v)| v.as_str())
    };
    let path = path.to_string();
    let rest = buf.split_off(head_end + 4);
    let body = match (get("content-length"), get("transfer-encoding")) {
        (Some(_), Some(_)) => return Err(HttpError(400, "ambiguous body length")),
        (None, Some(te)) if te.eq_ignore_ascii_case("chunked") => {
            read_chunked(stream, rest, deadline)?
        }
        (None, Some(_)) => return Err(HttpError(501, "unsupported transfer encoding")),
        (Some(len), None) => {
            let len: usize = len
                .parse()
                .map_err(|_| HttpError(400, "bad content length"))?;
            if len > MAX_BODY_BYTES {
                return Err(HttpError(413, "body too large"));
            }
            let mut body = rest;
            while body.len() < len {
                read_more(stream, &mut body, deadline)?;
            }
            body.truncate(len);
            body
        }
        (None, None) => return Err(HttpError(411, "length required")),
    };
    Ok(Request {
        path: path.split('?').next().unwrap_or("").to_string(),
        body,
    })
}

fn tokens_equal(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Method, path, auth, content type and encoding, checked before the body.
fn precheck(
    token: Option<&str>,
    method: &str,
    path: &str,
    headers: &[(String, String)],
) -> Result<(), HttpError> {
    let get = |n: &str| {
        headers
            .iter()
            .find(|(k, _)| k == n)
            .map(|(_, v)| v.as_str())
    };
    let path = path.split('?').next().unwrap_or("");
    let otlp = matches!(path, "/v1/logs" | "/v1/metrics" | "/v1/traces");
    if !otlp && path != SHUTDOWN_PATH {
        return Err(HttpError(404, "not found"));
    }
    if method != "POST" {
        return Err(HttpError(405, "method not allowed"));
    }
    if let Some(token) = token {
        let ok = get("authorization")
            .and_then(|v| v.strip_prefix("Bearer "))
            .is_some_and(|t| tokens_equal(t.as_bytes(), token.as_bytes()));
        if !ok {
            return Err(HttpError(401, "unauthorized"));
        }
    }
    if !otlp {
        return Ok(());
    }
    let json = get("content-type").is_some_and(|v| {
        v.split(';')
            .next()
            .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json"))
    });
    if !json {
        return Err(HttpError(415, "content type must be application/json"));
    }
    if get("content-encoding").is_some_and(|v| !v.eq_ignore_ascii_case("identity")) {
        return Err(HttpError(415, "content encoding not supported"));
    }
    Ok(())
}

/// Parses a request and hands its rows to the writer. Returns the status and
/// body to send, after the writer has committed.
fn dispatch(jobs: &SyncSender<Job>, stop: &AtomicBool, req: &Request, now: i64) -> (u16, String) {
    let invalid = || (400, r#"{"error":"invalid OTLP JSON"}"#.to_string());
    let unavailable = || (503, r#"{"error":"store unavailable"}"#.to_string());
    let kind = match req.path.as_str() {
        "/v1/metrics" => match otlp::parse_metrics(&req.body, now) {
            Ok(points) => Kind::Metrics(points),
            Err(_) => return invalid(),
        },
        "/v1/logs" => match otlp::parse_events(&req.body, now) {
            Ok(rows) => Kind::Logs(rows),
            Err(_) => return invalid(),
        },
        _ => return (200, "{}".to_string()),
    };
    let (reply, answer) = std::sync::mpsc::sync_channel(1);
    match jobs.try_send(Job { kind, reply }) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => return unavailable(),
        Err(TrySendError::Disconnected(_)) => {
            // The writer is gone, so nothing can be stored: end the process
            // and let the next starter replace this collector.
            stop.store(true, Ordering::SeqCst);
            return unavailable();
        }
    }
    match answer.recv() {
        Ok(true) => (200, r#"{"partialSuccess":{}}"#.to_string()),
        Ok(false) => unavailable(),
        // A dropped reply means the writer died with the job queued.
        Err(_) => {
            stop.store(true, Ordering::SeqCst);
            unavailable()
        }
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        411 => "Length Required",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        501 => "Not Implemented",
        _ => "Service Unavailable",
    }
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let text = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nX-Playbook-Collector: 1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        reason(status),
        body.len()
    );
    let _ = stream.write_all(text.as_bytes());
}

/// Counts requests in the current `RATE_WINDOW`.
struct RateLimiter {
    window: Instant,
    count: u32,
}

impl RateLimiter {
    fn allow(&mut self, now: Instant) -> bool {
        if now.duration_since(self.window) >= RATE_WINDOW {
            self.window = now;
            self.count = 0;
        }
        self.count += 1;
        self.count <= MAX_REQUESTS_PER_SEC
    }
}

/// Binds 127.0.0.1 on `port` (0 picks a free one).
pub fn bind(port: u16) -> std::io::Result<TcpListener> {
    TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
}

/// What the accept loop and every connection thread share.
struct Shared {
    /// Taken by `serve` on the way out, so the writer sees the channel close
    /// while a silent client still holds its thread.
    jobs: Mutex<Option<SyncSender<Job>>>,
    token: Option<String>,
    stop: Arc<AtomicBool>,
    /// Set by the idle watchdog, so `serve` can say why it ended.
    idled: AtomicBool,
    limiter: Mutex<RateLimiter>,
    live: AtomicUsize,
    /// Milliseconds from `started` to the last real request.
    last_request: AtomicU64,
    started: Instant,
    addr: SocketAddr,
    now: fn() -> i64,
    /// Test-only (debug builds): read each request and never reply, to stage
    /// a hung collector.
    hang: bool,
}

impl Shared {
    fn new(
        addr: SocketAddr,
        jobs: SyncSender<Job>,
        token: Option<String>,
        stop: &Arc<AtomicBool>,
        now: fn() -> i64,
    ) -> Self {
        let started = Instant::now();
        Self {
            jobs: Mutex::new(Some(jobs)),
            token,
            stop: Arc::clone(stop),
            idled: AtomicBool::new(false),
            limiter: Mutex::new(RateLimiter {
                window: started,
                count: 0,
            }),
            live: AtomicUsize::new(0),
            last_request: AtomicU64::new(0),
            started,
            addr,
            now,
            hang: super::daemon::test_hang(),
        }
    }

    fn jobs(&self) -> Option<SyncSender<Job>> {
        self.jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn close(&self) {
        self.jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }

    fn millis(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn touch(&self) {
        self.last_request.store(self.millis(), Ordering::SeqCst);
    }

    fn quiet_for(&self) -> Duration {
        let last = self.last_request.load(Ordering::SeqCst);
        Duration::from_millis(self.millis().saturating_sub(last))
    }

    /// Sets the stop flag and wakes the blocking `accept`.
    fn halt(&self) {
        self.stop.store(true, Ordering::SeqCst);
        wake(self.addr);
    }
}

/// Connects once to the listener, so a blocking `accept` returns and the loop
/// sees the stop flag. Whoever sets the flag from outside must call this.
pub fn wake(addr: SocketAddr) {
    let _ = TcpStream::connect_timeout(&addr, WAKE_TIMEOUT);
}

/// Frees a connection slot when its thread ends, even on a panic.
struct Slot<'a>(&'a AtomicUsize);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Serves until `stop` is set (then call `wake`), a shutdown request comes or
/// the idle limit passes. Returns why it ended. The connection moves to the
/// writer thread, which commits what it holds before this returns.
pub fn serve(
    listener: &TcpListener,
    conn: Connection,
    cfg: &Config,
    stop: &Arc<AtomicBool>,
    now: fn() -> i64,
) -> std::io::Result<&'static str> {
    let addr = listener.local_addr()?;
    let (jobs, writer) = writer::spawn(conn, cfg.prune, now, Arc::clone(stop));
    let shared = Arc::new(Shared::new(addr, jobs, cfg.token.clone(), stop, now));
    serve_with_writer(listener, &shared, writer, cfg.idle)
}

/// `run` while a thread waits on the writer: a writer that ends before
/// `run` does (a panic sets only the flag) halts the loop, so `accept` does
/// not wait forever for a client that never comes.
fn serve_with_writer(
    listener: &TcpListener,
    shared: &Arc<Shared>,
    writer: JoinHandle<()>,
    idle: Option<Duration>,
) -> std::io::Result<&'static str> {
    let returned = Arc::new(AtomicBool::new(false));
    let monitor = {
        let (shared, returned) = (Arc::clone(shared), Arc::clone(&returned));
        std::thread::spawn(move || {
            let _ = writer.join();
            if !returned.load(Ordering::SeqCst) {
                shared.halt();
            }
        })
    };
    let why = run(listener, shared, idle);
    returned.store(true, Ordering::SeqCst);
    // Closing the channel lets the writer commit its last batch and exit.
    shared.close();
    let _ = monitor.join();
    why
}

/// The accept loop plus the idle watchdog.
fn run(
    listener: &TcpListener,
    shared: &Arc<Shared>,
    idle: Option<Duration>,
) -> std::io::Result<&'static str> {
    let (done, finished) = std::sync::mpsc::channel::<()>();
    let watchdog = idle.map(|idle| {
        let shared = Arc::clone(shared);
        std::thread::spawn(move || watch_idle(&shared, idle, &finished))
    });
    let result = accept_loop(listener, shared);
    // Dropping the sender ends the watchdog's wait at once.
    drop(done);
    if let Some(w) = watchdog {
        let _ = w.join();
    }
    result?;
    Ok(if shared.idled.load(Ordering::SeqCst) {
        "idle"
    } else {
        "stopped"
    })
}

fn watch_idle(shared: &Shared, idle: Duration, finished: &Receiver<()>) {
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            return;
        }
        let quiet = shared.quiet_for();
        if quiet >= idle {
            shared.idled.store(true, Ordering::SeqCst);
            shared.halt();
            return;
        }
        match finished.recv_timeout(idle - quiet) {
            Err(RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn accept_loop(listener: &TcpListener, shared: &Arc<Shared>) -> std::io::Result<()> {
    // Blocking, so an idle collector costs no wake ups; `wake` ends the wait.
    listener.set_nonblocking(false)?;
    while !shared.stop.load(Ordering::SeqCst) {
        let mut stream = match listener.accept() {
            Ok((s, _)) => s,
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::Interrupted | ErrorKind::ConnectionAborted
                ) =>
            {
                continue;
            }
            Err(e) => return Err(e),
        };
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        let _ = stream.set_read_timeout(Some(READ_DEADLINE));
        let _ = stream.set_write_timeout(Some(READ_DEADLINE));
        if shared.live.load(Ordering::SeqCst) >= MAX_LIVE_CONNECTIONS {
            respond(&mut stream, 503, r#"{"error":"too many connections"}"#);
            continue;
        }
        shared.live.fetch_add(1, Ordering::SeqCst);
        let mine = Arc::clone(shared);
        let spawned = std::thread::Builder::new().spawn(move || {
            let _slot = Slot(&mine.live);
            handle(&mine, stream);
        });
        // The closure never ran, so its slot is given back here.
        if spawned.is_err() {
            shared.live.fetch_sub(1, Ordering::SeqCst);
        }
    }
    Ok(())
}

/// Reads, routes and answers one connection on its own thread.
fn handle(shared: &Shared, mut stream: TcpStream) {
    if shared.hang {
        let _ = read_request(&mut stream, &|_, _, _| Ok(()));
        // Holds the stream open with no reply, shutdown included.
        loop {
            std::thread::park();
        }
    }
    let token = shared.token.as_deref();
    // The limit is checked once the headers are in, so an authenticated
    // shutdown is never starved by a flood.
    let check = |m: &str, p: &str, h: &[(String, String)]| {
        let allowed = shared
            .limiter
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .allow(Instant::now());
        let verdict = precheck(token, m, p, h);
        let shutdown = verdict.is_ok() && p.split('?').next() == Some(SHUTDOWN_PATH);
        if allowed || shutdown {
            verdict
        } else {
            Err(HttpError(429, "rate limit"))
        }
    };
    let req = match read_request(&mut stream, &check) {
        Ok(req) => req,
        Err(HttpError(status, msg)) => {
            respond(&mut stream, status, &format!(r#"{{"error":"{msg}"}}"#));
            return;
        }
    };
    // Only a real request keeps the collector alive. A bare connect (a
    // liveness probe from `cc` or `usage status`) or a rejected request does
    // not, so polling cannot hold it open.
    shared.touch();
    if req.path == SHUTDOWN_PATH {
        respond(&mut stream, 200, "{}");
        shared.halt();
        return;
    }
    let (status, body) = match shared.jobs() {
        Some(jobs) => dispatch(&jobs, &shared.stop, &req, (shared.now)()),
        None => (503, r#"{"error":"shutting down"}"#.to_string()),
    };
    respond(&mut stream, status, &body);
    // `dispatch` sets the flag when the writer is gone; the loop must see it.
    if shared.stop.load(Ordering::SeqCst) {
        wake(shared.addr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry_db::{count_requests, migrate_up};
    use crate::util::within;
    use std::io::Cursor;

    /// A file database (the writer owns one connection) and a second
    /// connection to read what it stored.
    fn db(name: &str) -> (Connection, Connection) {
        let path = crate::util::scratch_dir(name).join("usage.db");
        let writer = crate::db::open_db(&path).unwrap();
        migrate_up(&writer).unwrap();
        (writer, crate::db::open_db(&path).unwrap())
    }

    fn raw(method_path: &str, headers: &[&str], body: &[u8]) -> Vec<u8> {
        let mut v = format!("{method_path} HTTP/1.1\r\n").into_bytes();
        for h in headers {
            v.extend_from_slice(format!("{h}\r\n").as_bytes());
        }
        v.extend_from_slice(b"\r\n");
        v.extend_from_slice(body);
        v
    }

    fn cfg(token: Option<&str>) -> Config {
        Config {
            token: token.map(str::to_string),
            idle: None,
            prune: None,
        }
    }

    fn parse(bytes: Vec<u8>, c: &Config) -> Result<Request, HttpError> {
        let check =
            |m: &str, p: &str, h: &[(String, String)]| precheck(c.token.as_deref(), m, p, h);
        read_request(&mut Cursor::new(bytes), &check)
    }

    const JSON: &str = "Content-Type: application/json";

    #[test]
    fn a_plain_json_post_is_read() {
        let req = parse(
            raw("POST /v1/logs", &[JSON, "Content-Length: 2"], b"{}"),
            &cfg(None),
        )
        .unwrap();
        assert_eq!(req.path, "/v1/logs");
        assert_eq!(req.body, b"{}");
    }

    #[test]
    fn a_chunked_body_is_decoded() {
        let req = parse(
            raw(
                "POST /v1/logs",
                &[JSON, "Transfer-Encoding: chunked"],
                b"2\r\n{\"\r\n2;x=1\r\na\"\r\n0\r\n\r\n",
            ),
            &cfg(None),
        )
        .unwrap();
        assert_eq!(req.body, b"{\"a\"");
    }

    #[test]
    fn a_body_over_the_limit_is_413_before_it_is_read() {
        let len = format!("Content-Length: {}", MAX_BODY_BYTES + 1);
        let err = parse(raw("POST /v1/logs", &[JSON, &len], b""), &cfg(None)).unwrap_err();
        assert_eq!(err.0, 413);
    }

    #[test]
    fn a_chunked_body_over_the_limit_is_413() {
        let big = format!("{:x}\r\n", MAX_BODY_BYTES + 1);
        let err = parse(
            raw(
                "POST /v1/logs",
                &[JSON, "Transfer-Encoding: chunked"],
                big.as_bytes(),
            ),
            &cfg(None),
        )
        .unwrap_err();
        assert_eq!(err.0, 413);
    }

    #[test]
    fn oversized_headers_are_431() {
        let long = format!("X-Pad: {}", "a".repeat(MAX_HEADER_BYTES + 10));
        let err = parse(raw("POST /v1/logs", &[JSON, &long], b""), &cfg(None)).unwrap_err();
        assert_eq!(err.0, 431);
    }

    #[test]
    fn wrong_path_method_type_encoding_and_token_are_refused() {
        let c = cfg(Some("secret"));
        let ok_auth = "Authorization: Bearer secret";
        let case = |req: Vec<u8>| parse(req, &c).unwrap_err().0;
        assert_eq!(
            case(raw(
                "POST /other",
                &[JSON, ok_auth, "Content-Length: 0"],
                b""
            )),
            404
        );
        assert_eq!(case(raw("GET /v1/logs", &[JSON, ok_auth], b"")), 405);
        assert_eq!(
            case(raw("POST /v1/logs", &[JSON, "Content-Length: 0"], b"")),
            401
        );
        assert_eq!(
            case(raw(
                "POST /v1/logs",
                &[JSON, "Authorization: Bearer nope", "Content-Length: 0"],
                b""
            )),
            401
        );
        assert_eq!(
            case(raw(
                "POST /v1/logs",
                &["Content-Type: text/plain", ok_auth, "Content-Length: 0"],
                b""
            )),
            415
        );
        assert_eq!(
            case(raw(
                "POST /v1/logs",
                &[JSON, ok_auth, "Content-Encoding: gzip", "Content-Length: 0"],
                b""
            )),
            415
        );
        assert_eq!(case(raw("POST /v1/logs", &[JSON, ok_auth], b"")), 411);
    }

    #[test]
    fn a_content_type_with_a_charset_is_accepted() {
        let req = raw(
            "POST /v1/logs",
            &[
                "Content-Type: application/json; charset=utf-8",
                "Content-Length: 0",
            ],
            b"",
        );
        assert!(parse(req, &cfg(None)).is_ok());
    }

    #[test]
    fn length_and_chunked_together_are_refused() {
        let req = raw(
            "POST /v1/logs",
            &[JSON, "Content-Length: 1", "Transfer-Encoding: chunked"],
            b"",
        );
        assert_eq!(parse(req, &cfg(None)).unwrap_err().0, 400);
    }

    #[test]
    fn the_rate_limiter_cuts_off_then_resets() {
        let t = Instant::now();
        let mut l = RateLimiter {
            window: t,
            count: 0,
        };
        for _ in 0..MAX_REQUESTS_PER_SEC {
            assert!(l.allow(t));
        }
        assert!(!l.allow(t));
        assert!(l.allow(t + RATE_WINDOW));
    }

    // End to end over a real loopback socket.

    fn post(port: u16, path: &str, extra: &[&str], body: &[u8]) -> (u16, String) {
        let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        let len = format!("Content-Length: {}", body.len());
        let mut headers = vec![JSON, len.as_str()];
        headers.extend_from_slice(extra);
        s.write_all(&raw(&format!("POST {path}"), &headers, body))
            .unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        let status = out
            .split(' ')
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        (status, out)
    }

    fn logs_body(request_id: &str) -> Vec<u8> {
        serde_json::json!({"resourceLogs":[{"scopeLogs":[{"logRecords":[{
        "attributes":[
            {"key":"event.name","value":{"stringValue":"api_request"}},
            {"key":"session.id","value":{"stringValue":"s1"}},
            {"key":"request_id","value":{"stringValue":request_id}},
            {"key":"prompt","value":{"stringValue":"top secret"}},
            {"key":"cost_usd","value":{"doubleValue":0.5}}
        ]}]}]}]})
        .to_string()
        .into_bytes()
    }

    #[test]
    fn the_server_stores_rows_dedupes_and_stops_on_idle() {
        let (conn, reader) = db("receiver-store");
        let listener = bind(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(listener.local_addr().unwrap().ip().is_loopback());
        let c = Config {
            token: Some("tok".into()),
            idle: Some(Duration::from_millis(1500)),
            prune: None,
        };
        let stop = Arc::new(AtomicBool::new(false));
        let client = std::thread::spawn(move || {
            let auth = ["Authorization: Bearer tok"];
            let a = post(port, "/v1/logs", &auth, &logs_body("r1"));
            let b = post(port, "/v1/logs", &auth, &logs_body("r1"));
            let c = post(
                port,
                "/v1/logs",
                &["Authorization: Bearer bad"],
                &logs_body("r2"),
            );
            let d = post(port, "/v1/logs", &auth, b"nope");
            let m = br#"{"resourceMetrics":[{"scopeMetrics":[{"metrics":[{"name":"claude_code.active_time.total","sum":{"aggregationTemporality":1,"dataPoints":[{"startTimeUnixNano":"1","timeUnixNano":"2000000000","asDouble":9,"attributes":[{"key":"session.id","value":{"stringValue":"s1"}},{"key":"type","value":{"stringValue":"user"}}]}]}}]}]}]}"#;
            let e = post(port, "/v1/metrics", &auth, m);
            let e2 = post(port, "/v1/metrics", &auth, m);
            assert_eq!(e2.0, 200);
            [a.0, b.0, c.0, d.0, e.0]
        });
        let why = serve(&listener, conn, &c, &stop, || 1).unwrap();
        assert_eq!(client.join().unwrap(), [200, 200, 401, 400, 200]);
        assert_eq!(why, "idle");
        assert_eq!(count_requests(&reader).unwrap(), 1);
        let active: f64 = reader
            .query_row("SELECT SUM(value) FROM telemetry_metrics", [], |r| r.get(0))
            .unwrap();
        assert_eq!(active, 9.0, "a redelivered metric point is stored once");
        let dump: String = reader
            .query_row(
                "SELECT group_concat(model||effort||agent_name) FROM telemetry_requests",
                [],
                |r| r.get::<_, Option<String>>(0),
            )
            .unwrap()
            .unwrap_or_default();
        assert!(!dump.contains("secret"));
    }

    #[test]
    fn the_stop_flag_ends_the_server() {
        let (conn, _reader) = db("receiver-stop");
        let listener = bind(0).unwrap();
        let stop = Arc::new(AtomicBool::new(true));
        assert_eq!(
            serve(&listener, conn, &cfg(None), &stop, || 1).unwrap(),
            "stopped"
        );
    }

    #[test]
    fn tool_results_and_api_errors_are_stored_without_their_content() {
        let (conn, reader) = db("receiver-tools");
        let stop = Arc::new(AtomicBool::new(false));
        let (jobs, writer) = writer::spawn(conn, None, || 5, Arc::clone(&stop));
        let body = serde_json::json!({"resourceLogs":[{"scopeLogs":[{"logRecords":[
            {"timeUnixNano":"1760000000000000000","attributes":[
                {"key":"event.name","value":{"stringValue":"tool_result"}},
                {"key":"session.id","value":{"stringValue":"s1"}},
                {"key":"tool_use_id","value":{"stringValue":"t1"}},
                {"key":"tool_name","value":{"stringValue":"Bash"}},
                {"key":"success","value":{"stringValue":"false"}},
                {"key":"duration_ms","value":{"stringValue":"42"}},
                {"key":"tool_input","value":{"stringValue":"rm -rf secret"}}]},
            {"timeUnixNano":"1760000000000000000","attributes":[
                {"key":"event.name","value":{"stringValue":"api_error"}},
                {"key":"session.id","value":{"stringValue":"s1"}},
                {"key":"event.sequence","value":{"intValue":"7"}},
                {"key":"attempt","value":{"intValue":"2"}},
                {"key":"error","value":{"stringValue":"secret failure"}}]}
        ]}]}]})
        .to_string();
        let req = Request {
            path: "/v1/logs".into(),
            body: body.into_bytes(),
        };
        for _ in 0..2 {
            assert_eq!(dispatch(&jobs, &stop, &req, 5).0, 200);
        }
        drop(jobs);
        writer.join().unwrap();
        let h = crate::telemetry_db::health(&reader, 0, i64::MAX).unwrap();
        assert_eq!(
            (h.tool_calls, h.tool_failures, h.errors, h.retried),
            (1, 1, 1, 1)
        );
        let dump: String = reader
            .query_row(
                "SELECT (SELECT group_concat(tool_name) FROM telemetry_tools)
                      || (SELECT group_concat(model) FROM telemetry_errors)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            !dump.contains("secret") && !dump.contains("rm -rf"),
            "{dump}"
        );
    }

    #[test]
    fn a_probe_that_sends_nothing_does_not_keep_the_server_alive() {
        let (conn, _reader) = db("receiver-probe");
        let listener = bind(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        let c = Config {
            token: None,
            idle: Some(Duration::from_millis(1200)),
            prune: None,
        };
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let flag = done.clone();
        let prober = std::thread::spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                let _ = std::net::TcpStream::connect(("127.0.0.1", port));
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        let started = Instant::now();
        let why = serve(&listener, conn, &c, &stop, || 1).unwrap();
        done.store(true, Ordering::SeqCst);
        prober.join().unwrap();
        assert_eq!(why, "idle");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    fn logs_request() -> Request {
        Request {
            path: "/v1/logs".into(),
            body: logs_body("r1"),
        }
    }

    #[test]
    fn a_full_writer_channel_answers_503_without_waiting() {
        let (jobs, _held_back) = std::sync::mpsc::sync_channel(1);
        let stop = AtomicBool::new(false);
        let filler = std::sync::mpsc::sync_channel(1).0;
        jobs.try_send(Job {
            kind: Kind::Metrics(Vec::new()),
            reply: filler,
        })
        .unwrap();

        let (status, _) = dispatch(&jobs, &stop, &logs_request(), 1);

        assert_eq!(status, 503);
        assert!(!stop.load(Ordering::SeqCst), "a full channel is not fatal");
    }

    #[test]
    fn a_gone_writer_answers_503_and_stops_the_collector() {
        let (jobs, gone) = std::sync::mpsc::sync_channel(1);
        drop(gone);
        let stop = AtomicBool::new(false);

        let (status, _) = dispatch(&jobs, &stop, &logs_request(), 1);

        assert_eq!(status, 503);
        assert!(stop.load(Ordering::SeqCst));
    }

    /// Generous bound for a reply that should come at once.
    const PROMPT: Duration = Duration::from_secs(1);
    /// Slack past `READ_DEADLINE` before a silent client must be closed.
    const GRACE: Duration = Duration::from_secs(3);
    const AUTH: &str = "Authorization: Bearer tok";

    fn token_cfg(idle: Option<Duration>) -> Config {
        Config {
            token: Some("tok".into()),
            idle,
            prune: None,
        }
    }

    /// Serves on a thread and always stops it in `Drop`, so a failed
    /// assertion cannot leave one serving.
    struct Server {
        port: u16,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<std::io::Result<&'static str>>>,
    }

    impl Server {
        fn spawn(
            body: impl FnOnce(&TcpListener, &Arc<AtomicBool>) -> std::io::Result<&'static str>
                + Send
                + 'static,
        ) -> Self {
            let listener = bind(0).unwrap();
            let port = listener.local_addr().unwrap().port();
            let stop = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&stop);
            let thread = std::thread::spawn(move || body(&listener, &flag));
            Self {
                port,
                stop,
                thread: Some(thread),
            }
        }

        fn serve(conn: Connection, c: Config) -> Self {
            Self::spawn(move |listener, stop| serve(listener, conn, &c, stop, || 1))
        }

        /// Waits up to `bound` for serve to return on its own.
        fn ended_within(&mut self, bound: Duration) -> Option<&'static str> {
            let finished = within(bound, || {
                self.thread.as_ref().is_some_and(|t| t.is_finished())
            });
            if !finished {
                return None;
            }
            Some(self.thread.take()?.join().unwrap().unwrap())
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            wake(SocketAddr::from((Ipv4Addr::LOCALHOST, self.port)));
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    fn connect(port: u16) -> TcpStream {
        TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap()
    }

    fn shutdown(port: u16, auth: &str) -> (u16, String) {
        post(port, SHUTDOWN_PATH, &[auth], b"")
    }

    #[test]
    fn a_stalled_client_does_not_delay_another() {
        let (conn, _reader) = db("receiver-stalled");
        let server = Server::serve(conn, token_cfg(None));
        let mut stalled = connect(server.port);
        stalled
            .write_all(b"POST /v1/logs HTTP/1.1\r\nContent-Type: appl")
            .unwrap();

        let started = Instant::now();
        let (status, _) = post(server.port, "/v1/logs", &[AUTH], &logs_body("r1"));

        assert_eq!(status, 200);
        assert!(started.elapsed() < PROMPT, "{:?}", started.elapsed());
    }

    /// Starts the rate window this far ahead, so it cannot roll over while a
    /// test counts.
    const FROZEN: Duration = Duration::from_secs(3600);

    fn freeze_rate_window(shared: &Shared) {
        let mut l = shared.limiter.lock().unwrap();
        l.window = Instant::now() + FROZEN;
        l.count = 0;
    }

    #[test]
    fn the_rate_limit_holds_across_threads() {
        const THREADS: u32 = 6;
        const EACH: u32 = 10;
        let (server, shared) = serve_shared(None);
        freeze_rate_window(&shared);
        let port = server.port;
        let gate = Arc::new(std::sync::Barrier::new(THREADS as usize));
        let clients: Vec<_> = (0..THREADS)
            .map(|_| {
                let gate = Arc::clone(&gate);
                std::thread::spawn(move || {
                    gate.wait();
                    // Traces skip the writer, which nobody reads here.
                    (0..EACH)
                        .map(|_| post(port, "/v1/traces", &[AUTH], b"{}"))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let replies: Vec<_> = clients
            .into_iter()
            .flat_map(|c| c.join().unwrap())
            .collect();

        let limited: Vec<_> = replies.iter().filter(|(s, _)| *s == 429).collect();
        let expected = (THREADS * EACH - MAX_REQUESTS_PER_SEC) as usize;
        assert_eq!(limited.len(), expected);
        assert!(limited
            .iter()
            .all(|(_, text)| text.contains("X-Playbook-Collector: 1")));
    }

    #[test]
    fn shutdown_with_the_token_stops_serve_and_keeps_earlier_rows() {
        let (conn, reader) = db("receiver-shutdown");
        let mut server = Server::serve(conn, token_cfg(None));
        assert_eq!(
            post(server.port, "/v1/logs", &[AUTH], &logs_body("r1")).0,
            200
        );

        assert_eq!(shutdown(server.port, AUTH).0, 200);

        assert_eq!(server.ended_within(PROMPT), Some("stopped"));
        assert_eq!(count_requests(&reader).unwrap(), 1);
    }

    #[test]
    fn shutdown_with_a_wrong_token_is_401_and_serving_goes_on() {
        let (conn, _reader) = db("receiver-shutdown-401");
        let mut server = Server::serve(conn, token_cfg(None));

        assert_eq!(shutdown(server.port, "Authorization: Bearer nope").0, 401);
        assert_eq!(shutdown(server.port, "X-None: 1").0, 401);

        assert_eq!(post(server.port, "/v1/traces", &[AUTH], b"{}").0, 200);
        assert_eq!(server.ended_within(Duration::ZERO), None);
    }

    #[test]
    fn with_no_traffic_serve_returns_idle() {
        const IDLE: Duration = Duration::from_secs(1);
        let (conn, _reader) = db("receiver-idle");
        let mut server = Server::serve(conn, token_cfg(Some(IDLE)));

        assert_eq!(server.ended_within(IDLE + GRACE), Some("idle"));
    }

    #[test]
    fn every_reply_carries_the_collector_header() {
        let (conn, _reader) = db("receiver-header");
        let server = Server::serve(conn, token_cfg(None));
        let port = server.port;
        let replies = [
            post(port, "/v1/traces", &[AUTH], b"{}"),
            post(port, "/v1/logs", &[AUTH], b"nope"),
            post(port, "/v1/logs", &[], b"{}"),
            post(port, "/elsewhere", &[AUTH], b""),
            shutdown(port, "Authorization: Bearer nope"),
            shutdown(port, AUTH),
        ];

        let statuses: Vec<u16> = replies.iter().map(|r| r.0).collect();
        assert_eq!(statuses, [200, 400, 401, 404, 401, 200]);
        for (status, text) in &replies {
            assert!(text.contains("X-Playbook-Collector: 1"), "{status}: {text}");
        }
    }

    #[test]
    fn a_gone_writer_answers_503_and_serve_returns() {
        let (jobs, gone) = std::sync::mpsc::sync_channel(1);
        drop(gone);
        let mut server = Server::spawn(move |listener, stop| {
            let shared = Arc::new(Shared::new(listener.local_addr()?, jobs, None, stop, || 1));
            run(listener, &shared, None)
        });

        let (status, _) = post(server.port, "/v1/logs", &[], &logs_body("r1"));

        assert_eq!(status, 503);
        assert_eq!(server.ended_within(PROMPT), Some("stopped"));
    }

    /// Serves with a writer channel nobody reads, exposing the shared state.
    fn serve_shared(token: Option<&str>) -> (Server, Arc<Shared>) {
        let token = token.map(str::to_string);
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let server = Server::spawn(move |listener, stop| {
            let (jobs, _unread) = std::sync::mpsc::sync_channel(1);
            let addr = listener.local_addr()?;
            let shared = Arc::new(Shared::new(addr, jobs, token, stop, || 1));
            tx.send(Arc::clone(&shared)).unwrap();
            run(listener, &shared, None)
        });
        let shared = rx.recv().unwrap();
        (server, shared)
    }

    #[test]
    fn a_silent_client_is_closed_after_the_read_deadline_and_frees_its_slot() {
        let (server, shared) = serve_shared(None);
        let mut silent = connect(server.port);
        let started = Instant::now();
        assert!(within(PROMPT, || shared.live.load(Ordering::SeqCst) == 1));

        let mut reply = String::new();
        let _ = silent.read_to_string(&mut reply);

        let waited = started.elapsed();
        assert!(reply.starts_with("HTTP/1.1 408"), "{reply}");
        assert!(waited < READ_DEADLINE + GRACE, "{waited:?}");
        assert!(within(PROMPT, || shared.live.load(Ordering::SeqCst) == 0));
    }

    #[test]
    fn a_hanging_server_reads_the_request_and_never_replies() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let server = Server::spawn(move |listener, stop| {
            let (jobs, _unread) = std::sync::mpsc::sync_channel(1);
            let mut shared = Shared::new(listener.local_addr()?, jobs, None, stop, || 1);
            shared.hang = true;
            let shared = Arc::new(shared);
            tx.send(Arc::clone(&shared)).unwrap();
            run(listener, &shared, None)
        });
        let shared = rx.recv().unwrap();
        let mut client = connect(server.port);
        client.set_read_timeout(Some(PROMPT)).unwrap();
        client
            .write_all(b"POST /v1/playbook/shutdown HTTP/1.1\r\nContent-Length: 0\r\n\r\n")
            .unwrap();

        let mut buf = [0u8; 64];
        let read = client.read(&mut buf);

        assert!(read.is_err(), "{read:?}");
        assert!(!shared.stop.load(Ordering::SeqCst));
    }

    #[test]
    fn a_connection_over_the_thread_cap_gets_503() {
        // Silent clients send no headers, so the rate limit never sees them.
        let (server, shared) = serve_shared(None);
        let held: Vec<_> = (0..MAX_LIVE_CONNECTIONS)
            .map(|_| connect(server.port))
            .collect();
        assert!(within(PROMPT, || shared.live.load(Ordering::SeqCst)
            == MAX_LIVE_CONNECTIONS));

        let mut over = connect(server.port);
        let mut reply = String::new();
        let _ = over.read_to_string(&mut reply);

        assert!(reply.starts_with("HTTP/1.1 503"), "{reply}");
        assert!(reply.contains("X-Playbook-Collector: 1"), "{reply}");
        drop(held);
    }

    /// A stand in writer that takes one job and hands it to `answer`.
    fn fake_writer(answer: fn(Job)) -> (SyncSender<Job>, std::thread::JoinHandle<()>) {
        let (jobs, queue) = std::sync::mpsc::sync_channel::<Job>(1);
        let handle = std::thread::spawn(move || answer(queue.recv().unwrap()));
        (jobs, handle)
    }

    #[test]
    fn a_writer_that_replies_false_answers_503() {
        let (jobs, handle) = fake_writer(|job| job.reply.send(false).unwrap());
        let stop = AtomicBool::new(false);

        let (status, _) = dispatch(&jobs, &stop, &logs_request(), 1);

        assert_eq!(status, 503);
        handle.join().unwrap();
    }

    #[test]
    fn a_dropped_job_reply_answers_503() {
        let (jobs, handle) = fake_writer(drop);
        let stop = AtomicBool::new(false);

        let (status, _) = dispatch(&jobs, &stop, &logs_request(), 1);

        assert_eq!(status, 503);
        handle.join().unwrap();
    }

    #[test]
    fn an_authenticated_shutdown_is_not_rate_limited() {
        let (mut server, shared) = serve_shared(Some("tok"));
        freeze_rate_window(&shared);
        for _ in 0..MAX_REQUESTS_PER_SEC {
            assert_eq!(post(server.port, "/v1/traces", &[AUTH], b"{}").0, 200);
        }
        assert_eq!(post(server.port, "/v1/traces", &[AUTH], b"{}").0, 429);
        assert_eq!(shutdown(server.port, "Authorization: Bearer nope").0, 429);

        assert_eq!(shutdown(server.port, AUTH).0, 200);

        assert_eq!(server.ended_within(PROMPT), Some("stopped"));
    }

    /// Bound for serve to notice a writer that died with no client waiting.
    const WRITER_GONE: Duration = Duration::from_secs(2);

    #[test]
    fn a_writer_panic_with_no_request_in_flight_ends_serve() {
        let (conn, _reader) = db("receiver-writer-panic");
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let mut server = Server::spawn(move |listener, stop| {
            let (jobs, writer) = writer::spawn(conn, None, || 1, Arc::clone(stop));
            tx.send(jobs.clone()).unwrap();
            let addr = listener.local_addr()?;
            let shared = Arc::new(Shared::new(addr, jobs, None, stop, || 1));
            serve_with_writer(listener, &shared, writer, None)
        });
        let jobs = rx.recv().unwrap();
        // Sent straight to the writer: no connection is open or waiting.
        let (reply, _answer) = std::sync::mpsc::sync_channel(1);
        jobs.send(Job {
            kind: Kind::Panic,
            reply,
        })
        .unwrap();

        assert_eq!(server.ended_within(WRITER_GONE), Some("stopped"));
    }

    #[test]
    fn a_job_queued_when_shutdown_comes_is_still_stored() {
        let (conn, reader) = db("receiver-shutdown-queued");
        let (queued_tx, queued) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let mut server = Server::spawn(move |listener, stop| {
            let (real, writer) = writer::spawn(conn, None, || 1, Arc::clone(stop));
            // A relay holds each job until the test releases it.
            let (jobs, held) = std::sync::mpsc::sync_channel::<Job>(1);
            let relay = std::thread::spawn(move || {
                for job in held {
                    queued_tx.send(()).unwrap();
                    released.recv().unwrap();
                    real.send(job).unwrap();
                }
                drop(real);
                writer.join().unwrap();
            });
            let addr = listener.local_addr()?;
            let shared = Arc::new(Shared::new(addr, jobs, Some("tok".into()), stop, || 1));
            serve_with_writer(listener, &shared, relay, None)
        });
        let port = server.port;
        let client = std::thread::spawn(move || post(port, "/v1/logs", &[AUTH], &logs_body("r1")));
        queued.recv_timeout(PROMPT).unwrap();

        assert_eq!(shutdown(server.port, AUTH).0, 200);
        assert_eq!(server.ended_within(Duration::ZERO), None, "serve waits");
        release.send(()).unwrap();

        assert_eq!(client.join().unwrap().0, 200);
        assert_eq!(server.ended_within(PROMPT), Some("stopped"));
        assert_eq!(count_requests(&reader).unwrap(), 1);
    }
}
