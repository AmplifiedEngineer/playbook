// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The audit log: one JSON object per line in `audit.jsonl`, rotated by size.
//! Text from GitHub can carry terminal escapes, so control characters are
//! replaced before a line is written.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// A file is rotated when the next line would take it past this size.
pub const MAX_BYTES: u64 = 10 * 1024 * 1024;
/// The live file plus the rotated ones.
pub const FILES: u32 = 5;

#[derive(Debug)]
pub struct AuditLog {
    dir: PathBuf,
    max_bytes: u64,
}

impl AuditLog {
    pub fn new(dir: &Path) -> Self {
        Self::with_limit(dir, MAX_BYTES)
    }

    pub fn with_limit(dir: &Path, max_bytes: u64) -> Self {
        Self {
            dir: dir.to_path_buf(),
            max_bytes,
        }
    }

    fn live(&self) -> PathBuf {
        self.dir.join("audit.jsonl")
    }

    fn rotated(&self, n: u32) -> PathBuf {
        self.dir.join(format!("audit.jsonl.{n}"))
    }

    /// Appends `record` as one line.
    pub fn append(&self, record: &Value) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let mut line = clean(record).to_string();
        line.push('\n');
        let size = fs::metadata(self.live()).map_or(0, |m| m.len());
        if size > 0 && size + line.len() as u64 > self.max_bytes {
            self.rotate()?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.live())?;
        file.write_all(line.as_bytes())
    }

    fn rotate(&self) -> io::Result<()> {
        let oldest = self.rotated(FILES - 1);
        if oldest.exists() {
            fs::remove_file(&oldest)?;
        }
        for n in (1..FILES - 1).rev() {
            if self.rotated(n).exists() {
                fs::rename(self.rotated(n), self.rotated(n + 1))?;
            }
        }
        fs::rename(self.live(), self.rotated(1))
    }
}

/// `value` with every control character in a string replaced by a space.
fn clean(value: &Value) -> Value {
    match value {
        Value::String(s) => Value::String(
            s.chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(clean).collect()),
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), clean(v))).collect())
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dir(tag: &str) -> PathBuf {
        crate::common::test_support::scratch_dir(tag)
    }

    fn count(dir: &Path) -> usize {
        fs::read_dir(dir).unwrap().count()
    }

    #[test]
    fn a_record_is_one_line() {
        let d = dir("watch-audit-line");
        let log = AuditLog::new(&d);
        log.append(&json!({"rule": "a", "outcome": "proposed"}))
            .unwrap();
        log.append(&json!({"rule": "b", "outcome": "skipped"}))
            .unwrap();
        let text = fs::read_to_string(d.join("audit.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 2);
    }

    #[test]
    fn a_terminal_escape_in_a_title_is_removed() {
        let d = dir("watch-audit-escape");
        let log = AuditLog::new(&d);
        log.append(&json!({"title": "\u{1b}[31mred\u{7}"})).unwrap();
        let text = fs::read_to_string(d.join("audit.jsonl")).unwrap();
        let body = text.trim_end_matches('\n');
        assert!(!body.chars().any(char::is_control), "{body:?}");
        assert!(body.contains("red"));
    }

    #[test]
    fn rotation_happens_only_when_a_write_passes_the_limit() {
        let record = json!({"k": "abcdef"});
        let line = record.to_string().len() as u64 + 1;
        // Two lines fit in exactly 2 * line bytes, and not in one byte less.
        for (limit, rotates) in [
            (2 * line + 1, false),
            (2 * line, false),
            (2 * line - 1, true),
        ] {
            // Arrange
            let d = dir(&format!("watch-audit-limit-{limit}"));
            let log = AuditLog::with_limit(&d, limit);
            log.append(&record).unwrap();

            // Act
            log.append(&record).unwrap();

            // Assert
            assert_eq!(d.join("audit.jsonl.1").exists(), rotates, "limit {limit}");
        }
    }

    #[test]
    fn at_most_five_files_remain_after_many_rotations() {
        let d = dir("watch-audit-five");
        let log = AuditLog::with_limit(&d, 10);
        for _ in 0..12 {
            log.append(&json!({"k": "abcdef"})).unwrap();
        }
        assert_eq!(count(&d), FILES as usize);
    }
}
