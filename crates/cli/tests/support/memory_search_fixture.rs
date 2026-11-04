// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Loads and checks the synthetic memory search fixture: fact files and a
//! query list with a tune and a score partition. Shared by the tests that
//! measure recall.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::Path;

/// The fixture format this loader reads.
pub(crate) const SUPPORTED_VERSION: u32 = 1;

/// Fewest queries the score partition may hold. Below it a recall comparison
/// has too little data to say anything.
pub(crate) const MIN_SCORE_QUERIES: usize = 40;

const VERSION_KEY: &str = "version";
const FIELD_SEPARATOR: char = '\t';
const ID_SEPARATOR: char = ',';
const QUERY_FIELDS: usize = 4;
const FACT_EXTENSION: &str = "md";
const FACTS_DIR: &str = "facts";
const QUERIES_FILE: &str = "queries.tsv";

/// Which partition a query belongs to. Weights are tuned on `Tune` only and
/// reported on `Score`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Split {
    Tune,
    Score,
}

/// One query and the fact ids that count as a hit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Query {
    pub(crate) id: String,
    pub(crate) split: Split,
    /// Any one of these ids is a hit.
    pub(crate) expected: Vec<String>,
    pub(crate) text: String,
}

/// A checked fixture: fact id to file text, and the queries in file order.
#[derive(Debug)]
pub(crate) struct Fixture {
    pub(crate) facts: BTreeMap<String, String>,
    pub(crate) queries: Vec<Query>,
}

/// Why a fixture was refused.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FixtureError {
    /// A query expects a fact id that no fact file defines.
    UnknownExpectedId { query: String, id: String },
    /// A file holds a carriage return.
    Crlf { file: String },
    /// A file holds a character outside ASCII, which also rules out text
    /// that is not in Unicode normal form.
    NonAscii { file: String },
    /// A query names neither partition.
    UnknownSplit { query: String, split: String },
    /// The version line is missing, unreadable or not supported.
    BadVersion(String),
    /// A query line is malformed.
    BadQueryLine { line: usize },
    /// Two queries share an id.
    DuplicateQuery(String),
    /// The score partition is smaller than [`MIN_SCORE_QUERIES`].
    ScoreTooSmall { found: usize },
    /// The folder could not be read.
    Io(String),
}

impl fmt::Display for FixtureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownExpectedId { query, id } => {
                write!(f, "query {query} expects unknown fact id {id}")
            }
            Self::Crlf { file } => write!(f, "{file} holds a carriage return"),
            Self::NonAscii { file } => write!(f, "{file} holds a non-ASCII character"),
            Self::UnknownSplit { query, split } => {
                write!(f, "query {query} has unknown split {split:?}")
            }
            Self::BadVersion(why) => write!(f, "bad fixture version: {why}"),
            Self::BadQueryLine { line } => write!(f, "queries.tsv line {line} is malformed"),
            Self::DuplicateQuery(id) => write!(f, "query id {id} appears twice"),
            Self::ScoreTooSmall { found } => write!(
                f,
                "score partition has {found} queries, need at least {MIN_SCORE_QUERIES}"
            ),
            Self::Io(why) => write!(f, "could not read fixture: {why}"),
        }
    }
}

impl Fixture {
    /// Checks `facts` (id and file text) and `queries_tsv`.
    pub(crate) fn parse(
        facts: &[(String, String)],
        queries_tsv: &str,
    ) -> Result<Self, FixtureError> {
        check_text(QUERIES_FILE, queries_tsv)?;
        let mut fact_map = BTreeMap::new();
        for (id, text) in facts {
            check_text(&format!("{id}.{FACT_EXTENSION}"), text)?;
            fact_map.insert(id.clone(), text.clone());
        }
        let mut lines = queries_tsv.lines();
        check_version(lines.next())?;
        let mut queries = Vec::new();
        let mut seen = BTreeSet::new();
        for (n, line) in lines.enumerate() {
            let query = parse_query(line, n + 2, &fact_map)?;
            if !seen.insert(query.id.clone()) {
                return Err(FixtureError::DuplicateQuery(query.id));
            }
            queries.push(query);
        }
        let found = queries.iter().filter(|q| q.split == Split::Score).count();
        if found < MIN_SCORE_QUERIES {
            return Err(FixtureError::ScoreTooSmall { found });
        }
        Ok(Self {
            facts: fact_map,
            queries,
        })
    }

    /// Reads `<dir>/facts/*.md` and `<dir>/queries.tsv`.
    pub(crate) fn load(dir: &Path) -> Result<Self, FixtureError> {
        let io = |e: std::io::Error| FixtureError::Io(e.to_string());
        let mut facts = Vec::new();
        for entry in fs::read_dir(dir.join(FACTS_DIR)).map_err(io)? {
            let path = entry.map_err(io)?.path();
            if path.extension().and_then(|e| e.to_str()) != Some(FACT_EXTENSION) {
                continue;
            }
            let id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            facts.push((id, fs::read_to_string(&path).map_err(io)?));
        }
        let queries = fs::read_to_string(dir.join(QUERIES_FILE)).map_err(io)?;
        Self::parse(&facts, &queries)
    }
}

fn check_text(file: &str, text: &str) -> Result<(), FixtureError> {
    if text.contains('\r') {
        return Err(FixtureError::Crlf {
            file: file.to_string(),
        });
    }
    if !text.is_ascii() {
        return Err(FixtureError::NonAscii {
            file: file.to_string(),
        });
    }
    Ok(())
}

fn check_version(line: Option<&str>) -> Result<(), FixtureError> {
    let line = line.ok_or_else(|| FixtureError::BadVersion("missing".to_string()))?;
    let (key, value) = line
        .split_once(FIELD_SEPARATOR)
        .ok_or_else(|| FixtureError::BadVersion(format!("not a version line: {line:?}")))?;
    if key != VERSION_KEY {
        return Err(FixtureError::BadVersion(format!("first line is {key:?}")));
    }
    match value.parse::<u32>() {
        Ok(SUPPORTED_VERSION) => Ok(()),
        Ok(other) => Err(FixtureError::BadVersion(format!("unsupported {other}"))),
        Err(_) => Err(FixtureError::BadVersion(format!("not a number: {value:?}"))),
    }
}

fn parse_query(
    line: &str,
    number: usize,
    facts: &BTreeMap<String, String>,
) -> Result<Query, FixtureError> {
    let fields: Vec<&str> = line.split(FIELD_SEPARATOR).collect();
    let [id, split, expected, text] = fields[..] else {
        debug_assert_ne!(fields.len(), QUERY_FIELDS);
        return Err(FixtureError::BadQueryLine { line: number });
    };
    if id.is_empty() || expected.is_empty() || text.trim().is_empty() {
        return Err(FixtureError::BadQueryLine { line: number });
    }
    let split = match split {
        "tune" => Split::Tune,
        "score" => Split::Score,
        other => {
            return Err(FixtureError::UnknownSplit {
                query: id.to_string(),
                split: other.to_string(),
            })
        }
    };
    let expected: Vec<String> = expected.split(ID_SEPARATOR).map(str::to_string).collect();
    if let Some(missing) = expected.iter().find(|e| !facts.contains_key(*e)) {
        return Err(FixtureError::UnknownExpectedId {
            query: id.to_string(),
            id: missing.clone(),
        });
    }
    Ok(Query {
        id: id.to_string(),
        split,
        expected,
        text: text.to_string(),
    })
}
