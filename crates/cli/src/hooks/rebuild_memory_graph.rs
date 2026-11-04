// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! PostToolUse hook: rebuild the memory graph in `~/.config/playbook/data/memory.db`
//! after any fact-file save. Ports the retired shell original. No-op unless the
//! edited file is inside `~/.config/playbook/memory`. Walks the whole memory tree
//! (not incremental), replaces the graph in one transaction, and emits nothing
//! on stdout. The fact files stay the source of truth. Older versions wrote
//! `memory.graph.json`; [`migrate_json_graph`] folds such a file in.

use crate::common::atomic::{acquire_dir_lock_aged, with_dir_lock_aged, LONG_LOCK_STALE_AGE};
use crate::common::home_dir;
use crate::common::paths::memory_dir;
use crate::common::payload::Payload;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Everything that can stop a rebuild before it produces a new
/// graph, one variant's worth of detail flattened into a single
/// string. Mirrors `settings::gen::GenError`'s struct-plus-`Display` shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuildError(pub String);

impl std::fmt::Display for RebuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

pub fn run(payload: &Payload) {
    if should_skip(payload) {
        return;
    }
    if let Err(err) = rebuild() {
        eprintln!("rebuild-memory-graph: {err}");
    }
}

/// Mirror the bash/python guard: skip the rebuild unless the edited file's
/// path (after expanding a leading `~`) is inside `MEMORY_DIR`.
///
/// Divergence from python: the retired shell original's `_should_skip`
/// reads its own raw stdin (or `HOOK_INPUT`) and treats completely empty
/// input as a signal to always rebuild, distinct from a non-empty payload
/// that merely lacks `tool_input.file_path` (which it skips). `main.rs`
/// already consumes that raw input before calling `Payload::parse`, and only
/// the parsed `Payload` reaches this hook: an empty raw string, a bare `{}`,
/// and malformed JSON all collapse to the identical empty object, so those
/// three cases are indistinguishable from inside this function. This port
/// always skips when `file_path` is missing or outside `MEMORY_DIR`, which
/// matches 2 of python's 3 branches (the non-empty-but-fieldless case and
/// the malformed-JSON case) and never triggers an unwanted full-tree
/// rebuild; only the "truly no input at all" manual-invocation case behaves
/// differently, and it is not exercised by rebuild-memory-graph.test.sh.
fn should_skip(payload: &Payload) -> bool {
    let raw_path = payload.field(".tool_input.file_path");
    let file_path = expand_tilde(&raw_path);
    if file_path.is_empty() {
        return true;
    }
    let mem_dir = memory_dir().to_string_lossy().into_owned();
    !file_path.starts_with(&mem_dir)
}

fn expand_tilde(path: &str) -> String {
    match path.strip_prefix('~') {
        Some(rest) => format!("{}{rest}", home_dir().to_string_lossy()),
        None => path.to_string(),
    }
}

/// Return the fact's markdown body: everything after the closing `---` line
/// of its frontmatter block. Walks lines the same way
/// `extract_frontmatter_block` does (an opening `---` line, then lines up to
/// a closing `---` line that must itself be followed by another line), so
/// the two helpers agree on exactly where a frontmatter block ends. A file
/// with no valid frontmatter block has no body to strip, so the whole
/// content is returned unchanged.
fn extract_body(content: &str) -> &str {
    let mut lines = content.split('\n').peekable();
    let Some(first) = lines.next() else {
        return content;
    };
    if !is_delimiter_line(first) {
        return content;
    }
    let mut consumed = first.len() + 1;
    for line in lines.by_ref() {
        if is_delimiter_line(line) {
            consumed += line.len() + 1;
            return if lines.peek().is_some() {
                &content[consumed..]
            } else {
                content
            };
        }
        consumed += line.len() + 1;
    }
    content
}

// --- Frontmatter parsing (hand-rolled YAML subset, no yaml crate) ---------

/// A single top-level frontmatter value: a bare scalar, a block or inline
/// list, or a dict of sub-keys (each of which is itself a scalar or a
/// list). Mirrors the three shapes the retired shell original:
/// parse_frontmatter` can produce for a python dict value.
#[derive(Debug, Clone)]
enum TopValue {
    Scalar(String),
    List(Vec<String>),
    Dict(HashMap<String, SubValue>),
}

/// All top-level frontmatter values, keyed by name. Kept in one map, rather
/// than one map per shape, so a later top-level redeclaration of a key
/// evicts whatever shape the earlier declaration held, regardless of shape:
/// python's `parse_frontmatter` keeps a single `result` dict and gets this
/// for free, since a later `result[current_key] = ...` simply overwrites.
#[derive(Debug, Default, Clone)]
struct Frontmatter {
    values: HashMap<String, TopValue>,
}

impl Frontmatter {
    fn scalar(&self, key: &str) -> Option<&String> {
        match self.values.get(key) {
            Some(TopValue::Scalar(s)) => Some(s),
            _ => None,
        }
    }

    fn list(&self, key: &str) -> Option<&Vec<String>> {
        match self.values.get(key) {
            Some(TopValue::List(l)) => Some(l),
            _ => None,
        }
    }

