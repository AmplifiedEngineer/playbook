// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Gives telemetry rows the real agent name.
//!
//! Claude Code reports an agent from a plugin that is not official as
//! `custom`, so `playbook:reviewer` and the other playbook agents land in
//! `telemetry_requests.agent_name` as `custom`. The transcripts keep the real
//! name: each subagent has `<session>/subagents/agent-<id>.meta.json` with
//! `agentType`, and its transcript lines carry the same `requestId` that the
//! telemetry row calls `request_id`.
//!
//! Only two things are read from a transcript: the `requestId` strings and the
//! `agentType` name, which must pass the same character check as an OTLP name.
//! No prompt, path or tool input leaves the file, and nothing is stored but
//! the name. The pass is a no-op while no row says `custom`.

use crate::claude_code::read_lines;
use crate::otlp::safe_text;
use playbook_store::{params, Connection};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_NAME_LEN: usize = 64;
const MAX_ID_LEN: usize = 128;
/// A meta file is a few hundred bytes; anything bigger is not one.
const MAX_META_BYTES: u64 = 16 * 1024;

/// The name Claude Code gives an agent from a plugin it does not list.
pub const CUSTOM: &str = "custom";

/// Replaces `custom` with the agent type of the transcript that holds the same
/// request. Returns the rows changed. A row without a matching transcript
/// keeps `custom`, so running it again later can still fix it.
pub fn resolve_custom(conn: &Connection, claude_projects: &Path) -> Result<usize, String> {
    let sessions = custom_requests(conn)?;
    if sessions.is_empty() {
        return Ok(0);
    }
    let projects: Vec<PathBuf> = fs::read_dir(claude_projects)
        .map(|dir| dir.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("failed to start the agent name update: {e}"))?;
    let mut changed = 0;
    for (session, wanted) in sessions {
        // The session id is a path segment next, so it must be a plain name.
        if safe_text(&session, MAX_ID_LEN).is_none()
            || session.contains('/')
            || session == "."
            || session == ".."
        {
            continue;
        }
        for project in &projects {
            let dir = project.join(&session).join("subagents");
            for (agent_type, requests) in subagents(&dir) {
                for request in requests.iter().filter(|r| wanted.contains(*r)) {
                    changed += rename(&tx, &session, request, &agent_type)?;
                }
            }
        }
    }
    tx.commit()
        .map_err(|e| format!("failed to save the agent names: {e}"))?;
    Ok(changed)
}

/// The request ids that still say `custom`, by session.
fn custom_requests(conn: &Connection) -> Result<BTreeMap<String, HashSet<String>>, String> {
    let mut stmt = conn
        .prepare("SELECT session_id, request_id FROM telemetry_requests WHERE agent_name = ?1")
        .map_err(|e| format!("failed to prepare the custom agent read: {e}"))?;
    let rows = stmt
        .query_map([CUSTOM], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|e| format!("failed to read custom agent rows: {e}"))?;
    let mut out: BTreeMap<String, HashSet<String>> = BTreeMap::new();
    for row in rows {
        let (session, request) =
            row.map_err(|e| format!("failed to read a custom agent row: {e}"))?;
        out.entry(session).or_default().insert(request);
    }
    Ok(out)
}

fn rename(conn: &Connection, session: &str, request: &str, name: &str) -> Result<usize, String> {
    conn.execute(
        "UPDATE telemetry_requests SET agent_name = ?1
         WHERE session_id = ?2 AND request_id = ?3 AND agent_name = ?4",
        params![name, session, request, CUSTOM],
    )
    .map_err(|e| format!("failed to set an agent name: {e}"))
}

/// Each subagent of one session folder: its agent type and the request ids of
/// its transcript. A folder, file or line that cannot be read adds nothing.
fn subagents(dir: &Path) -> Vec<(String, Vec<String>)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for meta in entries.flatten().map(|e| e.path()) {
        let Some(stem) = meta
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".meta.json"))
        else {
            continue;
        };
        let Some(agent_type) = agent_type(&meta) else {
            continue;
        };
        let mut requests = Vec::new();
        read_lines(&dir.join(format!("{stem}.jsonl")), &mut |line| {
            if let Some(id) = request_id(line) {
                requests.push(id);
            }
        });
        requests.sort();
        requests.dedup();
        out.push((agent_type, requests));
    }
    out
}

