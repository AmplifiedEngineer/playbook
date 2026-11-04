// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The tombstone index: an append-only file of JSON lines, one per state
//! change of an item. The last record for an id wins.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The index file name under the gc folder.
const INDEX_FILE: &str = "index.jsonl";

/// Where an item is in its life. `Pending` and `Copied` are mid-move states;
/// the last four are terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Pending,
    Copied,
    Done,
    Expiring,
    Expired,
    Restoring,
    Restored,
    Lost,
    Dropped,
}

impl State {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Expired | Self::Restored | Self::Lost | Self::Dropped
        )
    }
}

/// How a move got the item into the tombstone folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Source and destination share a filesystem, so one rename moves it.
    Rename,
    /// Copy to a temp folder, rename it into place, then remove the source.
    Copy,
}

/// The size and newest mtime of a source, taken when the copy finished, so a
/// later run can tell whether the source changed after the copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    pub bytes: u64,
    pub mtime: i64,
}

impl Fingerprint {
    /// Whether `self` differs from `recorded` in any way. Used while a source
    /// is held still, where any difference means someone wrote to it.
    pub fn changed_since(&self, recorded: &Self) -> bool {
        self != recorded
    }

    /// Whether `self` shows more or newer data than `recorded`. A source with
    /// less data is a partly removed one, which is not a change.
    pub fn grew_since(&self, recorded: &Self) -> bool {
        self.mtime > recorded.mtime || self.bytes > recorded.bytes
    }
}

/// One line of the index: the whole item plus its state at time `at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub state: State,
    /// Epoch seconds when this state was recorded.
    pub at: i64,
    /// Where each member lived before the move.
    pub paths: Vec<PathBuf>,
    /// The folder that holds the members, each under its own name.
    pub dest: PathBuf,
    pub size: u64,
    pub reason: String,
    pub tombstoned_at: i64,
    pub retain_days: u32,
    pub mode: Mode,
    /// The newest mtime of the source when it was tombstoned. Expiry ignores it.
    pub source_mtime: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<Fingerprint>,
    /// Members that were at neither path when the move finished.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<PathBuf>,
}

impl Record {
    #[must_use]
    pub fn with_state(&self, state: State, at: i64) -> Self {
        Self {
            state,
            at,
            ..self.clone()
        }
    }

    /// Where `original` lives inside the tombstone.
    pub fn member_dest(&self, original: &Path) -> PathBuf {
        self.dest.join(original.file_name().unwrap_or_default())
    }

    /// The staging folder a copy-mode move fills before the rename into place.
    pub fn tmp_dest(&self) -> PathBuf {
        Self::tmp_of(&self.dest)
    }

    pub fn tmp_of(dest: &Path) -> PathBuf {
        let mut name = OsString::from(dest.as_os_str());
        name.push(".tmp");
        PathBuf::from(name)
    }
}

pub fn index_path(gc_dir: &Path) -> PathBuf {
    gc_dir.join(INDEX_FILE)
}

pub fn encode(record: &Record) -> String {
    serde_json::to_string(record).unwrap_or_default()
}

/// Appends one line and syncs it. A last line left unfinished by a crash gets
/// a newline first, so the new record starts on its own line.
pub fn append_line(index: &Path, line: &str) -> io::Result<()> {
    if let Some(dir) = index.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut file = fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(index)?;
    let mut text = String::new();
    if file.metadata()?.len() > 0 {
        let mut last = [0_u8; 1];
        file.seek(SeekFrom::End(-1))?;
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            text.push('\n');
        }
    }
    text.push_str(line);
    text.push('\n');
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

/// Every record in file order. A missing file is empty, and a line that does
/// not parse (a torn last line) is skipped.
pub fn read(index: &Path) -> Vec<Record> {
    let Ok(text) = fs::read_to_string(index) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// The newest record of each id, in the order the ids first appeared.
pub fn latest(records: Vec<Record>) -> Vec<Record> {
    let mut out: Vec<Record> = Vec::new();
    for record in records {
        match out.iter_mut().find(|seen| seen.id == record.id) {
            Some(slot) => *slot = record,
            None => out.push(record),
        }
    }
    out
}

/// Replaces the index with exactly `records`.
pub fn rewrite(index: &Path, records: &[Record]) -> io::Result<()> {
    let mut text = String::new();
    for record in records {
        text.push_str(&encode(record));
        text.push('\n');
    }
    crate::common::atomic::write_atomic_durable(index, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, state: State) -> Record {
        Record {
            id: id.into(),
            state,
            at: 10,
            paths: vec![PathBuf::from("/r/a")],
            dest: PathBuf::from("/t/d").join(id),
            size: 1,
            reason: "idle".into(),
            tombstoned_at: 10,
            retain_days: 7,
            mode: Mode::Rename,
            source_mtime: 5,
            fingerprint: None,
            missing: Vec::new(),
        }
    }

    #[test]
    fn missing_index_is_empty() {
        let dir = crate::common::test_support::scratch_dir("index-missing");

        assert!(read(&dir.join("index.jsonl")).is_empty());
    }

    #[test]
    fn torn_index_line_is_skipped() {
        let dir = crate::common::test_support::scratch_dir("index-torn");
        let file = dir.join("index.jsonl");
        append_line(&file, &encode(&record("a", State::Done))).unwrap();
        let mut torn = fs::OpenOptions::new().append(true).open(&file).unwrap();
        torn.write_all(b"{\"id\":\"b\",\"sta").unwrap();

        append_line(&file, &encode(&record("c", State::Done))).unwrap();

        let ids: Vec<String> = read(&file).into_iter().map(|r| r.id).collect();
        assert_eq!(ids, ["a", "c"]);
    }

    #[test]
    fn newest_record_of_an_id_wins() {
        let records = vec![
            record("a", State::Pending),
            record("b", State::Done),
            record("a", State::Done),
        ];

        let states: Vec<(String, State)> = latest(records)
            .into_iter()
            .map(|r| (r.id, r.state))
            .collect();

        assert_eq!(
            states,
            [("a".into(), State::Done), ("b".into(), State::Done)]
        );
    }
}