    fn dict(&self, key: &str) -> Option<&HashMap<String, SubValue>> {
        match self.values.get(key) {
            Some(TopValue::Dict(d)) => Some(d),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
enum SubValue {
    Scalar(String),
    List(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    None,
    List,
    Dict,
}

/// Accumulator for the frontmatter state machine, one instance per file.
/// Fields mirror the nonlocal variables `parse_frontmatter`'s python
/// closures capture: `current_key`/`current_kind` track which top-level key
/// is being built, `buf_list`/`buf_dict` hold that key's in-progress value,
/// and `pending_key`/`pending_list` hold a dict sub-key whose value is a
/// block list still being read.
struct ParserState {
    result: Frontmatter,
    current_key: Option<String>,
    current_kind: Kind,
    buf_list: Vec<String>,
    buf_dict: HashMap<String, SubValue>,
    pending_key: Option<String>,
    pending_list: Vec<String>,
}

impl ParserState {
    fn new() -> Self {
        Self {
            result: Frontmatter::default(),
            current_key: None,
            current_kind: Kind::None,
            buf_list: Vec::new(),
            buf_dict: HashMap::new(),
            pending_key: None,
            pending_list: Vec::new(),
        }
    }

    fn flush_pending(&mut self) {
        if let Some(key) = self.pending_key.take() {
            self.buf_dict
                .insert(key, SubValue::List(std::mem::take(&mut self.pending_list)));
        }
    }

    /// Finalize whatever is buffered for `current_key`. A key that was
    /// opened but never accumulated a list or dict value (kind stayed
    /// `None`) is silently dropped, matching python: `flush()` only writes
    /// to `result` in the list/dict branches. The insert below evicts
    /// whatever value (of any shape) an earlier declaration of the same key
    /// left behind, matching python's single-dict overwrite semantics.
    fn flush(&mut self) {
        let Some(key) = self.current_key.take() else {
            return;
        };
        self.flush_pending();
        match self.current_kind {
            Kind::List => {
                self.result
                    .values
                    .insert(key, TopValue::List(std::mem::take(&mut self.buf_list)));
            }
            Kind::Dict => {
                self.result
                    .values
                    .insert(key, TopValue::Dict(std::mem::take(&mut self.buf_dict)));
            }
            Kind::None => {}
        }
        self.current_kind = Kind::None;
    }
}

/// Parse YAML frontmatter between `---` delimiters. Absent, unclosed, or
/// otherwise malformed frontmatter all yield an empty `Frontmatter`, never
/// an error: a fact file with no frontmatter still gets a node, using the
/// filename-derived defaults `rebuild()` applies below.
fn parse_frontmatter(content: &str) -> Frontmatter {
    let mut state = ParserState::new();
    let Some(fm) = extract_frontmatter_block(content) else {
        return state.result;
    };
    let fm = normalize_line_endings(&fm);

    for line in fm.split('\n') {
        if let Some((key, rest)) = match_top_level(line) {
            state.flush();
            state.current_key = Some(key.to_string());
            let val = rest.trim();
            if !val.is_empty() {
                let value = if val.starts_with('[') && val.ends_with(']') {
                    TopValue::List(parse_inline_list(val))
                } else {
                    TopValue::Scalar(val.to_string())
                };
                state.result.values.insert(key.to_string(), value);
                state.current_key = None;
            }
        } else if state.current_key.is_some() && state.pending_key.is_some() && is_block_item(line)
        {
            state.pending_list.push(extract_block_item(line));
        } else if state.current_key.is_some() && is_block_item(line) {
            state.current_kind = Kind::List;
            state.buf_list.push(extract_block_item(line));
        } else if state.current_key.is_some() && is_sub_kv_line(line) {
            if let Some((sub_key, sub_val)) = match_sub_kv(line) {
                state.flush_pending();
                state.current_kind = Kind::Dict;
                let sub_val = sub_val.trim();
                if sub_val.starts_with('[') && sub_val.ends_with(']') {
                    state.buf_dict.insert(
                        sub_key.to_string(),
                        SubValue::List(parse_inline_list(sub_val)),
                    );
                } else if !sub_val.is_empty() {
                    state
                        .buf_dict
                        .insert(sub_key.to_string(), SubValue::Scalar(sub_val.to_string()));
                } else {
                    state.pending_key = Some(sub_key.to_string());
                    state.pending_list = Vec::new();
                }
            }
        }
    }
    state.flush();
    state.result
}

/// The optional `aliases:` list of a fact file: other names a prompt may use
/// for the fact. Empty when the key is absent or the frontmatter is bad.
pub(crate) fn fact_aliases(content: &str) -> Vec<String> {
    parse_frontmatter(content)
        .list("aliases")
        .cloned()
        .unwrap_or_default()
}

/// Return the text strictly between a `---` opening line and the first
/// `---` closing line, both required to be a full line (only trailing
/// spaces/tabs allowed) and the closing line must itself be followed by
/// another line, matching python's `^---[ \t]*\n(.*?)\n---[ \t]*\n` (DOTALL,
/// anchored at the start of `content`). Returns `None` when the delimiters
/// are missing or unclosed.
fn extract_frontmatter_block(content: &str) -> Option<String> {
    let mut lines = content.split('\n').peekable();
    let first = lines.next()?;
    if !is_delimiter_line(first) {
        return None;
    }
    let mut fm_lines = Vec::new();
    for line in lines.by_ref() {
        if is_delimiter_line(line) {
            return if lines.peek().is_some() {
                Some(fm_lines.join("\n"))
            } else {
                None
            };
        }
        fm_lines.push(line);
    }
    None
}

/// Normalize `\r\n` and a lone `\r` to `\n` before the frontmatter body is
/// split into lines, so a stray carriage return cannot corrupt a scalar
/// value. Mirrors python's `str.splitlines()`, which treats a bare `\r` as
/// a line break, closely enough for this parser: the handful of exotic
/// Unicode line separators `splitlines()` also recognizes (`\v`, `\f`,
/// `\x1c`-`\x1e`, `\x85`, `U+2028`, `U+2029`) are not chased here, since
/// carriage-return coverage is the only gap that shows up in real fact
/// files. Applied only to the already-extracted frontmatter body, not to
/// the raw file content, so a whole-file-CRLF fact still fails to match the
/// opening delimiter the same way in both implementations.
fn normalize_line_endings(fm: &str) -> String {
    fm.replace("\r\n", "\n").replace('\r', "\n")
}

fn is_delimiter_line(line: &str) -> bool {
    line.starts_with("---") && line[3..].bytes().all(|b| b == b' ' || b == b'\t')
}

fn leading_ws_len(line: &str) -> usize {
    line.bytes()
        .take_while(|&b| b == b' ' || b == b'\t')
        .count()
}

/// `^([A-Za-z_]\w*):\s*(.*)`, only ever attempted against an unindented
/// line: the leading char class already excludes whitespace, so python's
/// extra `not line[0].isspace()` guard is redundant and not reproduced.
fn match_top_level(line: &str) -> Option<(&str, &str)> {
    match_key_colon_rest(line, 0)
}

/// `^\s{2,}- `: at least two leading spaces/tabs, then a literal `- `.
fn is_block_item(line: &str) -> bool {
    let n = leading_ws_len(line);
    n >= 2 && line[n..].starts_with("- ")
}

/// `re.sub(r'^\s+-\s+', '', line).strip()`.
fn extract_block_item(line: &str) -> String {
    let n = leading_ws_len(line);
    let after_dash = line[n..].strip_prefix('-').unwrap_or(&line[n..]);
    after_dash
        .trim_start_matches([' ', '\t'])
        .trim()
        .to_string()
}

/// Check for `^\s{2,}\w`: at least two leading spaces/tabs, then a word
/// character.
fn is_sub_kv_line(line: &str) -> bool {
    let n = leading_ws_len(line);
    n >= 2
        && line[n..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `^\s+([A-Za-z_]\w*):\s*(.*)`, applied to the whole line: the leading
/// `\s+` is greedy, so it consumes every leading space/tab regardless of how
/// many `is_sub_kv_line` required.
fn match_sub_kv(line: &str) -> Option<(&str, &str)> {
    let n = leading_ws_len(line);
    match_key_colon_rest(line, n)
}

fn match_key_colon_rest(line: &str, start: usize) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    if start >= bytes.len() || !(bytes[start].is_ascii_alphabetic() || bytes[start] == b'_') {
        return None;
    }
    let mut end = start + 1;
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }
    if end >= bytes.len() || bytes[end] != b':' {
        return None;
    }
    Some((&line[start..end], &line[end + 1..]))
}

/// Parse an inline YAML flow sequence like `[a, b, "c"]` into a list. Strips
/// the surrounding brackets, splits on commas, trims whitespace, strips
/// matching quotes on each item, and drops empty items so `[]` yields an
/// empty list rather than one empty string.
fn parse_inline_list(val: &str) -> Vec<String> {
    let inner = &val[1..val.len() - 1];
    inner
        .split(',')
        .filter_map(|raw| {
            let item = raw.trim();
            let bytes = item.as_bytes();
            let unquoted = if item.len() >= 2
                && bytes[0] == bytes[item.len() - 1]
                && (bytes[0] == b'"' || bytes[0] == b'\'')
            {
                &item[1..item.len() - 1]
            } else {
                item
            };
            if unquoted.is_empty() {
                None
            } else {
                Some(unquoted.to_string())
            }
        })
        .collect()
}

// --- Node/edge id derivation ----------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Scope {
    Global,
    Org,
    Project,
}

impl Scope {
    fn as_str(self) -> &'static str {
        let [global, org, project] = playbook_data::memory::FACT_SCOPES;
        match self {
            Scope::Global => global,
            Scope::Org => org,
            Scope::Project => project,
        }
    }
}

/// A repo-root-relative path (`owner/repo/tail...`) is project-scoped, a
/// two-segment path (`owner/tail...`) is org-scoped, a one-segment path is global.
fn scope_and_project(rel: &str) -> (Scope, Option<String>) {
    let normalized = rel.replace('\\', "/");
    let parts: Vec<&str> = normalized.split('/').collect();
    if parts.len() >= 3 {
        (Scope::Project, Some(format!("{}/{}", parts[0], parts[1])))
    } else if parts.len() == 2 {
        (Scope::Org, Some(parts[0].to_string()))
    } else {
        (Scope::Global, None)
    }
}

/// Strips a trailing `.md` regardless of case (`.MD`, `.Md`, ...), so a fact
/// walked in case-insensitively doesn't end up with a literal `.md`/`.MD`
/// tail baked into its node id or default display name.
fn strip_md_suffix_ci(s: &str) -> &str {
    if s.len() >= 3 && s[s.len() - 3..].eq_ignore_ascii_case(".md") {
        &s[..s.len() - 3]
    } else {
        s
    }
}

fn node_id(rel: &str, scope: Scope, project: Option<&str>) -> String {
    let normalized = rel.replace('\\', "/");
    let base = strip_md_suffix_ci(&normalized);
    match scope {
        Scope::Global => format!("global/{base}"),
        Scope::Org => {
            let parts: Vec<&str> = base.split('/').collect();
            let tail = parts.get(1..).unwrap_or(&[]).join("/");
            format!("{}/{tail}", project.unwrap_or(""))
        }
        Scope::Project => {
            let parts: Vec<&str> = base.split('/').collect();
            let tail = parts.get(2..).unwrap_or(&[]).join("/");
            format!("{}/{tail}", project.unwrap_or(""))
        }
    }
}

// --- Graph shape and rebuild ------------------------------------------------

// `version` has no reader yet; it is written for a future schema check
// described in ADR-0011. `pinned` is read by `render_ranked` in
// `json/memorycontext.rs`.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Graph {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    version: u32,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Node {
    id: String,
    file: String,
    scope: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pinned: Option<bool>,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Edge {
    from: String,
    to: String,
    relation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    signals: Option<Vec<String>>,
}

// --- Similarity signals for possible_relates_to edges -----------------------

/// Names for the 3 pairwise-similarity signals a `possible_relates_to` edge
/// can carry in its `signals` list: a shared anchor parent directory,
/// matching `type` and `scope` together (one signal, not two), and a
/// Jaccard word-overlap over `SIMILARITY_JACCARD_THRESHOLD` on the two
/// facts' bodies.
const SIGNAL_ANCHOR_DIR: &str = "anchor_dir";
const SIGNAL_TYPE_SCOPE: &str = "type_scope";
const SIGNAL_BODY_OVERLAP: &str = "body_overlap";

/// A pair sharing 2 or more of the 3 signals gets a `possible_relates_to`
/// edge. This is never auto-promoted to a real `relates_to` edge; that
/// judgement call is left to a human or model later.
const SIMILARITY_SIGNAL_HIT_THRESHOLD: usize = 2;

/// A fact body's word set counts as similar to another's once their Jaccard
/// ratio reaches this value.
const SIMILARITY_JACCARD_THRESHOLD: f64 = 0.35;

/// A fact's data reduced to what the pairwise similarity check needs.
/// Deliberately not stored on `Node`: nothing else reads an anchor list or a
/// body back from the graph, so keeping this internal to the rebuild avoids
/// growing the public graph schema for an implementation detail.
struct SimilarityInfo {
    id: String,
    /// Dense id of the (type, scope) pair, so the signal is one integer compare.
    kind_scope: u32,
    /// Sorted interned ids of the NON-EMPTY anchor parent directories.
    anchor_dirs: Vec<u32>,
    /// Sorted, deduplicated interned ids of the body's words.
    body_words: Vec<u32>,
}

/// A fast, non-cryptographic hasher (FxHash style) for the word interner,
/// whose keys are trusted memory text and never attacker-chosen map keys.
#[derive(Default, Clone, Copy)]
struct FastHasher(u64);

impl std::hash::Hasher for FastHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        const K: u64 = 0x517c_c1b7_2722_0a95;
        for chunk in bytes.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.0 = (self.0.rotate_left(5) ^ u64::from_le_bytes(word)).wrapping_mul(K);
        }
    }
}

