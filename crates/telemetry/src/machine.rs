// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The id and display name of this machine, kept in `machine.json` in the
//! data folder. The id is random, so it never derives from a path or hostname.

use crate::run::Paths;
use crate::util::atomic::{ensure_private_dir, write_atomic_private};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};

const FILE_NAME: &str = "machine.json";

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

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    pub id: String,
    /// The hostname when the id was made.
    pub hostname: String,
    name: Option<String>,
}

impl Machine {
    /// The name set with `--name`, or the creation hostname.
    pub fn name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.hostname)
    }

    pub fn text(&self) -> String {
        format!("id: {}\nname: {}", self.id, self.name())
    }
}

/// Reads this machine's record, making it with `generate` and `hostname` on the
/// first call. A file that cannot be read is an error and is never replaced.
pub fn load_or_create(
    dir: &Path,
    generate: impl FnOnce() -> IdBytes,
    hostname: &str,
) -> Result<Machine, String> {
    let path = file_path(dir);
    match std::fs::read_to_string(&path) {
        Ok(text) => parse(&path, &text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let machine = Machine {
                id: id_text(generate()),
                hostname: hostname.to_string(),
                name: None,
            };
            save(dir, &machine)?;
            Ok(machine)
        }
        Err(e) => Err(format!("failed to read {}: {e}", path.display())),
    }
}

/// Keeps `name` as the display name. An empty name restores the hostname.
pub fn rename(
    dir: &Path,
    name: &str,
    generate: impl FnOnce() -> IdBytes,
    hostname: &str,
) -> Result<Machine, String> {
    let mut machine = load_or_create(dir, generate, hostname)?;
    machine.name = Some(name.trim().to_string()).filter(|n| !n.is_empty());
    save(dir, &machine)?;
    Ok(machine)
}

/// The hostname stored at creation when it differs from `current`, such as
/// after a copied data folder or a renamed machine.
pub fn changed_hostname<'a>(machine: &'a Machine, current: &str) -> Option<&'a str> {
    (machine.hostname != current).then_some(machine.hostname.as_str())
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
    let hostname = current_hostname();
    let machine = match name {
        Some(name) => rename(dir, name, random_bytes, &hostname)?,
        None => load_or_create(dir, random_bytes, &hostname)?,
    };
    Ok(machine.text())
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

fn file_path(dir: &Path) -> PathBuf {
    dir.join(FILE_NAME)
}

fn parse(path: &Path, text: &str) -> Result<Machine, String> {
    let corrupt = |why: String| {
        format!(
            "{} is not a valid machine file ({why}); fix or remove it to make a new id",
            path.display()
        )
    };
    let machine: Machine = serde_json::from_str(text).map_err(|e| corrupt(e.to_string()))?;
    if !is_id_text(&machine.id) {
        return Err(corrupt("bad id: not a UUID".to_string()));
    }
    Ok(machine)
}

