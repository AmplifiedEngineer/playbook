// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The Turso SQL-over-HTTP transport. Error text and logs never carry the
//! token or the Authorization header, nor any part of a response body that
//! is not a decoded statement error.

use std::fmt;
use std::time::Duration;

use serde_json::{json, Value as Json};

use crate::token::Token;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const TOTAL_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_ATTEMPTS: u32 = 4;
pub const BACKOFF_BASE: Duration = Duration::from_millis(200);
pub const BACKOFF_CAP: Duration = Duration::from_secs(2);

const PIPELINE_PATH: &str = "/v2/pipeline";
const SCHEME_HTTPS: &str = "https://";
const SCHEME_LIBSQL: &str = "libsql://";
const SCHEME_HTTP: &str = "http://";
const LOCAL_HOSTS: [&str; 2] = ["127.0.0.1", "localhost"];
const STATUS_UNAUTHORIZED: u16 = 401;
const STATUS_FORBIDDEN: u16 = 403;
const AUTH_HINT: &str =
    "create a fresh token with `turso db tokens create <database>` and set it again";

/// One SQL statement with positional arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    pub sql: String,
    pub args: Vec<Value>,
}

/// A SQL value. Blobs are not needed yet.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
}

/// The outcome of one statement.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StatementResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub affected_rows: u64,
}

/// Runs statements in order on one connection.
pub trait Transport {
    fn execute(&self, statements: &[Statement]) -> Result<Vec<StatementResult>, TransportError>;
}

/// Why a transport call failed.
#[derive(Debug, PartialEq, Eq)]
pub enum TransportError {
    InvalidUrl,
    InsecureUrl { host: String },
    Auth { status: u16 },
    Statement { index: usize, message: String },
    Http { status: u16 },
    Network { reason: String },
    RetriesExhausted { attempts: u32, last: String },
    MalformedResponse { what: String },
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl => {
                write!(f, "the sync URL must start with {SCHEME_LIBSQL} or {SCHEME_HTTPS}")
            }
            Self::InsecureUrl { host } => write!(
                f,
                "refusing plain http to the non-local host {host}: use a {SCHEME_LIBSQL} or {SCHEME_HTTPS} URL"
            ),
            Self::Auth { status } => {
                write!(f, "the server rejected the token (HTTP {status}): {AUTH_HINT}")
            }
            Self::Statement { index, message } => {
                write!(f, "statement {index} failed: {message}")
            }
            Self::Http { status } => write!(f, "the server answered HTTP {status}"),
            Self::Network { reason } => write!(f, "the request failed: {reason}"),
            Self::RetriesExhausted { attempts, last } => {
                write!(f, "gave up after {attempts} attempts, last error: {last}")
            }
            Self::MalformedResponse { what } => {
                write!(f, "the server sent a malformed response: {what}")
            }
        }
    }
}

impl std::error::Error for TransportError {}

type Sleeper = Box<dyn Fn(Duration) + Send + Sync>;

/// What one HTTP attempt produced.
enum Attempt {
    Done(String),
    Transient(String),
    Fatal(TransportError),
}

/// Talks to Turso over `POST <base>/v2/pipeline` with bearer auth.
pub struct HttpTransport {
    endpoint: String,
    local: bool,
    token: Token,
    sleeper: Sleeper,
}

impl HttpTransport {
    /// Accepts `libsql://` and `https://` URLs, and `http://` only for a local host.
    pub fn new(base_url: &str, token: Token) -> Result<Self, TransportError> {
        let (base, local) = normalize_base(base_url)?;
        Ok(Self {
            endpoint: format!("{base}{PIPELINE_PATH}"),
            local,
            token,
            sleeper: Box::new(std::thread::sleep),
        })
    }

    /// Replaces the backoff sleep, so tests need not wait.
    #[must_use]
    pub fn with_sleeper(mut self, sleeper: impl Fn(Duration) + Send + Sync + 'static) -> Self {
        self.sleeper = Box::new(sleeper);
        self
    }

    fn post_with_retry(&self, body: &str) -> Result<String, TransportError> {
        let mut last = String::new();
        for attempt in 1..=MAX_ATTEMPTS {
            match self.post_once(body) {
                Attempt::Done(text) => return Ok(text),
                Attempt::Fatal(error) => return Err(error),
                Attempt::Transient(reason) => {
                    last = reason;
                    if attempt < MAX_ATTEMPTS {
                        (self.sleeper)(backoff(attempt));
                    }
                }
            }
        }
        Err(TransportError::RetriesExhausted {
            attempts: MAX_ATTEMPTS,
            last,
        })
    }