type FastHash = std::hash::BuildHasherDefault<FastHasher>;

/// Bit set for each of the three similarity signals a pair matched.
const BIT_ANCHOR_DIR: u8 = 1;
const BIT_TYPE_SCOPE: u8 = 2;
const BIT_BODY_OVERLAP: u8 = 4;

/// Parent directory of an anchor path, e.g. `src/foo/a.ts` -> `src/foo`. An
/// anchor with no parent (a bare filename) yields an empty string.
fn anchor_parent_dir(anchor: &str) -> String {
    Path::new(anchor)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Signal 1: the two facts' anchor lists share at least one NON-EMPTY parent
/// directory. A fact with no anchors has an empty `anchor_dirs` set, so it
/// never contributes this signal for either side of a pair. The empty string
/// (a bare top-level filename with no parent) is excluded from the overlap
/// check on purpose: otherwise any two facts each anchoring a different
/// top-level file (e.g. `README.md` and `package.json`) would share
/// "directory" `""` and falsely count as a hit despite anchoring unrelated
/// files.
fn shares_anchor_dir(a: &SimilarityInfo, b: &SimilarityInfo) -> bool {
    sorted_intersection_len(&a.anchor_dirs, &b.anchor_dirs) > 0
}

/// Whether `c` shared words out of `la` and `lb` reach the Jaccard threshold.
/// An empty union never does, so two bodyless facts are never similar by
/// default and the check never divides by zero.
fn jaccard_passes(c: usize, la: usize, lb: usize) -> bool {
    let union = la + lb - c;
    union != 0 && c as f64 / union as f64 >= SIMILARITY_JACCARD_THRESHOLD
}

/// The smallest shared-word count that reaches the threshold for lists of
/// `la` and `lb` words, decided with the same f64 test as `jaccard_passes`, so
/// the answer matches a direct ratio comparison exactly. Above `min(la, lb)`
/// means the pair cannot reach it.
fn min_shared_words(la: usize, lb: usize) -> usize {
    let t = SIMILARITY_JACCARD_THRESHOLD;
    let cap = la.min(lb) + 1;
    let mut c = ((t / (1.0 + t)) * (la + lb) as f64).floor() as usize;
    c = c.min(cap);
    while c > 0 && jaccard_passes(c - 1, la, lb) {
        c -= 1;
    }
    while c < cap && !jaccard_passes(c, la, lb) {
        c += 1;
    }
    c
}

/// Whether two sorted, deduplicated id lists share at least `need` ids. Stops
/// as soon as the answer is known: enough found, or too few left to find.
fn shares_at_least(a: &[u32], b: &[u32], need: usize) -> bool {
    let (mut i, mut j, mut n) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        if n >= need {
            return true;
        }
        if n + (a.len() - i).min(b.len() - j) < need {
            return false;
        }
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                n += 1;
                i += 1;
                j += 1;
            }
        }
    }
    n >= need
}

