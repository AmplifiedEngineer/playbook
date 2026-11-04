// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::lsp::fake::{Fake, Step};
use crate::lsp::transport::pipe_pair;

fn quick() -> ClientConfig {
    ClientConfig {
        request_timeout: Duration::from_millis(100),
        write_timeout: Duration::from_millis(100),
        shutdown_grace: Duration::from_millis(100),
        max_response: 4096,
    }
}

/// A config whose waits are long enough that CI load cannot trip them.
fn patient() -> ClientConfig {
    ClientConfig {
        request_timeout: Duration::from_secs(10),
        write_timeout: Duration::from_secs(10),
        shutdown_grace: Duration::from_secs(5),
        ..quick()
    }
}

fn reply(method: &'static str, result: Value) -> Step {
    Step::Reply { method, result }
}

fn frame_of(body: &[u8]) -> Vec<u8> {
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(body);
    frame
}

#[test]
fn the_server_choosing_utf8_negotiates_utf8_and_otherwise_utf16() {
    for (answer, expected) in [
        (
            json!({"capabilities": {"positionEncoding": "utf-8"}}),
            Encoding::Utf8,
        ),
        (json!({"capabilities": {}}), Encoding::Utf16),
    ] {
        // Arrange
        let (client_side, server) = pipe_pair().unwrap();
        let fake = Fake::spawn(
            server,
            vec![reply("initialize", answer), Step::Expect("initialized")],
        );
        let mut client = Client::with_transport(client_side, patient());

        // Act
        client.initialize("file:///repo", json!({})).unwrap();
        let encoding = client.encoding();
        drop(client);
        let log = fake.finish();

        // Assert
        assert_eq!(encoding, expected);
        let offered = &log[0]["params"]["capabilities"]["general"]["positionEncodings"];
        assert_eq!(offered, &json!(["utf-8", "utf-16"]));
    }
}

#[test]
fn a_timeout_cancels_the_request_by_id() {
    // Arrange
    let (client_side, server) = pipe_pair().unwrap();
    let (release, held) = std::sync::mpsc::channel();
    let fake = Fake::spawn(
        server,
        vec![
            Step::Hold {
                method: "textDocument/hover",
                result: json!(null),
                release: held,
            },
            Step::Expect("$/cancelRequest"),
        ],
    );
    let mut client = Client::with_transport(client_side, quick());

    // Act
    let got = client.request("textDocument/hover", json!({}));
    release.send(()).unwrap();
    let log = fake.finish();

    // Assert
    assert!(matches!(got, Err(ClientError::Timeout { .. })), "{got:?}");
    let cancel = log
        .iter()
        .find(|m| m["method"] == "$/cancelRequest")
        .unwrap();
    assert_eq!(cancel["params"]["id"], 1);
}

#[test]
fn the_three_server_requests_each_get_a_reply_with_the_matching_id() {
    // Arrange
    let (client_side, server) = pipe_pair().unwrap();
    let fake = Fake::spawn(
        server,
        vec![
            Step::Request {
                id: 10,
                method: "workspace/configuration",
                params: json!({"items": [{}, {}]}),
            },
            Step::Request {
                id: 11,
                method: "window/workDoneProgress/create",
                params: json!({}),
            },
            Step::Request {
                id: 12,
                method: "client/registerCapability",
                params: json!({}),
            },
            reply("textDocument/hover", json!("done")),
            reply("shutdown", Value::Null),
            Step::Expect("exit"),
        ],
    );
    let mut client = Client::with_transport(client_side, patient());
    client.set_configuration_reply(Box::new(|_| json!(["a", "b"])));

    // Act
    let got = client.request("textDocument/hover", json!({})).unwrap();
    client.shutdown();
    let log = fake.finish();

    // Assert
    assert_eq!(got, "done");
    let answers: Vec<_> = log.iter().filter(|m| m.get("method").is_none()).collect();
    let ids: Vec<_> = answers.iter().map(|m| m["id"].clone()).collect();
    assert_eq!(ids, [json!(10), json!(11), json!(12)]);
    assert_eq!(answers[0]["result"], json!(["a", "b"]));
    assert!(answers[1]["result"].is_null() && answers[2]["result"].is_null());
}

