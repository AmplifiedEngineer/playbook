// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The shared config snapshot. Playbook writes `sync-keys.json` (which keys may
//! leave the machine) and `config-values.json` (their global values); this
//! module pushes that snapshot and turns the other machines' snapshots into a
//! proposal file. It never changes the local config.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use playbook_common::atomic::{ensure_private_dir, write_atomic_private};
use serde_json::{json, Value as Json};

use crate::push::send_batch;
use crate::remote::REMOTE_SCHEMA_VERSION;
use crate::transport::{Statement, Transport, TransportError, Value};

pub const SYNC_KEYS_FILE: &str = "sync-keys.json";
pub const CONFIG_VALUES_FILE: &str = "config-values.json";
pub const CONFIG_PROPOSAL_FILE: &str = "config-proposal.json";
/// Never leaves the machine, even when the include list names it.
const ALWAYS_EXCLUDED_PREFIX: &str = "sync.";
const ORIGIN_CHARS: usize = 8;

const READ_OWN_SQL: &str = "SELECT key, value FROM shared_config_snapshots WHERE machine = ?";
const READ_OTHERS_SQL: &str = "SELECT machine, key, value, pushed_at FROM shared_config_snapshots \
     WHERE machine <> ? ORDER BY pushed_at, machine";
const UPSERT_SQL: &str = "INSERT INTO shared_config_snapshots \
     (machine, key, value, pushed_at, schema_version) VALUES (?, ?, ?, ?, ?) \
     ON CONFLICT(machine, key) DO UPDATE SET value = excluded.value, pushed_at = excluded.pushed_at \
     WHERE shared_config_snapshots.value <> excluded.value";
const DELETE_SQL: &str = "DELETE FROM shared_config_snapshots WHERE machine = ? AND key = ?";

#[derive(Debug)]
pub enum ConfigSyncError {
    Transport(TransportError),
    Io { path: PathBuf, reason: String },
    Corrupt { path: PathBuf, reason: &'static str },
    Malformed,
}

impl fmt::Display for ConfigSyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(f, "{error}"),
            Self::Io { path, reason } => write!(f, "cannot use {}: {reason}", path.display()),
            Self::Corrupt { path, reason } => {
                write!(f, "{} is not usable: {reason}", path.display())
            }
            Self::Malformed => write!(f, "the remote answered with an unexpected config row"),
        }
    }
}

impl std::error::Error for ConfigSyncError {}

impl From<TransportError> for ConfigSyncError {
    fn from(error: TransportError) -> Self {
        Self::Transport(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PullStats {
    pub proposed: usize,
    pub rejected: usize,
}

struct SyncKeys {
    include: BTreeSet<String>,
    exclude: Vec<String>,
}

impl SyncKeys {
    fn allows(&self, key: &str) -> bool {
        self.include.contains(key)
            && !key.starts_with(ALWAYS_EXCLUDED_PREFIX)
            && !self.exclude.iter().any(|prefix| key.starts_with(prefix))
    }
}

fn read_json(path: &Path) -> Result<Option<Json>, ConfigSyncError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(ConfigSyncError::Io {
                path: path.to_path_buf(),
                reason: e.to_string(),
            })
        }
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|_| ConfigSyncError::Corrupt {
            path: path.to_path_buf(),
            reason: "it is not valid JSON",
        })
}

fn strings(value: Option<&Json>, path: &Path) -> Result<Vec<String>, ConfigSyncError> {
    let corrupt = |reason| ConfigSyncError::Corrupt {
        path: path.to_path_buf(),
        reason,
    };
    value.map_or(Ok(vec![]), |list| {
        list.as_array()
            .ok_or_else(|| corrupt("a key list is not an array"))?
            .iter()
            .map(|item| item.as_str().map(str::to_string))
            .collect::<Option<_>>()
            .ok_or_else(|| corrupt("a key list holds a non-text item"))
    })
}

fn load_keys(data_dir: &Path) -> Result<Option<SyncKeys>, ConfigSyncError> {
    let path = data_dir.join(SYNC_KEYS_FILE);
    let Some(json) = read_json(&path)? else {
        return Ok(None);
    };
    Ok(Some(SyncKeys {
        include: strings(json.get("include"), &path)?.into_iter().collect(),
        exclude: strings(json.get("exclude"), &path)?,
    }))
}

fn load_values(data_dir: &Path) -> Result<Option<BTreeMap<String, String>>, ConfigSyncError> {
    let path = data_dir.join(CONFIG_VALUES_FILE);
    let Some(json) = read_json(&path)? else {
        return Ok(None);
    };
    let corrupt = |reason| ConfigSyncError::Corrupt {
        path: path.clone(),
        reason,
    };
    json.get("values")
        .and_then(Json::as_object)
        .ok_or_else(|| corrupt("it has no values object"))?
        .iter()
        .map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
        .collect::<Option<_>>()
        .map(Some)
        .ok_or_else(|| corrupt("a value is not text"))
}

