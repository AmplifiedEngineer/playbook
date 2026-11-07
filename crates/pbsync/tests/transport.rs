// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use pbsync::token::{resolve_token, Token, DEFAULT_TOKEN_ENV};
use pbsync::transport::{
    HttpTransport, Statement, StatementResult, Transport, TransportError, Value, BACKOFF_BASE,
    BACKOFF_CAP, MAX_ATTEMPTS,
};
use serde_json::{json, Value as Json};
use support::{FakeServer, Reply, ECHO_AUTH};

const SECRET: &str = "s3cret-token-value";

fn token() -> Token {
    resolve_token(Some(SECRET), DEFAULT_TOKEN_ENV, &|_| None).unwrap()
}

fn statement(sql: &str, args: Vec<Value>) -> Statement {
    Statement {
        sql: sql.to_string(),
        args,
    }
}

fn ok_result(cols: &[&str], rows: Json, affected: u64) -> Json {
    let cols: Vec<Json> = cols.iter().map(|name| json!({ "name": name })).collect();
    json!({"type": "ok", "response": {"type": "execute", "result":
        {"cols": cols, "rows": rows, "affected_row_count": affected}}})
}

fn reply(results: Vec<Json>) -> Reply {
    let mut all = results;
    all.push(json!({"type": "ok", "response": {"type": "close"}}));
    Reply::new(200, &json!({"results": all}).to_string())
}

fn transport(server: &FakeServer) -> (HttpTransport, Arc<Mutex<Vec<Duration>>>) {
    let delays = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&delays);
    let transport = HttpTransport::new(&server.url, token())
        .unwrap()
        .with_sleeper(move |d| record.lock().unwrap().push(d));
    (transport, delays)
}

#[test]
fn request_carries_bearer_header_and_typed_args() {
    // Arrange
    let server = FakeServer::start(vec![reply(vec![ok_result(&[], json!([]), 1)])]);
    let (transport, _) = transport(&server);
    let args = vec![
        Value::Text("a".into()),
        Value::Integer(42),
        Value::Real(1.5),
        Value::Null,
    ];
    // Act
    transport
        .execute(&[statement("INSERT INTO t VALUES (?, ?, ?, ?)", args)])
        .unwrap();
    // Assert
    let requests = server.requests();
    let sent: Json = serde_json::from_str(&requests[0].body).unwrap();
    assert_eq!(
        requests[0].header("authorization"),
        Some(format!("Bearer {SECRET}").as_str())
    );
    assert_eq!(
        sent,
        json!({"requests": [
            {"type": "execute", "stmt": {"sql": "INSERT INTO t VALUES (?, ?, ?, ?)", "args": [
                {"type": "text", "value": "a"},
                {"type": "integer", "value": "42"},
                {"type": "float", "value": 1.5},
                {"type": "null"}]}},
            {"type": "close"}]})
    );
}

#[test]
fn two_statements_return_two_results_in_order() {
    // Arrange
    let server = FakeServer::start(vec![reply(vec![
        ok_result(&[], json!([]), 3),
        ok_result(&["n"], json!([[{"type": "integer", "value": "7"}]]), 0),
    ])]);
    let (transport, _) = transport(&server);
    // Act
    let results = transport
        .execute(&[
            statement("DELETE FROM t", vec![]),
            statement("SELECT n", vec![]),
        ])
        .unwrap();
    // Assert
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].affected_rows, 3);
    assert_eq!(results[1].rows, vec![vec![Value::Integer(7)]]);
}

#[test]
fn rows_decode_every_value_kind() {
    // Arrange
    let row = json!([[
        {"type": "text", "value": "hi"},
        {"type": "integer", "value": "-5"},
        {"type": "float", "value": 2.5},
        {"type": "null"}
    ]]);
    let server = FakeServer::start(vec![reply(vec![ok_result(&["a", "b", "c", "d"], row, 0)])]);
    let (transport, _) = transport(&server);
    // Act
    let results = transport.execute(&[statement("SELECT 1", vec![])]).unwrap();
    // Assert
    assert_eq!(
        results,
        vec![StatementResult {
            columns: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            rows: vec![vec![
                Value::Text("hi".into()),
                Value::Integer(-5),
                Value::Real(2.5),
                Value::Null
            ]],
            affected_rows: 0,
        }]
    );
}

