// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The id and display name of this machine, kept in the one-row `machine`
//! table of `usage.db`. The id is random, so it never derives from a path or
//! hostname. An older build kept them in `machine.json` beside the database;
//! that file is read once to carry its id over and is never changed or deleted.

use crate::run::Paths;
use playbook_store::{params, Connection, OptionalExtension};
use serde::Deserialize;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::Path;

const LEGACY_FILE_NAME: &str = "machine.json";

/// The table holds at most one row, enforced by the `id = 1` check.
pub const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS machine (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    machine_id TEXT NOT NULL,
    name TEXT
);";

/// The random bytes in a version 4 UUID.
pub type IdBytes = [u8; 16];

const VERSION_BYTE: usize = 6;
const VERSION_MASK: u8 = 0x0f;
const VERSION_4: u8 = 0x40;
const VARIANT_BYTE: usize = 8;
const VARIANT_MASK: u8 = 0x3f;
const VARIANT_RFC_4122: u8 = 0x80;
/// The hex digits in each dash-separated group of the id text.
const GROUP_LENGTHS: [usize; 5] = [8, 4, 4, 4, 12];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    pub id: String,
    /// The name set with `--name`; `None` means the hostname is shown.
    pub name: Option<String>,
}

impl Machine {
    /// The name set with `--name`, else `hostname`.
    pub fn name_or<'a>(&'a self, hostname: &'a str) -> &'a str {
        self.name.as_deref().unwrap_or(hostname)
    }

    pub fn text(&self, hostname: &str) -> String {
        format!("id: {}\nname: {}", self.id, self.name_or(hostname))
    }
}

/// The shape of the legacy `machine.json`.
#[derive(Deserialize)]
struct LegacyMachine {
    id: String,
    hostname: String,
    name: Option<String>,
}

/// Reads this machine's row, making it on the first call: from a valid legacy
/// `machine.json` in `dir` when there is one, else with an id from `generate`.
pub fn load_or_create(
    conn: &Connection,
    dir: &Path,
    generate: impl FnOnce() -> IdBytes,
) -> Result<Machine, String> {
    if let Some(machine) = stored(conn)? {
        return Ok(machine);
    }
    let machine = read_legacy(dir).unwrap_or_else(|| Machine {
        id: id_text(generate()),
        name: None,
    });
    // OR IGNORE: a concurrent opener may have won; the re-read returns its row.
    conn.execute(
        "INSERT OR IGNORE INTO machine (id, machine_id, name) VALUES (1, ?1, ?2)",
        params![machine.id, machine.name],
    )
    .map_err(|e| format!("failed to store the machine id: {e}"))?;
    stored(conn)?.ok_or_else(|| "the machine row is missing after it was stored".to_string())
}

/// The row of this machine, `None` before it is made.
pub fn stored(conn: &Connection) -> Result<Option<Machine>, String> {
    conn.query_row(
        "SELECT machine_id, name FROM machine WHERE id = 1",
        [],
        |r| {
            Ok(Machine {
                id: r.get(0)?,
                name: r.get(1)?,
            })
        },
    )
    .optional()
    .map_err(|e| format!("failed to read the machine row: {e}"))
}

/// Keeps `name` as the display name. An empty name restores the hostname.
pub fn rename(
    conn: &Connection,
    dir: &Path,
    name: &str,
    generate: impl FnOnce() -> IdBytes,
) -> Result<Machine, String> {
    let mut machine = load_or_create(conn, dir, generate)?;
    machine.name = Some(name.trim().to_string()).filter(|n| !n.is_empty());
    conn.execute(
        "UPDATE machine SET name = ?1 WHERE id = 1",
        params![machine.name],
    )
    .map_err(|e| format!("failed to store the machine name: {e}"))?;
    Ok(machine)
}

/// The hostname of this computer.
pub fn current_hostname() -> String {
    #[cfg(unix)]
    {
        rustix::system::uname()
            .nodename()
            .to_string_lossy()
            .into_owned()
    }
    #[cfg(not(unix))]
    {
        std::env::var("COMPUTERNAME").unwrap_or_default()
    }
}

/// The text of `playbook usage machine`.
pub fn run(paths: &Paths, name: Option<&str>) -> Result<String, String> {
    let dir = paths
        .db
        .parent()
        .ok_or_else(|| format!("{} has no data folder", paths.db.display()))?;
    let conn = crate::db::open_db(&paths.db)?;
    let machine = match name {
        Some(name) => rename(&conn, dir, name, random_bytes)?,
        None => load_or_create(&conn, dir, random_bytes)?,
    };
    Ok(machine.text(&current_hostname()))
}