/// The `agentType` of a meta file when it is a safe name other than `custom`.
fn agent_type(meta: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    fs::File::open(meta)
        .ok()?
        .take(MAX_META_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_META_BYTES {
        return None;
    }
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let name = safe_text(value.get("agentType")?.as_str()?, MAX_NAME_LEN)?;
    (name != CUSTOM).then_some(name)
}

/// The `requestId` of an assistant line. Other lines are skipped unparsed.
fn request_id(line: &str) -> Option<String> {
    if !line.contains("\"requestId\"") {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;
    safe_text(value.get("requestId")?.as_str()?, MAX_ID_LEN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry_db::{insert_request, migrate_up, TelemetryRequest};
    const SESSION: &str = "bddb3ffa-134d-4e01-8127-59ee5a3d7d2a";

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrate_up(&conn).unwrap();
        conn
    }

    fn row(conn: &Connection, session: &str, request: &str, agent: &str) {
        insert_request(
            conn,
            &TelemetryRequest {
                session_id: session.into(),
                request_id: request.into(),
                timestamp: 10,
                model: "claude-sonnet-5-5".into(),
                agent_name: agent.into(),
                ..TelemetryRequest::default()
            },
        )
        .unwrap();
    }

    fn agent_of(conn: &Connection, request: &str) -> String {
        conn.query_row(
            "SELECT agent_name FROM telemetry_requests WHERE request_id = ?1",
            [request],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// A subagent folder shaped like Claude Code writes it.
    fn subagent(root: &Path, session: &str, id: &str, agent_type: &str, requests: &[&str]) {
        let dir: PathBuf = root.join("-Users-me-repo").join(session).join("subagents");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("agent-{id}.meta.json")),
            format!(
                r#"{{"agentType":"{agent_type}","isBuiltIn":false,"description":"Review the diff","name":"review","spawnDepth":1,"requestShape":"background","requestNonInteractive":true,"finished":true}}"#
            ),
        )
        .unwrap();
        let mut body = format!(
            r#"{{"parentUuid":null,"isSidechain":true,"agentId":"{id}","type":"user","message":{{"role":"user","content":"secret prompt text"}}}}"#
        );
        for request in requests {
            body.push_str(&format!(
                "\n{{\"parentUuid\":\"p\",\"isSidechain\":true,\"agentId\":\"{id}\",\"type\":\"assistant\",\"requestId\":\"{request}\",\"message\":{{\"model\":\"claude-sonnet-5-5\",\"id\":\"msg_{request}\",\"role\":\"assistant\",\"content\":[]}}}}"
            ));
        }
        fs::write(dir.join(format!("agent-{id}.jsonl")), body).unwrap();
    }

    fn root(tag: &str) -> PathBuf {
        crate::util::scratch_dir(tag)
    }

    #[test]
    fn a_custom_row_takes_the_agent_type_of_its_transcript() {
        let conn = conn();
        let dir = root("agent-names-resolve");
        row(&conn, SESSION, "req_011CgNdsCckhbg1JtfoAhChQ", "custom");
        row(&conn, SESSION, "req_011CgNdsnd9kWnkWnnw8VPMe", "custom");
        subagent(
            &dir,
            SESSION,
            "a5768ef9aeb956008",
            "playbook:auditor",
            &[
                "req_011CgNdsCckhbg1JtfoAhChQ",
                "req_011CgNdsnd9kWnkWnnw8VPMe",
            ],
        );

        let changed = resolve_custom(&conn, &dir).unwrap();

        assert_eq!(changed, 2);
        assert_eq!(
            agent_of(&conn, "req_011CgNdsCckhbg1JtfoAhChQ"),
            "playbook:auditor"
        );
        assert_eq!(
            agent_of(&conn, "req_011CgNdsnd9kWnkWnnw8VPMe"),
            "playbook:auditor"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn two_agents_in_one_session_each_keep_their_own_name() {
        let conn = conn();
        let dir = root("agent-names-two");
        row(&conn, SESSION, "req_a", "custom");
        row(&conn, SESSION, "req_b", "custom");
        subagent(&dir, SESSION, "a1", "playbook:reviewer", &["req_a"]);
        subagent(&dir, SESSION, "a2", "playbook:critic", &["req_b"]);

        resolve_custom(&conn, &dir).unwrap();

        assert_eq!(agent_of(&conn, "req_a"), "playbook:reviewer");
        assert_eq!(agent_of(&conn, "req_b"), "playbook:critic");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rows_that_are_not_custom_or_have_no_transcript_stay_as_they_are() {
        let conn = conn();
        let dir = root("agent-names-keep");
        row(&conn, SESSION, "req_named", "Explore");
        row(&conn, SESSION, "req_missing", "custom");
        row(&conn, "other-session", "req_other", "custom");
        subagent(&dir, SESSION, "a1", "playbook:reviewer", &["req_named"]);

        let changed = resolve_custom(&conn, &dir).unwrap();

        assert_eq!(changed, 0);
        assert_eq!(agent_of(&conn, "req_named"), "Explore");
        assert_eq!(agent_of(&conn, "req_missing"), "custom");
        assert_eq!(agent_of(&conn, "req_other"), "custom");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_name_with_unsafe_characters_is_not_stored() {
        let conn = conn();
        let dir = root("agent-names-unsafe");
        row(&conn, SESSION, "req_a", "custom");
        subagent(&dir, SESSION, "a1", "bad name; drop table", &["req_a"]);

        assert_eq!(resolve_custom(&conn, &dir).unwrap(), 0);
        assert_eq!(agent_of(&conn, "req_a"), "custom");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn nothing_is_read_when_no_row_says_custom() {
        let conn = conn();
        let dir = root("agent-names-not-custom");
        row(&conn, SESSION, "req_a", "Explore");
        subagent(&dir, SESSION, "a1", "playbook:reviewer", &["req_a"]);

        assert_eq!(resolve_custom(&conn, &dir).unwrap(), 0);
        assert_eq!(agent_of(&conn, "req_a"), "Explore");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_second_run_changes_nothing() {
        let conn = conn();
        let dir = root("agent-names-again");
        row(&conn, SESSION, "req_a", "custom");
        subagent(&dir, SESSION, "a1", "playbook:fact-checker", &["req_a"]);

        assert_eq!(resolve_custom(&conn, &dir).unwrap(), 1);
        assert_eq!(resolve_custom(&conn, &dir).unwrap(), 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_session_id_that_climbs_a_folder_is_never_followed() {
        let conn = conn();
        let dir = root("agent-names-climb");
        row(&conn, "..", "req_a", "custom");
        row(&conn, ".", "req_b", "custom");
        // <projects>/subagents is where ".." would lead.
        let escaped = dir.join("subagents");
        fs::create_dir_all(&escaped).unwrap();
        fs::write(
            escaped.join("agent-x.meta.json"),
            r#"{"agentType":"playbook:evil"}"#,
        )
        .unwrap();
        fs::write(
            escaped.join("agent-x.jsonl"),
            "{\"requestId\":\"req_a\"}\n{\"requestId\":\"req_b\"}",
        )
        .unwrap();
        // "." from a project folder leads to <project>/subagents.
        let project = dir.join("-Users-me-repo");
        fs::create_dir_all(project.join("subagents")).unwrap();
        fs::copy(
            escaped.join("agent-x.meta.json"),
            project.join("subagents/agent-x.meta.json"),
        )
        .unwrap();
        fs::copy(
            escaped.join("agent-x.jsonl"),
            project.join("subagents/agent-x.jsonl"),
        )
        .unwrap();

        assert_eq!(resolve_custom(&conn, &dir).unwrap(), 0);
        assert_eq!(agent_of(&conn, "req_a"), "custom");
        assert_eq!(agent_of(&conn, "req_b"), "custom");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_meta_file_that_cannot_name_the_agent_leaves_the_row_alone() {
        let cases: [(&str, Option<String>); 4] = [
            ("custom-type", Some(r#"{"agentType":"custom"}"#.into())),
            ("bad-json", Some("{not json".into())),
            ("no-type", Some(r#"{"name":"x"}"#.into())),
            (
                "oversized",
                Some(format!(
                    r#"{{"agentType":"playbook:big","pad":"{}"}}"#,
                    "a".repeat(20_000)
                )),
            ),
        ];
        for (tag, meta) in cases {
            let conn = conn();
            let dir = root(&format!("agent-names-{tag}"));
            row(&conn, SESSION, "req_a", "custom");
            let sub = dir.join("-Users-me-repo").join(SESSION).join("subagents");
            fs::create_dir_all(&sub).unwrap();
            fs::write(sub.join("agent-1.meta.json"), meta.unwrap()).unwrap();
            fs::write(sub.join("agent-1.jsonl"), "{\"requestId\":\"req_a\"}").unwrap();

            assert_eq!(resolve_custom(&conn, &dir).unwrap(), 0, "{tag}");
            assert_eq!(agent_of(&conn, "req_a"), "custom", "{tag}");
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn a_meta_file_without_a_transcript_changes_nothing() {
        let conn = conn();
        let dir = root("agent-names-no-jsonl");
        row(&conn, SESSION, "req_a", "custom");
        let sub = dir.join("-Users-me-repo").join(SESSION).join("subagents");
        fs::create_dir_all(&sub).unwrap();
        fs::write(
            sub.join("agent-1.meta.json"),
            r#"{"agentType":"playbook:reviewer"}"#,
        )
        .unwrap();

        assert_eq!(resolve_custom(&conn, &dir).unwrap(), 0);
        assert_eq!(agent_of(&conn, "req_a"), "custom");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_request_that_is_not_custom_is_not_renamed_by_a_sibling_transcript() {
        let conn = conn();
        let dir = root("agent-names-wanted");
        row(&conn, SESSION, "req_a", "custom");
        row(&conn, SESSION, "req_b", "Explore");
        subagent(
            &dir,
            SESSION,
            "a1",
            "playbook:reviewer",
            &["req_a", "req_b"],
        );

        assert_eq!(resolve_custom(&conn, &dir).unwrap(), 1);
        assert_eq!(agent_of(&conn, "req_b"), "Explore");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn an_ingest_names_the_custom_rows() {
        let paths = crate::util::scratch_paths("agent-names-ingest");
        let conn = crate::db::open_db(&paths.db).unwrap();
        row(&conn, SESSION, "req_a", "custom");
        subagent(
            &paths.claude_projects,
            SESSION,
            "a1",
            "playbook:critic",
            &["req_a"],
        );

        crate::run::ingest_into(&paths, &conn, &mut |_, _| true).unwrap();

        assert_eq!(agent_of(&conn, "req_a"), "playbook:critic");
    }
}