fn text(value: &str) -> Value {
    Value::Text(value.to_string())
}

fn cell(value: &Value) -> Result<&str, ConfigSyncError> {
    match value {
        Value::Text(text) => Ok(text),
        _ => Err(ConfigSyncError::Malformed),
    }
}

/// Makes this machine's remote rows equal the shareable local values, writing
/// only what changed. Returns how many rows were added, changed or removed.
pub fn push_config(
    transport: &dyn Transport,
    data_dir: &Path,
    machine: &str,
    now: i64,
) -> Result<usize, ConfigSyncError> {
    let (Some(keys), Some(values)) = (load_keys(data_dir)?, load_values(data_dir)?) else {
        return Ok(0);
    };
    let snapshot: BTreeMap<&String, &String> =
        values.iter().filter(|(key, _)| keys.allows(key)).collect();
    let remote = transport.execute(&[Statement {
        sql: READ_OWN_SQL.to_string(),
        args: vec![text(machine)],
    }])?;
    let mut stale = BTreeMap::new();
    for row in &remote[0].rows {
        stale.insert(cell(&row[0])?.to_string(), cell(&row[1])?.to_string());
    }
    let mut writes = Vec::new();
    for (key, value) in snapshot {
        if stale.remove(key).as_ref() != Some(value) {
            writes.push(Statement {
                sql: UPSERT_SQL.to_string(),
                args: vec![
                    text(machine),
                    text(key),
                    text(value),
                    Value::Integer(now),
                    Value::Integer(i64::from(REMOTE_SCHEMA_VERSION)),
                ],
            });
        }
    }
    writes.extend(stale.keys().map(|key| Statement {
        sql: DELETE_SQL.to_string(),
        args: vec![text(machine), text(key)],
    }));
    if writes.is_empty() {
        return Ok(0);
    }
    let changed = writes.len();
    let plain = |sql: &str| Statement {
        sql: sql.to_string(),
        args: vec![],
    };
    let mut pipeline = vec![plain("BEGIN")];
    pipeline.extend(writes);
    pipeline.push(plain("COMMIT"));
    send_batch(transport, &pipeline)?;
    Ok(changed)
}

/// Writes `config-proposal.json` with the values the other machines hold that
/// differ from the local ones, newest push first. A pulled key that is not
/// shareable here is counted and dropped. No local config changes.
pub fn pull_config(
    transport: &dyn Transport,
    data_dir: &Path,
    machine: &str,
    now: i64,
) -> Result<PullStats, ConfigSyncError> {
    let keys = load_keys(data_dir)?;
    let local = load_values(data_dir)?.unwrap_or_default();
    let rows = transport.execute(&[Statement {
        sql: READ_OTHERS_SQL.to_string(),
        args: vec![text(machine)],
    }])?;
    // Rows arrive oldest first, so a later row for a key replaces an earlier one.
    let mut newest: BTreeMap<&str, (&str, &str)> = BTreeMap::new();
    for row in &rows[0].rows {
        newest.insert(cell(&row[1])?, (cell(&row[2])?, cell(&row[0])?));
    }
    let mut stats = PullStats::default();
    let (mut proposed, mut origin) = (BTreeMap::new(), BTreeMap::new());
    for (key, (value, from)) in newest {
        if !keys.as_ref().is_some_and(|keys| keys.allows(key)) {
            stats.rejected += 1;
        } else if local.get(key).map(String::as_str) != Some(value) {
            proposed.insert(key, value);
            origin.insert(key, from.chars().take(ORIGIN_CHARS).collect::<String>());
        }
    }
    stats.proposed = proposed.len();
    if proposed.is_empty() {
        remove_proposal(data_dir)?;
    } else {
        write_proposal(
            data_dir,
            &json!({"created_at": now, "proposed": proposed, "from": origin}),
        )?;
    }
    Ok(stats)
}

fn write_proposal(data_dir: &Path, proposal: &Json) -> Result<(), ConfigSyncError> {
    let path = data_dir.join(CONFIG_PROPOSAL_FILE);
    let io = |e: std::io::Error| ConfigSyncError::Io {
        path: path.clone(),
        reason: e.to_string(),
    };
    ensure_private_dir(data_dir).map_err(io)?;
    write_atomic_private(&path, proposal.to_string()).map_err(io)
}

fn remove_proposal(data_dir: &Path) -> Result<(), ConfigSyncError> {
    let path = data_dir.join(CONFIG_PROPOSAL_FILE);
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(ConfigSyncError::Io {
            path,
            reason: e.to_string(),
        }),
        _ => Ok(()),
    }
}