/// 16 random bytes. std seeds every `RandomState` with keys from the operating
/// system's random source, so each one yields 8 unpredictable bytes. This
/// avoids a platform-specific random API, since rustix has none on macOS.
pub fn random_bytes() -> IdBytes {
    let mut bytes = [0u8; 16];
    for chunk in bytes.chunks_mut(8) {
        let word = RandomState::new().build_hasher().finish();
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    bytes
}

/// The version 4 UUID text for `bytes`, with the version and variant bits set.
fn id_text(mut bytes: IdBytes) -> String {
    bytes[VERSION_BYTE] = (bytes[VERSION_BYTE] & VERSION_MASK) | VERSION_4;
    bytes[VARIANT_BYTE] = (bytes[VARIANT_BYTE] & VARIANT_MASK) | VARIANT_RFC_4122;
    let mut hex = bytes.iter().map(|b| format!("{b:02x}"));
    let groups: Vec<String> = GROUP_LENGTHS
        .iter()
        .map(|digits| hex.by_ref().take(digits / 2).collect())
        .collect();
    groups.join("-")
}

fn is_id_text(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == GROUP_LENGTHS.len()
        && groups.iter().zip(GROUP_LENGTHS).all(|(group, digits)| {
            group.len() == digits && group.bytes().all(|b| b.is_ascii_hexdigit())
        })
}

/// The machine in `dir/machine.json`, `None` when the file is missing, cannot be
/// parsed or holds a bad id: the row is then made fresh and the file is ignored.
/// Its hostname is dropped, and a name equal to it was only the default.
fn read_legacy(dir: &Path) -> Option<Machine> {
    let text = std::fs::read_to_string(dir.join(LEGACY_FILE_NAME)).ok()?;
    let legacy: LegacyMachine = serde_json::from_str(&text).ok()?;
    let name = legacy
        .name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty() && *n != legacy.hostname);
    is_id_text(&legacy.id).then_some(Machine {
        id: legacy.id,
        name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const FIXED: IdBytes = 0x1234_5678_9abc_4def_8123_4567_89ab_cdef_u128.to_be_bytes();
    const FIXED_TEXT: &str = "12345678-9abc-4def-8123-456789abcdef";
    const LEGACY_ID: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    const LEGACY_JSON: &str =
        r#"{"id":"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee","hostname":"old-host","name":"old-name"}"#;

    fn dir(tag: &str) -> PathBuf {
        crate::util::scratch_dir(tag)
    }

    fn table() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA_SQL).unwrap();
        conn
    }

    fn legacy_file(dir: &Path, text: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(LEGACY_FILE_NAME);
        std::fs::write(&path, text).unwrap();
        path
    }

    fn never() -> IdBytes {
        panic!("the id exists, so no new one is made")
    }

    #[test]
    fn two_reads_give_the_same_id() {
        // Arrange
        let (conn, d) = (table(), dir("machine-twice"));
        // Act
        let first = load_or_create(&conn, &d, || FIXED).unwrap();
        let second = load_or_create(&conn, &d, never).unwrap();
        // Assert
        assert_eq!(first, second);
    }

    #[test]
    fn a_missing_file_and_an_empty_table_make_an_id_with_no_name() {
        // Arrange
        let (conn, d) = (table(), dir("machine-shape"));
        // Act
        let machine = load_or_create(&conn, &d, || FIXED).unwrap();
        // Assert
        assert_eq!(machine.id, FIXED_TEXT);
        assert_eq!(machine.name, None);
        assert_eq!(
            machine.text("host-a"),
            format!("id: {FIXED_TEXT}\nname: host-a")
        );
        assert!(
            !d.join(LEGACY_FILE_NAME).exists(),
            "no legacy file is written"
        );
    }

    #[test]
    fn the_real_generator_makes_a_random_v4_id() {
        // Arrange
        let (conn, d) = (table(), dir("machine-random"));
        // Act
        let machine = load_or_create(&conn, &d, random_bytes).unwrap();
        // Assert
        assert!(is_id_text(&machine.id), "{}", machine.id);
        assert_eq!(machine.id.as_bytes()[14], b'4');
    }

    #[test]
    fn the_text_sets_the_version_and_variant_bits_and_the_shape() {
        // Arrange
        let all_ones = [0xff; 16];
        let all_zeros = [0x00; 16];
        // Act
        let ones = id_text(all_ones);
        let zeros = id_text(all_zeros);
        // Assert
        assert_eq!(ones, "ffffffff-ffff-4fff-bfff-ffffffffffff");
        assert_eq!(zeros, "00000000-0000-4000-8000-000000000000");
        assert_eq!(id_text(FIXED), FIXED_TEXT);
    }

    #[test]
    fn a_legacy_file_is_copied_into_the_table_and_left_byte_identical() {
        // Arrange
        let (conn, d) = (table(), dir("machine-legacy"));
        let path = legacy_file(&d, LEGACY_JSON);
        // Act
        let machine = load_or_create(&conn, &d, never).unwrap();
        // Assert
        assert_eq!(machine.id, LEGACY_ID);
        assert_eq!(machine.name.as_deref(), Some("old-name"));
        assert_eq!(stored(&conn).unwrap(), Some(machine));
        assert_eq!(std::fs::read_to_string(path).unwrap(), LEGACY_JSON);
    }

    #[test]
    fn a_legacy_name_equal_to_its_hostname_is_not_a_set_name() {
        // Arrange
        let (conn, d) = (table(), dir("machine-legacy-unnamed"));
        legacy_file(
            &d,
            r#"{"id":"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee","hostname":"old-host","name":"old-host"}"#,
        );
        // Act
        let machine = load_or_create(&conn, &d, never).unwrap();
        // Assert
        assert_eq!(machine.name, None);
    }

    #[test]
    fn the_table_wins_over_a_legacy_file_that_differs() {
        // Arrange
        let (conn, d) = (table(), dir("machine-table-wins"));
        let before = load_or_create(&conn, &d, || FIXED).unwrap();
        let path = legacy_file(&d, LEGACY_JSON);
        // Act
        let after = load_or_create(&conn, &d, never).unwrap();
        // Assert
        assert_eq!(after, before);
        assert_eq!(std::fs::read_to_string(path).unwrap(), LEGACY_JSON);
    }

    #[test]
    fn a_corrupt_file_and_an_empty_table_make_a_new_id_and_leave_the_file() {
        // Arrange
        let (conn, d) = (table(), dir("machine-corrupt"));
        let path = legacy_file(&d, "{not json");
        // Act
        let machine = load_or_create(&conn, &d, || FIXED).unwrap();
        // Assert
        assert_eq!(machine.id, FIXED_TEXT);
        assert_eq!(std::fs::read_to_string(path).unwrap(), "{not json");
    }

    #[test]
    fn a_corrupt_file_is_ignored_when_the_table_has_its_row() {
        // Arrange
        let (conn, d) = (table(), dir("machine-corrupt-row"));
        let before = load_or_create(&conn, &d, || FIXED).unwrap();
        legacy_file(&d, "{not json");
        // Act
        let after = load_or_create(&conn, &d, never).unwrap();
        // Assert
        assert_eq!(after, before);
    }

    #[test]
    fn a_file_with_a_bad_id_is_ignored() {
        // Arrange
        let (conn, d) = (table(), dir("machine-bad-id"));
        legacy_file(&d, r#"{"id":"abc","hostname":"h","name":null}"#);
        // Act
        let machine = load_or_create(&conn, &d, || FIXED).unwrap();
        // Assert
        assert_eq!(machine.id, FIXED_TEXT);
    }

    #[test]
    fn a_new_name_persists_in_the_table_and_the_id_stays() {
        // Arrange
        let (conn, d) = (table(), dir("machine-rename"));
        let before = load_or_create(&conn, &d, || FIXED).unwrap();
        // Act
        rename(&conn, &d, " laptop ", never).unwrap();
        let after = load_or_create(&conn, &d, never).unwrap();
        // Assert
        assert_eq!(after.name.as_deref(), Some("laptop"));
        assert_eq!(after.id, before.id);
        assert!(
            !d.join(LEGACY_FILE_NAME).exists(),
            "no legacy file is written"
        );
    }

    #[test]
    fn an_empty_name_clears_the_name() {
        // Arrange
        let (conn, d) = (table(), dir("machine-clear"));
        rename(&conn, &d, "laptop", || FIXED).unwrap();
        // Act
        let machine = rename(&conn, &d, "", never).unwrap();
        // Assert
        assert_eq!(machine.name, None);
        assert_eq!(stored(&conn).unwrap().unwrap().name, None);
    }

    #[test]
    fn the_id_ignores_the_hostname_and_the_folder() {
        // Arrange
        let (a, b) = (table(), table());
        let d = dir("machine-path");
        // Act
        let first = load_or_create(&a, &d, random_bytes).unwrap();
        let second = load_or_create(&b, &d, random_bytes).unwrap();
        let moved = load_or_create(&a, &d, never).unwrap();
        // Assert
        assert_ne!(first.id, second.id);
        assert_eq!(moved.id, first.id);
    }

    #[test]
    fn the_command_prints_the_id_and_applies_a_name() {
        // Arrange
        let mut paths = Paths::from_home(&dir("machine-cmd"));
        paths.db = dir("machine-cmd-data").join("usage.db");
        // Act
        let shown = run(&paths, None).unwrap();
        let renamed = run(&paths, Some("laptop")).unwrap();
        let again = run(&paths, None).unwrap();
        // Assert
        let id_line = shown.lines().next().unwrap();
        assert!(renamed.starts_with(id_line), "{renamed}");
        assert!(renamed.ends_with("name: laptop"), "{renamed}");
        assert_eq!(again, renamed);
    }

    #[test]
    fn the_command_succeeds_beside_a_corrupt_legacy_file() {
        // Arrange
        let d = dir("machine-cmd-corrupt");
        legacy_file(&d, "{not json");
        let mut paths = Paths::from_home(&dir("machine-cmd-corrupt-home"));
        paths.db = d.join("usage.db");
        // Act
        let shown = run(&paths, None).unwrap();
        // Assert
        assert!(shown.starts_with("id: "), "{shown}");
    }
}