/// Size of the intersection of two sorted, deduplicated id lists, by merge.
fn sorted_intersection_len(a: &[u32], b: &[u32]) -> usize {
    let (mut i, mut j, mut n) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                n += 1;
                i += 1;
                j += 1;
            }
        }
    }
    n
}

/// True when the size ratio alone rules out reaching the Jaccard threshold:
/// the score never exceeds `min / max`, and f64 division is monotonic.
fn too_unequal_for_jaccard(a: &[u32], b: &[u32]) -> bool {
    let (small, large) = (a.len().min(b.len()), a.len().max(b.len()));
    small == 0 || (small as f64 / large as f64) < SIMILARITY_JACCARD_THRESHOLD
}

/// Below this many facts the pair loop runs on one thread: the pairs cost less
/// than starting threads.
const PARALLEL_PAIR_MIN_FACTS: usize = 256;

/// The `possible_relates_to` edges of fact `i` against every later fact.
fn similarity_edges_of(infos: &[SimilarityInfo], i: usize) -> Vec<Edge> {
    let mut out = Vec::new();
    let a = &infos[i];
    for b in &infos[i + 1..] {
        let mut mask = 0u8;
        if shares_anchor_dir(a, b) {
            mask |= BIT_ANCHOR_DIR;
        }
        if a.kind_scope == b.kind_scope {
            mask |= BIT_TYPE_SCOPE;
        }
        // No signal yet: even body overlap alone cannot reach 2.
        if mask == 0 {
            continue;
        }
        // Body overlap is one signal, so with no other hit the pair cannot reach 2.
        if !too_unequal_for_jaccard(&a.body_words, &b.body_words) {
            let need = min_shared_words(a.body_words.len(), b.body_words.len());
            if shares_at_least(&a.body_words, &b.body_words, need) {
                mask |= BIT_BODY_OVERLAP;
            }
        }
        if mask.count_ones() as usize >= SIMILARITY_SIGNAL_HIT_THRESHOLD {
            let mut signals = Vec::with_capacity(3);
            if mask & BIT_ANCHOR_DIR != 0 {
                signals.push(SIGNAL_ANCHOR_DIR.to_string());
            }
            if mask & BIT_TYPE_SCOPE != 0 {
                signals.push(SIGNAL_TYPE_SCOPE.to_string());
            }
            if mask & BIT_BODY_OVERLAP != 0 {
                signals.push(SIGNAL_BODY_OVERLAP.to_string());
            }
            out.push(Edge {
                from: a.id.clone(),
                to: b.id.clone(),
                relation: "possible_relates_to".to_string(),
                signals: Some(signals),
            });
        }
    }
    out
}