fn save(dir: &Path, machine: &Machine) -> Result<(), String> {
    let path = file_path(dir);
    let json = serde_json::to_string_pretty(machine)
        .map_err(|e| format!("failed to encode the machine record: {e}"))?;
    ensure_private_dir(dir).map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
    write_atomic_private(&path, json)
        .map_err(|e| format!("failed to write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXED: IdBytes = 0x1234_5678_9abc_4def_8123_4567_89ab_cdef_u128.to_be_bytes();
    const FIXED_TEXT: &str = "12345678-9abc-4def-8123-456789abcdef";

    fn dir(tag: &str) -> PathBuf {
        crate::util::scratch_dir(tag)
    }

    fn never() -> IdBytes {
        panic!("the id exists, so no new one is made")
    }

    #[test]
    fn two_reads_give_the_same_id() {
        // Arrange
        let d = dir("machine-twice");
        // Act
        let first = load_or_create(&d, || FIXED, "host-a").unwrap();
        let second = load_or_create(&d, never, "host-a").unwrap();
        // Assert
        assert_eq!(first, second);
    }

    #[test]
    fn a_fixed_generator_gives_a_v4_shaped_id_named_after_the_host() {
        // Arrange
        let d = dir("machine-shape");
        // Act
        let machine = load_or_create(&d, || FIXED, "host-a").unwrap();
        // Assert
        assert_eq!(machine.id, FIXED_TEXT);
        assert_eq!(machine.name(), "host-a");
        assert_eq!(machine.text(), format!("id: {FIXED_TEXT}\nname: host-a"));
    }

    #[test]
    fn the_real_generator_makes_a_random_v4_id() {
        // Arrange
        let d = dir("machine-random");
        // Act
        let machine = load_or_create(&d, random_bytes, "host-a").unwrap();
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
    fn a_corrupt_file_is_an_error_and_is_left_alone() {
        // Arrange
        let d = dir("machine-corrupt");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(file_path(&d), "{not json").unwrap();
        // Act
        let err = load_or_create(&d, || FIXED, "host-a").unwrap_err();
        // Assert
        assert!(err.contains("not a valid machine file"), "{err}");
        assert_eq!(std::fs::read_to_string(file_path(&d)).unwrap(), "{not json");
    }

    #[test]
    fn a_file_with_a_bad_id_is_an_error() {
        // Arrange
        let d = dir("machine-bad-id");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(file_path(&d), r#"{"id":"abc","hostname":"h","name":null}"#).unwrap();
        // Act
        let err = load_or_create(&d, || FIXED, "host-a").unwrap_err();
        // Assert
        assert!(err.contains("bad id"), "{err}");
    }

    #[test]
    fn a_new_name_persists_and_the_id_stays() {
        // Arrange
        let d = dir("machine-rename");
        let before = load_or_create(&d, || FIXED, "host-a").unwrap();
        // Act
        rename(&d, " laptop ", never, "host-a").unwrap();
        let after = load_or_create(&d, never, "host-a").unwrap();
        // Assert
        assert_eq!(after.name(), "laptop");
        assert_eq!(after.id, before.id);
    }

    #[test]
    fn an_empty_name_restores_the_hostname() {
        // Arrange
        let d = dir("machine-clear");
        rename(&d, "laptop", || FIXED, "host-a").unwrap();
        // Act
        let machine = rename(&d, "", never, "host-a").unwrap();
        // Assert
        assert_eq!(machine.name(), "host-a");
    }

    #[test]
    fn the_id_ignores_the_hostname_and_the_folder() {
        // Arrange
        let (a, b) = (dir("machine-path-a"), dir("machine-path-b"));
        // Act
        let first = load_or_create(&a, random_bytes, "host-a").unwrap();
        let second = load_or_create(&b, random_bytes, "host-a").unwrap();
        let moved = load_or_create(&a, never, "host-b").unwrap();
        // Assert
        assert_ne!(first.id, second.id);
        assert_eq!(moved.id, first.id);
    }

    #[test]
    fn a_different_current_hostname_is_reported() {
        // Arrange
        let machine = load_or_create(&dir("machine-host"), || FIXED, "host-a").unwrap();
        // Act and Assert
        assert_eq!(changed_hostname(&machine, "host-a"), None);
        assert_eq!(changed_hostname(&machine, "host-b"), Some("host-a"));
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        // Arrange
        let d = dir("machine-mode");
        // Act
        load_or_create(&d, || FIXED, "host-a").unwrap();
        // Assert
        let mode = std::fs::metadata(file_path(&d))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn the_command_prints_the_id_and_applies_a_name() {
        // Arrange
        let mut paths = Paths::from_home(&dir("machine-cmd"));
        paths.db = dir("machine-cmd-data").join("usage.db");
        // Act
        let shown = run(&paths, None).unwrap();
        let renamed = run(&paths, Some("laptop")).unwrap();
        // Assert
        let id_line = shown.lines().next().unwrap();
        assert!(renamed.starts_with(id_line), "{renamed}");
        assert!(renamed.ends_with("name: laptop"), "{renamed}");
    }
}