#[test]
fn two_requests_in_flight_each_get_their_own_result_when_answers_arrive_reversed() {
    // Arrange
    let (client_side, server) = pipe_pair().unwrap();
    let fake = Fake::spawn(
        server,
        vec![
            Step::Read(2),
            Step::ReplyId {
                id: 2,
                result: json!("second"),
            },
            Step::ReplyId {
                id: 1,
                result: json!("first"),
            },
        ],
    );
    let mut client = Client::with_transport(client_side, patient());
    let a = client.send_request("a", json!({})).unwrap();
    let b = client.send_request("b", json!({})).unwrap();

    // Act
    let for_a = client.wait_for(a, "a", Duration::from_secs(10)).unwrap();
    let for_b = client.wait_for(b, "b", Duration::from_secs(10)).unwrap();
    fake.finish();

    // Assert
    assert_eq!((for_a, for_b), (json!("first"), json!("second")));
}

#[test]
fn a_notification_before_the_reply_is_kept_and_the_call_still_gets_its_reply() {
    // Arrange
    let (client_side, server) = pipe_pair().unwrap();
    let fake = Fake::spawn(
        server,
        vec![
            Step::Notify {
                method: "textDocument/publishDiagnostics",
                params: json!({"uri": "file:///a"}),
            },
            reply("x", json!(7)),
        ],
    );
    let mut client = Client::with_transport(client_side, patient());

    // Act
    let got = client.request("x", json!({})).unwrap();
    let kept = client.take_notifications();
    fake.finish();

    // Assert
    assert_eq!(got, 7);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0]["params"]["uri"], "file:///a");
}