/// Every `possible_relates_to` edge, in `(i, j)` order. `i < j` visits each
/// unordered pair once and never compares a fact to itself. With enough facts
/// the rows are spread over threads (row `i` costs about `n - i` pairs, so
/// they are dealt round robin) and put back in `i` order, so the output is the
/// same bytes as the single thread run.
fn similarity_edges(infos: &[SimilarityInfo]) -> Vec<Edge> {
    let n = infos.len();
    let threads = if n < PARALLEL_PAIR_MIN_FACTS {
        1
    } else {
        std::thread::available_parallelism()
            .map_or(1, std::num::NonZeroUsize::get)
            .min(8)
    };
    if threads == 1 {
        return (0..n).flat_map(|i| similarity_edges_of(infos, i)).collect();
    }
    let mut rows: Vec<Vec<Edge>> = (0..n).map(|_| Vec::new()).collect();
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|t| {
                scope.spawn(move || {
                    (t..n)
                        .step_by(threads)
                        .map(|i| (i, similarity_edges_of(infos, i)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for worker in workers {
            // A panicking worker loses its rows rather than the whole rebuild.
            for (i, row) in worker.join().unwrap_or_default() {
                rows[i] = row;
            }
        }
    });
    rows.into_iter().flatten().collect()
}

/// Rebuild the graph unconditionally, with no payload and no skip check.
///
/// Exists because `run` deliberately skips when the payload names no file
/// under the memory dir, which is correct for a PostToolUse hook but leaves
/// no way to force a rebuild. `commands/learn-project.md`'s `--graph-only`
/// path needs exactly that: the python original treated empty stdin as
/// "rebuild everything", and this port dropped that branch (see `should_skip`)
/// on the grounds it was unexercised by the test suite. It was exercised, just
/// by a slash command rather than a test.
pub fn rebuild_now() -> Result<(), RebuildError> {
    rebuild()
}

/// Serializes concurrent rebuilds (two sessions saving facts near the same
/// moment) with an mkdir-based advisory lock at `memory.graph.json.lock`, matching
/// `atomic_append`'s convention in `common::atomic`. Without this, two
/// rebuilds can interleave: the one that started from a staler view of the
/// memory tree can still finish (and rename) after the fresher one, silently
/// discarding whatever the fresher rebuild had just seen. The lock forces
/// the full read-build-write cycle to run as one unit, so the rebuild that
/// completes second always reads the tree as the first one left it. Fails
/// open after the retry budget, same as every other lock in this codebase:
/// a hook must never hang waiting on contention.
fn rebuild() -> Result<(), RebuildError> {
    rebuild_in(&memory_dir())
}

/// [`rebuild`] for the store at `mem_dir`.
fn rebuild_in(mem_dir: &Path) -> Result<(), RebuildError> {
    with_rebuild_lock(mem_dir, || rebuild_locked(mem_dir))
}

/// Runs `f` holding the rebuild lock, or without it after the retry budget.
fn with_rebuild_lock<T>(mem_dir: &Path, f: impl FnOnce() -> T) -> T {
    let lock_path = mem_dir.join("memory.graph.json.lock");
    // The memory migration holds this same lock across a copy, so a rebuild
    // must use its stale age or it would steal a live migration's lock.
    let (acquired, result) = with_dir_lock_aged(
        &lock_path,
        50,
        Duration::from_millis(10),
        LONG_LOCK_STALE_AGE,
        f,
    );
    if acquired {
        let _ = fs::remove_dir(&lock_path);
    }
    result
}

fn rebuild_locked(mem_dir: &Path) -> Result<(), RebuildError> {
    let mut nodes: Vec<Node> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen_code: HashSet<String> = HashSet::new();
    // (from_id, relation, raw_target, source_scope, source_project): buffered
    // here, resolved in pass 2 once every node id is known.
    let mut pending_links: Vec<(String, String, String, Scope, Option<String>)> = Vec::new();
    // One entry per fact (never per code-anchor node), consumed by the
    // pairwise similarity check in pass 3, once every fact has been walked.
    let mut infos: Vec<SimilarityInfo> = Vec::new();
    // Body word to dense id, shared by every fact so overlap is an integer merge.
    let mut interner: HashMap<String, u32, FastHash> = HashMap::default();
    // (type, scope) and anchor directory to dense ids, so those signals are integer compares.
    let mut kind_scopes: HashMap<(String, Scope), u32> = HashMap::new();
    let mut dir_ids: HashMap<String, u32> = HashMap::new();

    let files = walk_markdown_files(mem_dir)
        .map_err(|e| RebuildError(format!("read memory directory tree: {e}")))?;
    for fpath in files {
        let Ok(rel_path) = fpath.strip_prefix(mem_dir) else {
            continue;
        };
        let rel = rel_path.to_string_lossy().replace('\\', "/");
        let Ok(content) = fs::read_to_string(&fpath) else {
            continue;
        };

        let fm = parse_frontmatter(&content);
        let (scope, proj) = scope_and_project(&rel);
        let nid = node_id(&rel, scope, proj.as_deref());

        let file_name = fpath
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let default_name = strip_md_suffix_ci(&file_name).to_string();

        let node_type = fm
            .scalar("type")
            .cloned()
            .unwrap_or_else(|| "reference".to_string());
        let name = fm.scalar("name").cloned().unwrap_or(default_name);
        let description = fm.scalar("description").cloned().unwrap_or_default();
        // Exact, case-sensitive match against the literal `true`: anything else
        // (quoted, a different case, `false`, or absent) is treated as unset.
        let pinned = (fm.scalar("pinned").map(String::as_str) == Some("true")).then_some(true);

        let anchor_dirs: HashSet<String> = fm
            .list("anchors")
            .map(|anchors| anchors.iter().map(|a| anchor_parent_dir(a)).collect())
            .unwrap_or_default();
        let mut body_words: Vec<u32> = extract_body(&content)
            .to_lowercase()
            .split_whitespace()
            .map(|w| match interner.get(w) {
                Some(&id) => id,
                None => {
                    let id = interner.len() as u32;
                    interner.insert(w.to_string(), id);
                    id
                }
            })
            .collect();
        body_words.sort_unstable();
        body_words.dedup();
        let next_kind = kind_scopes.len() as u32;
        let kind_scope = *kind_scopes
            .entry((node_type.clone(), scope))
            .or_insert(next_kind);
        let mut anchor_dir_ids: Vec<u32> = anchor_dirs
            .iter()
            .filter(|dir| !dir.is_empty())
            .map(|dir| {
                let next = dir_ids.len() as u32;
                *dir_ids.entry(dir.clone()).or_insert(next)
            })
            .collect();
        anchor_dir_ids.sort_unstable();
        infos.push(SimilarityInfo {
            id: nid.clone(),
            kind_scope,
            anchor_dirs: anchor_dir_ids,
            body_words,
        });

        nodes.push(Node {
            id: nid.clone(),
            file: rel.clone(),
            scope: scope.as_str().to_string(),
            kind: node_type,
            name: Some(name),
            description: Some(description),
            project: proj.clone(),
            pinned,
        });

        if let Some(links) = fm.dict("links") {
            // Sorted by relation so two rebuilds of the same store write the
            // same bytes; a HashMap walk made the edge order change per run.
            let mut links: Vec<(&String, &SubValue)> = links.iter().collect();
            links.sort_by(|a, b| a.0.cmp(b.0));
            for (relation, target) in links {
                let targets = match target {
                    SubValue::List(items) => items.clone(),
                    SubValue::Scalar(one) => vec![one.clone()],
                };
                for one_target in targets {
                    pending_links.push((
                        nid.clone(),
                        relation.clone(),
                        one_target,
                        scope,
                        proj.clone(),
                    ));
                }
            }
        }

        if let Some(anchors) = fm.list("anchors") {
            for anchor in anchors {
                let cid = match &proj {
                    Some(p) => format!("code:{p}/{anchor}"),
                    None => format!("code:{anchor}"),
                };
                if !seen_code.contains(&cid) {
                    nodes.push(Node {
                        id: cid.clone(),
                        file: anchor.clone(),
                        scope: playbook_data::memory::CODE_SCOPE.to_string(),
                        kind: "code".to_string(),
                        name: None,
                        description: None,
                        project: proj.clone(),
                        pinned: None,
                    });
                    seen_code.insert(cid.clone());
                }
                edges.push(Edge {
                    from: nid.clone(),
                    to: cid,
                    relation: "anchors".to_string(),
                    signals: None,
                });
            }
        }
    }

    // Pass 2: resolve buffered links now that every node id is known. A
    // project-scoped source resolves in its own scope first, then falls
    // back to global. A target found nowhere still emits the same-scope id,
    // so the edge is written and reads as dangling instead of being
    // silently dropped.
    let node_ids: HashSet<String> = nodes.iter().map(|n| n.id.clone()).collect();
    for (from_id, relation, raw_target, src_scope, src_project) in pending_links {
        let target_id = match src_scope {
            Scope::Global => format!("global/{raw_target}"),
            Scope::Org | Scope::Project => {
                let project = src_project.unwrap_or_default();
                let same_scope_id = format!("{project}/{raw_target}");
                let global_id = format!("global/{raw_target}");
                if node_ids.contains(&same_scope_id) {
                    same_scope_id
                } else if node_ids.contains(&global_id) {
                    global_id
                } else {
                    same_scope_id
                }
            }
        };
        edges.push(Edge {
            from: from_id,
            to: target_id,
            relation,
            signals: None,
        });
    }

    // Pass 3: pairwise similarity check between every two distinct facts.
    // `i < j` visits each unordered pair exactly once and never compares a
    // fact to itself, so no separate dedup step is needed. A pair sharing 2
    // or more of the 3 signals gets a `possible_relates_to` edge naming
    // which signals matched.
    edges.extend(similarity_edges(&infos));

    // Two files can name one node (`x.md` and `global/x.md` are both
    // `global/x`) and one link can be written twice. Both stores hold each
    // node id and each (from, to, relation) once, the first one found.
    let mut seen_nodes: HashSet<String> = HashSet::new();
    nodes.retain(|n| seen_nodes.insert(n.id.clone()));
    let mut seen_edges: HashSet<(String, String, String)> = HashSet::new();
    edges.retain(|e| seen_edges.insert((e.from.clone(), e.to.clone(), e.relation.clone())));

    write_graph_store(
        mem_dir,
        &Graph {
            nodes,
            edges,
            version: 1,
        },
    )
}

/// Recursively collect every `.md` file under `dir` except `MEMORY.md`,
/// pruning dot-directories. Loosely mirrors the `os.walk` filter in
/// the retired shell original:rebuild`, but diverges on case: the match
/// here is case-insensitive (`.MD`, `MEMORY.MD`, ...), where python's
/// `endswith` was not. A directory that cannot be read (missing, permissions)
/// aborts the walk with an error naming that directory, rather than silently
/// contributing zero files.
fn walk_markdown_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    // A store that was never created holds no facts. Only the top level is
    // forgiven: a missing nested directory still reports its path.
    if let Err(e) = fs::symlink_metadata(dir) {
        if e.kind() == io::ErrorKind::NotFound {
            return Ok(out);
        }
    }
    walk_markdown_files_into(dir, &mut out)?;
    Ok(out)
}

fn walk_markdown_files_into(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    // Wrapped here, at the actual failing directory, rather than only once
    // where the walk is first invoked: a bare `io::Error` carries no path, so
    // without this every failure would read as "top-level memory dir
    // unreadable" regardless of which nested subdirectory actually failed.
    let entries = fs::read_dir(dir)
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", dir.display())))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            // `staging` holds unapproved learn-project candidates, never live facts.
            if !name.starts_with('.') && name != "staging" {
                walk_markdown_files_into(&path, out)?;
            }
        } else if name.to_ascii_lowercase().ends_with(".md")
            && !name.eq_ignore_ascii_case("MEMORY.md")
        {
            out.push(path);
        }
    }
    Ok(())
}