    fn post_once(&self, body: &str) -> Attempt {
        let mut config = ureq::Agent::config_builder()
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_global(Some(TOTAL_TIMEOUT))
            .http_status_as_error(false);
        if self.local {
            config = config.proxy(None);
        }
        let agent: ureq::Agent = config.build().into();
        let sent = agent
            .post(&self.endpoint)
            .header("Authorization", format!("Bearer {}", self.token.expose()))
            .content_type("application/json")
            .send(body);
        match sent {
            Ok(mut response) => {
                let status = response.status().as_u16();
                match status {
                    STATUS_UNAUTHORIZED | STATUS_FORBIDDEN => {
                        Attempt::Fatal(TransportError::Auth { status })
                    }
                    500..=599 => Attempt::Transient(format!("HTTP {status}")),
                    200..=299 => match response.body_mut().read_to_string() {
                        Ok(text) => Attempt::Done(text),
                        Err(error) => Attempt::Transient(error.to_string()),
                    },
                    _ => Attempt::Fatal(TransportError::Http { status }),
                }
            }
            Err(
                error @ (ureq::Error::Io(_)
                | ureq::Error::Timeout(_)
                | ureq::Error::ConnectionFailed),
            ) => Attempt::Transient(error.to_string()),
            Err(error) => Attempt::Fatal(TransportError::Network {
                reason: error.to_string(),
            }),
        }
    }
}

impl Transport for HttpTransport {
    fn execute(&self, statements: &[Statement]) -> Result<Vec<StatementResult>, TransportError> {
        let body = encode_request(statements).to_string();
        let text = self.post_with_retry(&body)?;
        decode_response(&text, statements.len())
    }
}

fn backoff(attempt: u32) -> Duration {
    BACKOFF_BASE
        .saturating_mul(1 << (attempt - 1))
        .min(BACKOFF_CAP)
}

/// Returns the https base without a trailing slash, and whether it is a local http base.
fn normalize_base(url: &str) -> Result<(String, bool), TransportError> {
    let trimmed = url.trim_end_matches('/');
    if let Some(rest) = trimmed.strip_prefix(SCHEME_LIBSQL) {
        return Ok((format!("{SCHEME_HTTPS}{rest}"), false));
    }
    if trimmed.starts_with(SCHEME_HTTPS) {
        return Ok((trimmed.to_string(), false));
    }
    let Some(rest) = trimmed.strip_prefix(SCHEME_HTTP) else {
        return Err(TransportError::InvalidUrl);
    };
    let authority = rest.split('/').next().unwrap_or_default();
    let host = authority.split(':').next().unwrap_or_default();
    if authority.contains('@') || !LOCAL_HOSTS.contains(&host) {
        return Err(TransportError::InsecureUrl {
            host: host.to_string(),
        });
    }
    Ok((trimmed.to_string(), true))
}

fn encode_value(value: &Value) -> Json {
    match value {
        Value::Null => json!({"type": "null"}),
        Value::Integer(n) => json!({"type": "integer", "value": n.to_string()}),
        Value::Real(x) => json!({"type": "float", "value": x}),
        Value::Text(s) => json!({"type": "text", "value": s}),
    }
}

fn encode_request(statements: &[Statement]) -> Json {
    let mut requests: Vec<Json> = statements
        .iter()
        .map(|statement| {
            let args: Vec<Json> = statement.args.iter().map(encode_value).collect();
            json!({"type": "execute", "stmt": {"sql": statement.sql, "args": args}})
        })
        .collect();
    requests.push(json!({"type": "close"}));
    json!({ "requests": requests })
}

fn malformed(what: &str) -> TransportError {
    TransportError::MalformedResponse {
        what: what.to_string(),
    }
}

fn decode_value(json: &Json) -> Result<Value, TransportError> {
    let kind = json["type"]
        .as_str()
        .ok_or_else(|| malformed("value type"))?;
    let payload = &json["value"];
    match kind {
        "null" => Ok(Value::Null),
        "text" => payload
            .as_str()
            .map(|s| Value::Text(s.to_string()))
            .ok_or_else(|| malformed("text value")),
        "integer" => payload
            .as_str()
            .and_then(|s| s.parse().ok())
            .or_else(|| payload.as_i64())
            .map(Value::Integer)
            .ok_or_else(|| malformed("integer value")),
        "float" => payload
            .as_f64()
            .map(Value::Real)
            .ok_or_else(|| malformed("float value")),
        _ => Err(malformed("unsupported value type")),
    }
}

fn decode_result(result: &Json) -> Result<StatementResult, TransportError> {
    let columns = result["cols"]
        .as_array()
        .ok_or_else(|| malformed("columns"))?
        .iter()
        .map(|col| col["name"].as_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| malformed("column name"))?;
    let rows = result["rows"]
        .as_array()
        .ok_or_else(|| malformed("rows"))?
        .iter()
        .map(|row| {
            row.as_array()
                .ok_or_else(|| malformed("row"))?
                .iter()
                .map(decode_value)
                .collect()
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(StatementResult {
        columns,
        rows,
        affected_rows: result["affected_row_count"].as_u64().unwrap_or(0),
    })
}

fn decode_response(text: &str, expected: usize) -> Result<Vec<StatementResult>, TransportError> {
    let parsed: Json = serde_json::from_str(text).map_err(|_| malformed("not JSON"))?;
    let results = parsed["results"]
        .as_array()
        .filter(|results| results.len() >= expected)
        .ok_or_else(|| malformed("results"))?;
    results
        .iter()
        .take(expected)
        .enumerate()
        .map(|(index, entry)| match entry["type"].as_str() {
            Some("ok") => decode_result(&entry["response"]["result"]),
            Some("error") => Err(TransportError::Statement {
                index,
                message: entry["error"]["message"]
                    .as_str()
                    .unwrap_or("no message")
                    .to_string(),
            }),
            _ => Err(malformed("result type")),
        })
        .collect()
}
