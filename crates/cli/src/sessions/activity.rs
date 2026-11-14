// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The per session file that older builds of the `session-activity` hook wrote
//! (ADR-0024, stage 3): whether a turn was running and the last waiting notice.
//! Nothing writes it now. It is read back, only when it is a plain file the same
//! user owns and stays under [`MAX_BYTES`], to import it into `sessions.db`.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Read;
use std::path::Path;

const FILE: &str = "activity.json";
/// The most bytes read back. A real file is about 60.
const MAX_BYTES: u64 = 512;

/// Whether a turn was running.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Turn {
    Working,
    #[default]
    Idle,
}

#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Activity {
    pub(crate) turn: Turn,
    /// The last waiting notice since the turn began, one of three fixed words.
    pub(crate) notice: Option<String>,
    /// Epoch seconds of the event.
    pub(crate) ts: u64,
}

/// The file in `dir`, or `None` when it is missing, not a plain file, too
/// large, owned by someone else, or not valid.
#[allow(dead_code)] // The import that reads the old file is not wired yet.
pub(crate) fn read(dir: &Path) -> Option<Activity> {
    let path = dir.join(FILE);
    let meta = fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_BYTES || !same_owner(&meta, dir) {
        return None;
    }
    let mut text = String::new();
    fs::File::open(&path)
        .ok()?
        .take(MAX_BYTES)
        .read_to_string(&mut text)
        .ok()?;
    serde_json::from_str(&text).ok()
}

/// Whether the file belongs to the user that owns its folder. The folder is
/// created owner only, so another user cannot have put a file there. Windows
/// has no uid, and the folder sits in the user's own profile.
#[cfg(unix)]
fn same_owner(file: &fs::Metadata, dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(dir).is_ok_and(|d| d.uid() == file.uid())
}

#[cfg(not(unix))]
fn same_owner(_file: &fs::Metadata, _dir: &Path) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = crate::common::test_support::scratch_dir(name);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn an_oversized_or_invalid_file_reads_as_missing() {
        let d = dir("activity-bad");
        fs::write(d.join(FILE), "x".repeat(MAX_BYTES as usize + 1)).unwrap();
        assert_eq!(read(&d), None);
        fs::write(d.join(FILE), "not json").unwrap();
        assert_eq!(read(&d), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_file_is_not_followed() {
        let d = dir("activity-link");
        let target = d.join("elsewhere.json");
        fs::write(&target, r#"{"turn":"working","notice":null,"ts":1}"#).unwrap();
        std::os::unix::fs::symlink(&target, d.join(FILE)).unwrap();
        assert_eq!(read(&d), None);
    }
}