/// Writes `graph` into `data/memory.db` beside `mem_dir`, in one transaction,
/// so a reader sees the old graph or the new one and a failed write leaves
/// the old one. A file that is not a database is derived data: it is set
/// aside (never deleted) and the write runs once more on a fresh file.
fn write_graph_store(mem_dir: &Path, graph: &Graph) -> Result<(), RebuildError> {
    let db = store_db(mem_dir)?;
    let (nodes, edges) = rows_of(graph);
    let mut result = playbook_data::memory::write_graph(&db, &nodes, &edges);
    if result
        .as_ref()
        .err()
        .is_some_and(|e| playbook_data::memory::is_corrupt(e))
        && playbook_data::migrate::set_aside(&db).is_ok()
    {
        result = playbook_data::memory::write_graph(&db, &nodes, &edges);
    }
    result.map_err(|e| RebuildError(format!("write memory.db: {e}")))
}

/// The rows of `memory.db` that hold `graph`.
fn rows_of(
    graph: &Graph,
) -> (
    Vec<playbook_data::memory::NodeRow>,
    Vec<playbook_data::memory::EdgeRow>,
) {
    let nodes = graph
        .nodes
        .iter()
        .map(|n| playbook_data::memory::NodeRow {
            id: n.id.clone(),
            file: n.file.clone(),
            scope: n.scope.clone(),
            project: n.project.clone(),
            kind: n.kind.clone(),
            name: n.name.clone(),
            description: n.description.clone(),
            pinned: n.pinned.unwrap_or(false),
        })
        .collect();
    let edges = graph
        .edges
        .iter()
        .map(|e| playbook_data::memory::EdgeRow {
            from: e.from.clone(),
            to: e.to.clone(),
            relation: e.relation.clone(),
            signals: e.signals.clone(),
        })
        .collect();
    (nodes, edges)
}

