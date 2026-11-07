// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Salted hashes for the values that must not leave the machine in the clear.

use std::collections::hash_map::RandomState;
use std::fmt::Write;
use std::hash::{BuildHasher, Hasher};

use sha2::{Digest, Sha256};

/// The hashed bytes kept: 128 bits.
const HASH_BYTES: usize = 16;
const SALT_BYTES: usize = 16;
const WORD_BYTES: usize = 8;

/// What kind of value is hashed, so the same text hashes differently per kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    Account,
    Repo,
}

impl Domain {
    const fn tag(self) -> &'static str {
        match self {
            Self::Account => "account",
            Self::Repo => "repo",
        }
    }
}

/// Lowercase hex of the first 16 bytes of SHA-256 over the domain tag, the salt
/// and the value, each as a 4 byte big-endian length followed by its bytes.
pub fn salted_hash(salt: &str, domain: Domain, value: &str) -> String {
    let mut hasher = Sha256::new();
    for field in [domain.tag(), salt, value] {
        let length = u32::try_from(field.len()).unwrap_or(u32::MAX);
        hasher.update(length.to_be_bytes());
        hasher.update(field.as_bytes());
    }
    to_hex(&hasher.finalize()[..HASH_BYTES])
}

/// A fresh salt: 16 random bytes as hex. std seeds every `RandomState` from the
/// operating system, so no random-number crate is needed.
pub fn random_salt() -> String {
    let mut bytes = [0u8; SALT_BYTES];
    for chunk in bytes.chunks_mut(WORD_BYTES) {
        let word = RandomState::new().build_hasher().finish();
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    to_hex(&bytes)
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SALT: &str = "pepper";
    const VALUE: &str = "dev@example.com";

    #[test]
    fn the_hash_is_32_lowercase_hex_chars_and_hides_the_value() {
        // Arrange / Act
        let hash = salted_hash(SALT, Domain::Account, VALUE);
        // Assert
        assert_eq!(hash.len(), 32);
        assert!(hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        assert!(!hash.contains("example"));
    }

    #[test]
    fn the_hash_is_deterministic() {
        // Arrange / Act
        let (a, b) = (
            salted_hash(SALT, Domain::Repo, VALUE),
            salted_hash(SALT, Domain::Repo, VALUE),
        );
        // Assert
        assert_eq!(a, b);
    }

    #[test]
    fn another_salt_or_domain_gives_another_hash() {
        // Arrange
        let base = salted_hash(SALT, Domain::Account, VALUE);
        // Act
        let other_salt = salted_hash("other", Domain::Account, VALUE);
        let other_domain = salted_hash(SALT, Domain::Repo, VALUE);
        // Assert
        assert_ne!(base, other_salt);
        assert_ne!(base, other_domain);
    }

    #[test]
    fn the_hash_matches_a_known_answer_over_the_documented_layout() {
        // Arrange
        let mut bytes = Vec::new();
        for field in ["account", SALT, VALUE] {
            bytes.extend_from_slice(&(field.len() as u32).to_be_bytes());
            bytes.extend_from_slice(field.as_bytes());
        }
        let digest = Sha256::digest(&bytes);
        let expected: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
        // Act
        let hash = salted_hash(SALT, Domain::Account, VALUE);
        // Assert
        assert_eq!(hash, expected);
    }

    #[test]
    fn length_prefixes_stop_fields_from_blurring() {
        // Arrange / Act
        let a = salted_hash("ab", Domain::Repo, "c");
        let b = salted_hash("a", Domain::Repo, "bc");
        // Assert
        assert_ne!(a, b);
    }

    #[test]
    fn random_salts_are_32_hex_chars_and_differ() {
        // Arrange / Act
        let (a, b) = (random_salt(), random_salt());
        // Assert
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }
}