#[test]
fn statement_error_carries_index_and_message() {
    // Arrange
    let failure =
        json!({"type": "error", "error": {"message": "no such table: t", "code": "SQLITE_ERROR"}});
    let server = FakeServer::start(vec![reply(vec![ok_result(&[], json!([]), 0), failure])]);
    let (transport, _) = transport(&server);
    // Act
    let error = transport
        .execute(&[
            statement("SELECT 1", vec![]),
            statement("SELECT * FROM t", vec![]),
        ])
        .unwrap_err();
    // Assert
    assert_eq!(
        error,
        TransportError::Statement {
            index: 1,
            message: "no such table: t".into()
        }
    );
}

#[test]
fn unauthorized_maps_to_auth_error_with_hint() {
    // Arrange
    let server = FakeServer::start(vec![Reply::new(401, "{}")]);
    let (transport, _) = transport(&server);
    // Act
    let error = transport
        .execute(&[statement("SELECT 1", vec![])])
        .unwrap_err();
    // Assert
    assert_eq!(error, TransportError::Auth { status: 401 });
    assert!(error.to_string().contains("turso db tokens create"));
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn transient_503_is_retried_then_succeeds() {
    // Arrange
    let server = FakeServer::start(vec![
        Reply::new(503, "busy"),
        reply(vec![ok_result(&[], json!([]), 0)]),
    ]);
    let (transport, delays) = transport(&server);
    // Act
    let outcome = transport.execute(&[statement("SELECT 1", vec![])]);
    // Assert
    assert!(outcome.is_ok());
    assert_eq!(server.requests().len(), 2);
    assert_eq!(*delays.lock().unwrap(), vec![BACKOFF_BASE]);
}

#[test]
fn persistent_503_gives_up_after_the_attempt_limit() {
    // Arrange
    let server = FakeServer::start(vec![Reply::new(503, "busy")]);
    let (transport, delays) = transport(&server);
    // Act
    let error = transport
        .execute(&[statement("SELECT 1", vec![])])
        .unwrap_err();
    // Assert
    assert_eq!(
        error,
        TransportError::RetriesExhausted {
            attempts: MAX_ATTEMPTS,
            last: "HTTP 503".into()
        }
    );
    assert_eq!(server.requests().len(), MAX_ATTEMPTS as usize);
    let expected: Vec<Duration> = (0..MAX_ATTEMPTS - 1)
        .map(|n| (BACKOFF_BASE * 2u32.pow(n)).min(BACKOFF_CAP))
        .collect();
    assert_eq!(*delays.lock().unwrap(), expected);
}

#[test]
fn plain_http_to_a_remote_host_is_rejected() {
    // Arrange
    let urls = [
        "http://db.example.com",
        "http://localhost.evil.com",
        "http://localhost@evil.com",
    ];
    for url in urls {
        // Act
        let error = HttpTransport::new(url, token()).err().unwrap();
        // Assert
        assert!(matches!(error, TransportError::InsecureUrl { .. }), "{url}");
    }
}

#[test]
fn libsql_and_https_urls_are_accepted_and_others_rejected() {
    // Arrange
    let accepted = ["libsql://db-org.turso.io", "https://db-org.turso.io/"];
    // Act
    let results: Vec<bool> = accepted
        .iter()
        .map(|url| HttpTransport::new(url, token()).is_ok())
        .collect();
    let ftp = HttpTransport::new("ftp://x", token()).err().unwrap();
    // Assert
    assert_eq!(results, vec![true, true]);
    assert_eq!(ftp, TransportError::InvalidUrl);
}

#[test]
fn libsql_url_is_sent_as_https() {
    // Arrange
    let transport = HttpTransport::new("libsql://127.0.0.1:1", token())
        .unwrap()
        .with_sleeper(|_| {});
    // Act
    let error = transport
        .execute(&[statement("SELECT 1", vec![])])
        .unwrap_err();
    // Assert
    let text = error.to_string();
    assert!(!text.contains(SECRET));
    assert!(!matches!(error, TransportError::InvalidUrl));
}

#[test]
fn token_never_appears_in_error_text_when_the_server_echoes_it() {
    // Arrange
    let echo = format!("echo {ECHO_AUTH}");
    for status in [400, 401, 403, 503] {
        let server = FakeServer::start(vec![Reply::new(status, &echo)]);
        let (transport, _) = transport(&server);
        // Act
        let error = transport
            .execute(&[statement("SELECT 1", vec![])])
            .unwrap_err();
        // Assert
        let text = format!("{error} {error:?}");
        assert!(!text.contains(SECRET), "status {status}: {text}");
        assert!(
            !text.to_ascii_lowercase().contains("bearer"),
            "status {status}"
        );
    }
}