/// `data/memory.db` of the store at `mem_dir` (`<root>/memory`).
fn store_db(mem_dir: &Path) -> Result<PathBuf, RebuildError> {
    let root = mem_dir
        .parent()
        .ok_or_else(|| RebuildError(format!("no data folder beside {}", mem_dir.display())))?;
    Ok(playbook_data::memory::db_in(&root.join("data")))
}

/// Puts a `memory.graph.json` left by an older playbook into `memory.db`.
///
/// The file is derived from the fact files, so the database is rebuilt from
/// them rather than copied (skipped when the database is already at least as
/// new as the file), and the file is then renamed to
/// `memory.graph.json.migrated`. A copy left by an earlier move is replaced,
/// because both are regenerable from the facts. An older binary that keeps
/// writing the file brings it back, and the next call folds it in again.
/// Returns `false` when the file is still there, because the rebuild lock was
/// busy or the rebuild failed, so a caller can read the file instead.
pub fn migrate_json_graph(mem_dir: &Path) -> bool {
    let json = mem_dir.join("memory.graph.json");
    if !json.is_file() {
        return true;
    }
    // Under the rebuild lock, so two readers do not move the file twice, and
    // never without it: the memory move holds this lock across a copy.
    let lock_path = mem_dir.join("memory.graph.json.lock");
    if !acquire_dir_lock_aged(
        &lock_path,
        50,
        Duration::from_millis(10),
        LONG_LOCK_STALE_AGE,
    ) {
        return false;
    }
    let moved = (|| {
        if !json.is_file() {
            return true;
        }
        let touched = |p: Option<PathBuf>| {
            p.and_then(|p| playbook_data::memory::touched_at(&p).ok().flatten())
        };
        let db_time = touched(store_db(mem_dir).ok());
        let json_time = fs::metadata(&json).and_then(|m| m.modified()).ok();
        let current = matches!((db_time, json_time), (Some(d), Some(j)) if d >= j);
        if !current && rebuild_locked(mem_dir).is_err() {
            return false;
        }
        let mut kept = json.clone().into_os_string();
        kept.push(".migrated");
        if fs::rename(&json, kept).is_err() {
            return false;
        }
        playbook_data::migrate::notice(
            "playbook: moved the memory graph into data/memory.db; the old memory.graph.json was renamed to memory.graph.json.migrated",
        );
        true
    })();
    let _ = fs::remove_dir(&lock_path);
    moved
}

/// The memory graph as the JSON document readers parse
/// (`{"nodes": [...], "edges": [...], "version": 1}`), from `memory.db`.
/// `None` when there is no graph. A `memory.graph.json` from an older
/// playbook is folded into the database first, and is read as it is only
/// when that fails.
pub fn graph_text(mem_dir: &Path) -> Option<String> {
    if !migrate_json_graph(mem_dir) {
        return fs::read_to_string(mem_dir.join("memory.graph.json")).ok();
    }
    export_json(mem_dir.parent()?).ok().flatten()
}