#[test]
fn a_reply_with_an_unknown_id_is_ignored() {
    // Arrange
    let (client_side, server) = pipe_pair().unwrap();
    let stray = frame_of(br#"{"jsonrpc":"2.0","id":99,"result":"stray"}"#);
    let fake = Fake::spawn(server, vec![Step::Raw(stray), reply("x", json!("mine"))]);
    let mut client = Client::with_transport(client_side, patient());

    // Act
    let got = client.request("x", json!({})).unwrap();
    fake.finish();

    // Assert
    assert_eq!(got, "mine");
}

#[test]
fn json_rpc_errors_map_to_server_error_and_retryable_variants() {
    for (code, retryable) in [
        (-32602, false),
        (CONTENT_MODIFIED, true),
        (SERVER_CANCELLED, true),
    ] {
        // Arrange
        let (client_side, server) = pipe_pair().unwrap();
        let fake = Fake::spawn(
            server,
            vec![Step::ReplyError {
                method: "x",
                code,
                message: "nope",
            }],
        );
        let mut client = Client::with_transport(client_side, patient());

        // Act
        let err = client.request("x", json!({})).unwrap_err();
        fake.finish();

        // Assert
        assert_eq!(err.is_retryable(), retryable, "{err}");
        assert!(err.to_string().contains("server error"), "{err}");
    }
}

#[test]
fn the_response_limit_admits_the_cap_and_rejects_one_more() {
    let cap = quick().max_response;
    let overhead = br#"{"jsonrpc":"2.0","id":1,"result":""}"#.len();
    for (size, ok) in [(cap - 1, true), (cap, true), (cap + 1, false)] {
        // Arrange
        let pad = "x".repeat(size - overhead);
        let body = format!(r#"{{"jsonrpc":"2.0","id":1,"result":"{pad}"}}"#);
        assert_eq!(body.len(), size);
        let (client_side, server) = pipe_pair().unwrap();
        let fake = Fake::spawn(
            server,
            vec![Step::Expect("x"), Step::Raw(frame_of(body.as_bytes()))],
        );
        let config = ClientConfig {
            max_response: cap,
            ..patient()
        };
        let mut client = Client::with_transport(client_side, config);

        // Act
        let got = client.request("x", json!({}));
        fake.finish();

        // Assert
        match (ok, got) {
            (true, Ok(value)) => assert_eq!(value.as_str().map(str::len), Some(pad.len())),
            (false, Err(e @ ClientError::ResponseLimit { .. })) => {
                assert!(e.to_string().contains("response limit"), "{e}");
            }
            (_, other) => panic!("size {size}: {other:?}"),
        }
    }
}

#[test]
fn a_server_that_never_reads_makes_a_large_write_time_out() {
    // Arrange
    let (client_side, _server_that_never_reads) = pipe_pair().unwrap();
    let mut client = Client::with_transport(client_side, quick());
    let text = "x".repeat(1024 * 1024);

    // Act
    let got = client.notify("textDocument/didOpen", json!({"text": text}));

    // Assert
    let err = got.unwrap_err();
    assert!(matches!(err, ClientError::WriteTimeout), "{err:?}");
    assert!(err.to_string().contains("write"), "{err}");
}

#[test]
fn a_fake_that_answers_shutdown_and_exit_finishes_with_no_process_to_kill() {
    // Arrange
    let (client_side, server) = pipe_pair().unwrap();
    let fake = Fake::spawn(
        server,
        vec![reply("shutdown", Value::Null), Step::Expect("exit")],
    );
    let client = Client::with_transport(client_side, patient());

    // Act
    let exit = client.shutdown();
    let log = fake.finish();

    // Assert
    assert!(exit.is_none());
    assert_eq!(log.last().unwrap()["method"], "exit");
}

#[cfg(unix)]
mod unix {
    use super::*;
    use rustix::process::{test_kill_process, Pid};
    use std::os::unix::process::ExitStatusExt;
    use std::path::{Path, PathBuf};

    fn sh(script: &str, config: ClientConfig) -> Client {
        let args = [OsString::from("-c"), OsString::from(script)];
        Client::spawn(Path::new("/bin/sh"), &args, &std::env::temp_dir(), config).unwrap()
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = playbook_common::test_support::scratch_dir(tag);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Polls for the pid a script wrote, with a generous deadline.
    fn recorded_pid(file: &Path) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let text = std::fs::read_to_string(file).unwrap_or_default();
            if let Ok(pid) = text.trim().parse() {
                return pid;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the script never recorded a pid");
    }

    fn is_gone(pid: i32) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        let pid = Pid::from_raw(pid).unwrap();
        while Instant::now() < deadline {
            if test_kill_process(pid).is_err() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn a_server_that_exits_when_its_input_closes_is_not_killed() {
        // Arrange
        let client = sh("cat >/dev/null", quick());

        // Act
        let exit = client.shutdown().unwrap();

        // Assert
        assert!(!exit.killed, "{exit:?}");
        assert!(exit.status.unwrap().success());
    }

    #[test]
    fn a_server_that_ignores_shutdown_is_killed_after_the_grace_period() {
        // Arrange
        let client = sh("read x; exec sleep 300", quick());

        // Act
        let exit = client.shutdown().unwrap();

        // Assert
        assert!(exit.killed);
        assert_eq!(exit.status.unwrap().signal(), Some(9));
    }

    #[test]
    fn a_grandchild_that_ignores_input_closing_is_gone_after_teardown() {
        // Arrange
        let pidfile = scratch("grandchild").join("pid");
        let script = format!(
            "sleep 300 & echo $! > '{}'; cat >/dev/null",
            pidfile.display()
        );
        let client = sh(&script, quick());
        let pid = recorded_pid(&pidfile);

        // Act
        client.shutdown();

        // Assert
        assert!(is_gone(pid));
    }

    #[test]
    fn an_interrupt_stops_the_wait_and_dropping_the_client_kills_the_group() {
        // Arrange
        let pidfile = scratch("interrupt").join("pid");
        let script = format!(
            "sleep 300 & echo $! > '{}'; read x; wait",
            pidfile.display()
        );
        let flag = Arc::new(AtomicBool::new(false));
        let config = ClientConfig {
            shutdown_grace: Duration::from_millis(100),
            ..patient()
        };
        let mut client = sh(&script, config);
        client.set_interrupt(flag.clone());
        let pid = recorded_pid(&pidfile);
        flag.store(true, Ordering::SeqCst);

        // Act
        let got = client.request("textDocument/hover", json!({}));
        drop(client);

        // Assert
        assert!(matches!(got, Err(ClientError::Interrupted)), "{got:?}");
        assert!(is_gone(pid));
    }

    #[test]
    fn a_script_that_writes_one_frame_and_exits_3_gives_the_frame_then_a_crash_report() {
        // Arrange
        let body = r#"{"jsonrpc":"2.0","id":1,"result":"hi"}"#;
        let script = format!(
            "read x; printf 'Content-Length: {}\\r\\n\\r\\n%s' '{body}'; exit 3",
            body.len()
        );
        let mut client = sh(&script, patient());

        // Act
        let first = client.request("x", json!({})).unwrap();
        let second = client.request("y", json!({}));

        // Assert
        assert_eq!(first, "hi");
        let err = second.unwrap_err();
        assert!(matches!(err, ClientError::Crashed { .. }), "{err:?}");
        assert!(err.to_string().contains('3'), "{err}");
    }
}
