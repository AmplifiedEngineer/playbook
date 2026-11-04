// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A deterministic corpus for recall tests: the fixture facts plus generated
//! distractors that reuse the fixture's own vocabulary, so they are
//! confusable by construction. Everything here is pinned (a SplitMix64
//! generator and FNV-1a), so the same seed gives the same bytes on any
//! platform and Rust release.

use crate::memory_search_fixture::Fixture;
use std::collections::{BTreeMap, BTreeSet};

/// Words shorter than this are not worth drawing: the matcher ignores them.
const MIN_WORD_CHARS: usize = 4;
const DESCRIPTION_WORDS: usize = 7;
const BODY_WORDS: usize = 24;
const NAME_WORDS: usize = 2;

/// SplitMix64: a small generator whose output depends only on its seed.
pub(crate) struct SplitMix64(u64);

impl SplitMix64 {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }
}

/// FNV-1a over 64 bits. `DefaultHasher` is not stable across Rust releases,
/// so a golden hash cannot rest on it.
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xCBF2_9CE4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01B3)
    })
}

/// Fact id to file text, in id order.
pub(crate) struct Corpus {
    pub(crate) facts: BTreeMap<String, String>,
}

impl Corpus {
    /// The fixture facts plus enough distractors to reach `total` facts.
    pub(crate) fn build(fixture: &Fixture, total: usize, seed: u64) -> Self {
        let vocabulary = vocabulary(fixture);
        let mut rng = SplitMix64::new(seed);
        let mut facts = fixture.facts.clone();
        let mut serial = 0usize;
        while facts.len() < total {
            let id = distractor_id(&vocabulary, &mut rng, serial);
            serial += 1;
            let text = distractor_text(&id, &vocabulary, &mut rng);
            facts.insert(id, text);
        }
        Self { facts }
    }

    /// A hash of every fact id and text, for a golden test.
    pub(crate) fn hash(&self) -> u64 {
        let mut bytes = Vec::new();
        for (id, text) in &self.facts {
            bytes.extend_from_slice(id.as_bytes());
            bytes.push(0);
            bytes.extend_from_slice(text.as_bytes());
            bytes.push(0);
        }
        fnv1a(&bytes)
    }
}

fn vocabulary(fixture: &Fixture) -> Vec<String> {
    let words: BTreeSet<String> = fixture
        .facts
        .values()
        .flat_map(|text| text.split(|c: char| !c.is_ascii_alphabetic()))
        .filter(|w| w.len() >= MIN_WORD_CHARS)
        .map(str::to_ascii_lowercase)
        .collect();
    words.into_iter().collect()
}

fn pick<'a>(vocabulary: &'a [String], rng: &mut SplitMix64) -> &'a str {
    &vocabulary[rng.below(vocabulary.len())]
}

fn distractor_id(vocabulary: &[String], rng: &mut SplitMix64, serial: usize) -> String {
    let words: Vec<&str> = (0..NAME_WORDS).map(|_| pick(vocabulary, rng)).collect();
    format!("{}-{serial:05}", words.join("-"))
}

fn distractor_text(id: &str, vocabulary: &[String], rng: &mut SplitMix64) -> String {
    let mut sentence = |count: usize| {
        (0..count)
            .map(|_| pick(vocabulary, rng))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let description = sentence(DESCRIPTION_WORDS);
    let body = sentence(BODY_WORDS);
    format!("---\nname: {id}\ndescription: {description}\ntype: project\n---\n\n{body}\n")
}

/// The name and description lines of a fact file's frontmatter.
pub(crate) fn name_and_description(text: &str) -> (String, String) {
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .map(|v| v.trim().to_string())
            .unwrap_or_default()
    };
    (field("name:"), field("description:"))
}