/// When the graph last changed: the later of `memory.db` and a
/// `memory.graph.json` an older playbook wrote. It never folds the file in,
/// so asking does not count as a write. `Ok(None)` when there is no graph,
/// and an error when either cannot be inspected.
pub fn graph_touched_at(mem_dir: &Path) -> io::Result<Option<std::time::SystemTime>> {
    let db = store_db(mem_dir).map_err(|e| io::Error::other(e.0))?;
    let db_time = playbook_data::memory::touched_at(&db)?;
    let json_time = match fs::metadata(mem_dir.join("memory.graph.json")).and_then(|m| m.modified())
    {
        Ok(t) => Some(t),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    Ok(db_time.into_iter().chain(json_time).max())
}

/// Writes the graph in `json` (the document shape above) into `memory.db`
/// beside `mem_dir`. For tests and tools that start from a graph document.
pub fn import_json(mem_dir: &Path, json: &str) -> Result<(), RebuildError> {
    let graph: Graph =
        serde_json::from_str(json).map_err(|e| RebuildError(format!("parse graph: {e}")))?;
    let (nodes, edges) = rows_of(&graph);
    playbook_data::memory::write_graph(&store_db(mem_dir)?, &nodes, &edges)
        .map_err(|e| RebuildError(format!("write memory.db: {e}")))
}

/// The graph stored in `memory.db`, as the JSON document `memory.graph.json`
/// holds (`{"nodes": [...], "edges": [...], "version": 1}`), for tools that
/// read the JSON shape. `Ok(None)` when there is no database yet.
pub fn export_json(root: &Path) -> Result<Option<String>, RebuildError> {
    let db = playbook_data::memory::db_in(&root.join("data"));
    let Some((nodes, edges)) = playbook_data::memory::read_graph(&db)
        .map_err(|e| RebuildError(format!("read memory.db: {e}")))?
    else {
        return Ok(None);
    };
    let graph = Graph {
        nodes: nodes
            .into_iter()
            .map(|n| Node {
                id: n.id,
                file: n.file,
                scope: n.scope,
                kind: n.kind,
                name: n.name,
                description: n.description,
                project: n.project,
                pinned: n.pinned.then_some(true),
            })
            .collect(),
        edges: edges
            .into_iter()
            .map(|e| Edge {
                from: e.from,
                to: e.to,
                relation: e.relation,
                signals: e.signals,
            })
            .collect(),
        version: 1,
    };
    serde_json::to_string_pretty(&graph)
        .map(Some)
        .map_err(|e| RebuildError(format!("serialize graph: {e}")))
}

#[cfg(test)]
mod similarity_tests {
    use super::*;

    #[test]
    fn min_shared_words_matches_a_direct_ratio_test_for_every_size() {
        for la in 1..=80usize {
            for lb in 1..=80usize {
                if too_unequal_for_jaccard(&vec![0; la], &vec![0; lb]) {
                    continue;
                }
                let need = min_shared_words(la, lb);
                for c in 0..=la.min(lb) {
                    assert_eq!(
                        c >= need,
                        jaccard_passes(c, la, lb),
                        "la={la} lb={lb} c={c} need={need}"
                    );
                }
            }
        }
    }

    #[test]
    fn shares_at_least_agrees_with_a_full_merge() {
        let a = [1u32, 3, 5, 7, 9, 11];
        let b = [2u32, 3, 4, 7, 11, 12, 13];
        let full = sorted_intersection_len(&a, &b);
        assert_eq!(full, 3);
        for need in 0..=7 {
            assert_eq!(shares_at_least(&a, &b, need), full >= need, "need={need}");
        }
        assert!(!shares_at_least(&[], &b, 1));
        assert!(shares_at_least(&[], &b, 0));
    }

    fn info(id: &str, kind_scope: u32, dirs: &[u32], words: &[u32]) -> SimilarityInfo {
        SimilarityInfo {
            id: id.to_string(),
            kind_scope,
            anchor_dirs: dirs.to_vec(),
            body_words: words.to_vec(),
        }
    }

    #[test]
    fn the_threaded_run_returns_the_same_edges_in_the_same_order() {
        // Enough facts to take the threaded path, with overlapping anchors,
        // two type/scope groups and body words that overlap in blocks.
        let infos: Vec<SimilarityInfo> = (0..PARALLEL_PAIR_MIN_FACTS + 40)
            .map(|i| {
                let words: Vec<u32> = (0..30).map(|k| (i as u32 / 7) * 5 + k).collect();
                info(&format!("n{i}"), (i % 2) as u32, &[(i % 11) as u32], &words)
            })
            .collect();
        let threaded = similarity_edges(&infos);
        let single: Vec<Edge> = (0..infos.len())
            .flat_map(|i| similarity_edges_of(&infos, i))
            .collect();
        assert!(!single.is_empty());
        let key = |e: &Edge| (e.from.clone(), e.to.clone(), e.signals.clone());
        assert_eq!(
            threaded.iter().map(key).collect::<Vec<_>>(),
            single.iter().map(key).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_pair_needs_two_signals_and_names_them_in_a_fixed_order() {
        let a = info("a", 0, &[1], &[1, 2, 3, 4]);
        let same_all = info("b", 0, &[1], &[1, 2, 3, 4]);
        let anchor_only = info("c", 9, &[1], &[50, 51, 52, 53]);
        let none = info("d", 9, &[], &[60, 61]);
        let infos = [a, same_all, anchor_only, none];
        let edges = similarity_edges_of(&infos, 0);
        let signals: Vec<_> = edges
            .iter()
            .map(|e| (e.to.as_str(), e.signals.clone().unwrap()))
            .collect();
        assert_eq!(
            signals,
            vec![(
                "b",
                vec![
                    "anchor_dir".to_string(),
                    "type_scope".to_string(),
                    "body_overlap".to_string()
                ]
            )]
        );
    }
}

#[cfg(test)]
mod alias_tests {
    use super::*;

    #[test]
    fn fact_aliases_reads_an_inline_list() {
        let content = "---\nname: x\naliases: [pbcode, pb code]\n---\nbody\n";
        assert_eq!(fact_aliases(content), vec!["pbcode", "pb code"]);
    }

    #[test]
    fn fact_aliases_reads_a_block_list() {
        let content = "---\nname: x\naliases:\n  - pbcode\n  - pb code\n---\nbody\n";
        assert_eq!(fact_aliases(content), vec!["pbcode", "pb code"]);
    }

    #[test]
    fn fact_aliases_is_empty_without_the_key() {
        assert!(fact_aliases("---\nname: x\n---\nbody\n").is_empty());
    }

    #[test]
    fn fact_aliases_is_empty_for_bad_frontmatter() {
        assert!(fact_aliases("no frontmatter at all").is_empty());
        assert!(fact_aliases("---\naliases: [a, b]\nnever closed").is_empty());
    }

    #[test]
    fn fact_aliases_is_empty_when_the_key_is_a_plain_scalar() {
        assert!(fact_aliases("---\naliases: pbcode\n---\nbody\n").is_empty());
    }
}
