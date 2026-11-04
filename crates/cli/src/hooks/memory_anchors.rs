// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Two events, one cache. `PreToolUse` on Edit|Write: when the target path
//! is anchored in the graph-first memory store
//! (`~/.config/playbook/data/memory.db`), surface the facts that describe it, plus
//! their `depends_on` and `contradicts` neighbours, as `additionalContext`
//! before the edit lands. `UserPromptSubmit` (ADR 0008 WU-0): match prompt
//! text and this-session touched files against the same index, injecting
//! the matched facts' BODIES (not just names), deduped per session. The
//! edit event emits nothing on no match. The prompt event prints one hint
//! line (at most 5 per session) when a prompt with searchable words matches
//! no fact or only weak ones, and nothing extra on a strong match. It stays
//! silent when the anchor index is empty or when every match was already
//! shown earlier in the session. Neither
//! ever blocks. `PreToolUse` ports
//! the retired shell original; `UserPromptSubmit` has no python precedent.
//!
//! Performance: `PreToolUse` fires on every single Edit and Write, so it
//! must not parse the graph on every call. The anchor index is built once
//! per session into a flat, tab-separated file under the session dir, and
//! every lookup after that is a plain scan of that file, no JSON parsing.
//! `UserPromptSubmit` reuses the identical cache: whichever event fires
//! first in a session builds it.
//!
//! Staleness: the index is built once, on the first Edit, Write, or prompt
//! of the session, and never rebuilt within that session. A fact added to
//! the graph mid-session (via `rebuild_memory_graph.rs`, the sole writer of
//! the file this hook reads) will not appear here until the next session
//! starts with a fresh cache. This is deliberate and pinned by the stale
//! cache scenario in the retired shell test; see that file's own
//! comment for the full rationale.

use crate::common::paths::memory_dir;
use crate::common::payload::Payload;
use crate::common::{emit_pre_context, emit_prompt_context, repo_slug, session_dir};
use crate::hooks::git_toplevel;
use crate::hooks::memory_signals;
use crate::hooks::staleness::{self, check_staleness};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Total budget for `run_prompt`'s staleness-check loop across all matched
/// facts; `git::TIMEOUT` only bounds one call, not the chain of them.
const STALENESS_BUDGET: Duration = Duration::from_secs(3);

/// Most facts one prompt may inject.
const MAX_FACTS_PER_PROMPT: usize = 3;

/// Most facts recalled over a whole session, across prompts. Past it the hook
/// goes quiet, so a long session cannot keep adding recall to its history.
const MAX_FACTS_PER_SESSION: usize = 9;

/// Longest fact body injected, in characters. Longer bodies are cut and point
/// at the file, which the model can read when it needs the rest.
const BODY_CAP_CHARS: usize = 1500;

/// Hard ceiling for one prompt's whole recall message.
const PROMPT_RECALL_CAP_CHARS: usize = 5000;

/// Most searched terms a recall hint names.
const MAX_HINT_TERMS: usize = 6;

/// Longest single term in a recall hint, in characters.
const MAX_HINT_TERM_CHARS: usize = 30;

/// Longest joined term list in a recall hint, in characters.
const MAX_HINT_TERMS_CHARS: usize = 100;

/// Longest repo slug in a recall hint's folder path, in characters.
const MAX_HINT_REPO_CHARS: usize = 80;

/// Session file holding one line per recall hint already emitted.
const HINTS_FILE: &str = "prompt-recall-hints.tsv";

/// The line written to `HINTS_FILE` for each emitted hint.
const HINT_MARK: &str = "hint";

/// Most recall hints one session may receive.
const MAX_HINTS_PER_SESSION: usize = 5;

/// Distinct prompt tokens that must hit a fact's name for a strong match.
const STRONG_NAME_TOKENS: usize = 2;

/// Score of a name hit; a description hit scores one.
const NAME_HIT_SCORE: usize = 2;

/// Score of a fact alias found in the prompt.
const ALIAS_HIT_SCORE: usize = 2;

/// Index column that holds the fact file, relative to the memory root.
const FACT_FILE_COLUMN: usize = 5;

/// Index column that holds the fact aliases, joined with `ALIAS_SEPARATOR`.
/// Older rows have six columns and no aliases.
const ALIAS_COLUMN: usize = 6;

/// Joins the aliases of one fact inside the alias column.
const ALIAS_SEPARATOR: char = '|';

/// Word that names the crates folder in an anchor path.
const CRATES_DIR: &str = "crates";

/// Score of a prompt word that names the crate folder of an anchor.
const PATH_HIT_SCORE: usize = 2;

/// Score of a fact anchored to a file touched this session.
const TOUCHED_FILE_SCORE: usize = 1;

/// Hard ceiling for the `PreToolUse:Edit` anchor message, in characters. The
/// message repeated on every edit and averaged about 1,600 characters, so it is
/// cut at a line boundary and says how many anchored facts it left out.
const EDIT_ANCHOR_CAP_CHARS: usize = 600;

/// Shortest prompt word that can match.
const MIN_TOKEN_CHARS: usize = 3;

/// Words that appear in most prompts and say nothing about a topic. Without
/// this list, "fix the failing test" matched 180 of 261 facts. Sorted, so a
/// binary search finds a word.
const STOPWORDS: [&str; 117] = [
    "about", "above", "add", "after", "again", "all", "also", "and", "any", "are", "because",
    "been", "before", "being", "both", "but", "can", "change", "check", "code", "could", "did",
    "does", "doing", "done", "each", "else", "file", "files", "find", "fix", "for", "from", "get",
    "give", "going", "have", "help", "here", "how", "into", "its", "just", "keep", "let", "like",
    "look", "make", "may", "more", "most", "need", "new", "not", "now", "off", "one", "only",
    "other", "our", "out", "over", "please", "put", "really", "run", "same", "see", "set",
    "should", "show", "some", "still", "such", "take", "tell", "test", "tests", "than", "that",
    "the", "their", "them", "then", "there", "these", "they", "thing", "things", "this", "those",
    "through", "too", "try", "update", "use", "using", "very", "want", "was", "way", "well",
    "were", "what", "when", "where", "which", "while", "who", "why", "will", "with", "work",
    "would", "yes", "you", "your",
];

/// Pure wrapper around the deadline comparison, so the boundary condition is
/// directly testable without depending on real elapsed wall-clock time or a
/// real `git` subprocess in a test.
fn within_staleness_budget(deadline: Instant) -> bool {
    Instant::now() < deadline
}

pub fn run(payload: &Payload) {
    let dir = session_dir(payload);
    if dir.is_empty() {
        return;
    }

    if payload.field(".hook_event_name") == "UserPromptSubmit" {
        run_prompt(payload, &dir);
        return;
    }

    let raw_path = payload.field(".tool_input.file_path");
    if raw_path.is_empty() {
        return;
    }
    let root = git_toplevel();
    let relpath = repo_relative_path(&root, &raw_path);
    if relpath.is_empty() {
        return;
    }

    let idx_path = Path::new(&dir).join("memory-anchor-index.tsv");
    if !idx_path.exists() {
        build_index(&idx_path);
    }

    let Ok(metadata) = fs::metadata(&idx_path) else {
        return;
    };
    if metadata.len() == 0 {
        return;
    }
    let Ok(contents) = fs::read_to_string(&idx_path) else {
        return;
    };

    let matches = matching_rows(&contents, &relpath);
    if matches.is_empty() {
        return;
    }

    let bump_seen_path = Path::new(&dir).join("anchor-bump-seen.tsv");
    let bump_seen = read_seen(&bump_seen_path);
    let mut newly_bumped = Vec::new();
    for row in &matches {
        let from_id = row.get(1).map(String::as_str).unwrap_or("");
        if from_id.is_empty() || bump_seen.contains(from_id) {
            continue;
        }
        newly_bumped.push(from_id.to_string());
    }
    if !newly_bumped.is_empty() {
        memory_signals::bump_hits(&mem_dir(), &newly_bumped);
        append_seen(&bump_seen_path, &newly_bumped);
    }

    // Once per file per session: the facts for a file do not change between
    // its edits, so repeating them only grows the history.
    let files_seen_path = Path::new(&dir).join("anchor-files-seen.tsv");
    if read_seen(&files_seen_path).contains(&relpath) {
        return;
    }
    append_seen(&files_seen_path, std::slice::from_ref(&relpath));

    let msg = cap_message(
        &format_message(&root, &relpath, &matches),
        EDIT_ANCHOR_CAP_CHARS,
    );
    emit_pre_context("PreToolUse", &msg);
}

/// Cuts `msg` to at most `cap` characters on a line boundary, replacing the
/// dropped lines with a count. The first line (the header) is always kept, and a
/// single over-long line is truncated rather than dropped.
fn cap_message(msg: &str, cap: usize) -> String {
    if msg.chars().count() <= cap {
        return msg.to_string();
    }
    let lines: Vec<&str> = msg.lines().collect();
    let mut out = String::new();
    let mut kept = 0;
    for line in &lines {
        let extra = usize::from(!out.is_empty()) + line.chars().count();
        // Reserve room for the trailing "omitted" note.
        if out.chars().count() + extra + 40 > cap && kept > 0 {
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
        kept += 1;
    }
    if out.chars().count() + 40 > cap {
        out = out.chars().take(cap.saturating_sub(40)).collect();
    }
    let dropped = lines.len() - kept;
    if dropped > 0 {
        out.push_str(&format!("\n(+{dropped} more anchored facts omitted)"));
    }
    out
}

fn mem_dir() -> PathBuf {
    memory_dir()
}

/// `UserPromptSubmit` branch: match prompt text and this-session touched
/// files against the same anchor index `PreToolUse` builds, inject the
/// matched facts' BODIES (not their names), deduped per session. Never
/// panics: a missing or unparsable index, or a fact whose `file` has been
/// deleted since the graph was last rebuilt, degrades to skipping that one
/// fact rather than aborting the whole match.
fn run_prompt(payload: &Payload, dir: &str) {
    let idx_path = Path::new(dir).join("memory-anchor-index.tsv");
    if !idx_path.exists() {
        build_index(&idx_path);
    }

    let Ok(metadata) = fs::metadata(&idx_path) else {
        return;
    };
    if metadata.len() == 0 {
        return;
    }
    let Ok(contents) = fs::read_to_string(&idx_path) else {
        return;
    };

    let mut prompt = payload.field(".prompt");
    if prompt.is_empty() {
        // The repo's own live code reads `.prompt` (auto_model_detect.rs),
        // but the official docs report `.user_prompt`; read both rather
        // than guess which name is real. See the ADR 0008 blueprint's
        // resolved open items for the citations behind this.
        prompt = payload.field(".user_prompt");
    }

    let root = git_toplevel();
    let query = PromptQuery::new(&prompt);
    let tokens = &query.tokens;
    let mut matches = prompt_token_matches(&contents, &query);
    let index = AnchorIndex::parse(&contents);
    for touched_abs in touched_paths(dir) {
        let relpath = repo_relative_path(&root, &touched_abs);
        if relpath.is_empty() {
            continue;
        }
        for row in index.matching_rows(&relpath) {
            let from_id = row.get(1).cloned().unwrap_or_default();
            if !matches.iter().any(|m| m.row.get(1) == Some(&from_id)) {
                matches.push(RecallMatch {
                    score: TOUCHED_FILE_SCORE,
                    strong: true,
                    path_scored: false,
                    row,
                });
            }
        }
    }
    if matches.is_empty() {
        emit_zero_match_hint(dir, tokens);
        return;
    }

    let seen_path = Path::new(dir).join("prompt-recall-seen.tsv");
    let seen = read_seen(&seen_path);
    let room = MAX_FACTS_PER_SESSION
        .saturating_sub(seen.len())
        .min(MAX_FACTS_PER_PROMPT);
    if room == 0 {
        return;
    }

    let mut newly_seen = Vec::new();
    let mut entries: Vec<String> = Vec::new();
    let mut unreadable = false;
    let mut used_chars = 0;
    let staleness_deadline = Instant::now() + STALENESS_BUDGET;
    for RecallMatch { row, .. } in &matches {
        if newly_seen.len() >= room {
            break;
        }
        let from_id = row.get(1).cloned().unwrap_or_default();
        if from_id.is_empty() || seen.contains(&from_id) {
            continue;
        }
        let name = row.get(2).cloned().unwrap_or_default();
        let desc = row.get(3).cloned().unwrap_or_default();
        let file = row.get(5).cloned().unwrap_or_default();
        if file.is_empty() {
            continue;
        }
        // A deleted or unreadable fact file is skipped, not fatal: the rest
        // of the matches still get their chance.
        let Some(body) = read_fact_body(&file) else {
            unreadable = true;
            continue;
        };
        // Past budget: skip the staleness check, not the recall itself.
        let note = if within_staleness_budget(staleness_deadline) {
            let anchor = row.first().cloned().unwrap_or_default();
            staleness_note(&root, &from_id, &anchor)
        } else {
            ""
        };
        let entry = format_recalled_fact(&name, &desc, note, &body, &file);
        if used_chars + entry.chars().count() > PROMPT_RECALL_CAP_CHARS {
            break;
        }
        used_chars += entry.chars().count();
        entries.push(entry);
        newly_seen.push(from_id);
    }
    if entries.is_empty() {
        // Matches existed but none could be read: the model got nothing.
        if unreadable {
            emit_zero_match_hint(dir, tokens);
        }
        return;
    }

    // Only ids that are genuinely new this session, not every raw match: a
    // fact already injected earlier this session and matched again should
    // not keep re-bumping every prompt for the rest of the session, or the
    // promotion threshold would be trivially easy to cross from repetition
    // within one session rather than genuine cross-session recurrence.
    memory_signals::bump_hits(&mem_dir(), &newly_seen);

    append_seen(&seen_path, &newly_seen);
    let mut msg = format!(
        "Recalled from memory, matching this prompt:\n\n{}",
        entries.join("\n\n")
    );
    // Strength is judged over every match, so a strong fact shown on an
    // earlier prompt still keeps the weak hint away.
    if !tokens.is_empty() && !matches.iter().any(|m| m.strong) {
        let hint = recall_hint(tokens, &repo_slug(), true);
        let fits = msg.chars().count() + 2 + hint.chars().count() <= PROMPT_RECALL_CAP_CHARS;
        if fits && reserve_hint_slot(dir) {
            msg.push_str("\n\n");
            msg.push_str(&hint);
        }
    }
    emit_prompt_context(&msg);
}

/// Prints the zero-match hint when the prompt has searchable terms and the
/// session still has hint budget. Silent otherwise.
fn emit_zero_match_hint(dir: &str, tokens: &[String]) {
    if tokens.is_empty() || !reserve_hint_slot(dir) {
        return;
    }
    emit_prompt_context(&recall_hint(tokens, &repo_slug(), false));
}

/// Counts one hint against the session budget. Fails closed: false when the
/// budget is spent, the counter cannot be read, or the new line cannot be
/// written, so a broken counter can never let hints repeat without bound.
fn reserve_hint_slot(dir: &str) -> bool {
    let path = Path::new(dir).join(HINTS_FILE);
    let used = match fs::read_to_string(&path) {
        Ok(text) => text.lines().count(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(_) => return false,
    };
    used < MAX_HINTS_PER_SESSION && try_append_seen(&path, &[HINT_MARK.to_string()])
}

/// One recalled fact: its name, its description, then the body, cut at
/// `BODY_CAP_CHARS` with a pointer to the file when it is longer.
fn format_recalled_fact(name: &str, desc: &str, note: &str, body: &str, file: &str) -> String {
    let mut out = format!("### {name}{note}");
    if !desc.is_empty() {
        out.push('\n');
        out.push_str(desc);
    }
    let body = strip_frontmatter(body).trim();
    if body.chars().count() > BODY_CAP_CHARS {
        let head: String = body.chars().take(BODY_CAP_CHARS).collect();
        out.push_str(&format!(
            "\n{head}\n[cut; full fact: ~/.config/playbook/memory/{file}]"
        ));
    } else if !body.is_empty() {
        out.push('\n');
        out.push_str(body);
    }
    out
}

/// `text` without a leading `---` frontmatter block, which the graph already
/// carries as the name and description.
fn strip_frontmatter(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("---\n") else {
        return text;
    };
    match rest.find("\n---") {
        Some(end) => rest[end + 4..].trim_start_matches(['\n', '\r']),
        None => text,
    }
}

/// Prompt words that can match: lowercased, trimmed of punctuation, at least
/// `MIN_TOKEN_CHARS` long, and not a stopword. Order and repeats are dropped.
fn prompt_tokens(prompt: &str) -> Vec<String> {
    let lower = prompt.to_lowercase();
    let mut tokens: Vec<String> = Vec::new();
    for word in lower.split(char::is_whitespace) {
        let word = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '-');
        if word.chars().count() < MIN_TOKEN_CHARS
            || STOPWORDS.binary_search(&word).is_ok()
            || tokens.iter().any(|t| t == word)
        {
            continue;
        }
        tokens.push(word.to_string());
    }
    tokens
}

/// Whether `token` matches `text`. A hyphenated token (a fact name pasted
/// from the prompt) matches as a substring. Any other token matches a word of
/// `text` that equals it, or, from 4 characters, one that extends it or that
/// it extends ("test" and "testing"), which skips the accidental hits a plain
/// substring scan made ("add" inside "address", "cli" inside "client").
fn token_matches(token: &str, text: &str) -> bool {
    if token.contains('-') {
        return text.contains(token);
    }
    text.split(|c: char| !c.is_alphanumeric()).any(|word| {
        word == token
            || (token.chars().count() >= 4
                && word.chars().count() >= 4
                && (word.starts_with(token) || token.starts_with(word)))
    })
}

/// Whether `token` sits in `text` with a non-alphanumeric character or an
/// edge on both sides, so "re-run" does not count inside "pre-run-hook".
fn contains_at_boundary(text: &str, token: &str) -> bool {
    text.match_indices(token).any(|(i, m)| {
        let before = text[..i].chars().next_back();
        let after = text[i + m.len()..].chars().next();
        before.is_none_or(|c| !c.is_alphanumeric()) && after.is_none_or(|c| !c.is_alphanumeric())
    })
}

/// The searchable parts of one prompt.
struct PromptQuery {
    /// Words that can match a fact name or description.
    tokens: Vec<String>,
    /// Words that name a crate, for matching the crate folder of an anchor.
    path_tokens: Vec<String>,
    /// The whole prompt, lowercased, for matching aliases as phrases.
    lower: String,
}

impl PromptQuery {
    fn new(prompt: &str) -> Self {
        Self {
            tokens: prompt_tokens(prompt),
            path_tokens: prompt_path_tokens(prompt),
            lower: prompt
                .to_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
}

/// Whether any alias in `aliases` appears in the lowercased `prompt` as a
/// whole word or phrase, so "pbcode" does not match "pbcodex".
fn alias_in_prompt(aliases: &str, prompt: &str) -> bool {
    aliases
        .split(ALIAS_SEPARATOR)
        .any(|a| !a.is_empty() && contains_at_boundary(prompt, a))
}

/// Add the alias column to each index row. Reads each fact file once; a file
/// that is missing or has no `aliases:` leaves the column empty.
fn append_alias_column(rows: Vec<String>, mem_dir: &Path) -> Vec<String> {
    let mut cache: HashMap<String, String> = HashMap::new();
    rows.into_iter()
        .map(|row| {
            let file = row.split('\t').nth(FACT_FILE_COLUMN).unwrap_or("");
            let aliases = cache
                .entry(file.to_string())
                .or_insert_with(|| read_alias_cell(mem_dir, file))
                .clone();
            format!("{row}\t{aliases}")
        })
        .collect()
}

/// The aliases of one fact file as an index cell: lowercased, whitespace
/// folded (so no tab, newline or separator survives), joined with `|`.
fn read_alias_cell(mem_dir: &Path, file: &str) -> String {
    if file.is_empty() {
        return String::new();
    }
    let Ok(content) = fs::read_to_string(mem_dir.join(file)) else {
        return String::new();
    };
    crate::hooks::rebuild_memory_graph::fact_aliases(&content)
        .iter()
        .map(|a| {
            a.to_lowercase()
                .replace(ALIAS_SEPARATOR, " ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|a| !a.is_empty())
        .collect::<Vec<_>>()
        .join(&ALIAS_SEPARATOR.to_string())
}

/// Words directly before `crate` or `crates`, stopwords included ("code" in
/// "PB code crate"). Only these can name a crate, so a bare "src" or "docs"
/// in a prompt never reaches an anchor path.
fn prompt_path_tokens(prompt: &str) -> Vec<String> {
    let lower = prompt.to_lowercase();
    let words: Vec<&str> = lower
        .split(char::is_whitespace)
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '-'))
        .collect();
    let mut out: Vec<String> = Vec::new();
    for pair in words.windows(2) {
        let (before, next) = (pair[0], pair[1]);
        if !before.is_empty()
            && matches!(next, "crate" | "crates")
            && !out.iter().any(|t| t == before)
        {
            out.push(before.to_string());
        }
    }
    out
}

/// The crate folder name of an anchor that is the crate folder itself
/// (`crates/code` or `crates/code/`). Anchors on files or folders inside the
/// crate (`src`, `main.rs`) give `None`, so a prompt naming a crate does not
/// pull in every fact anchored somewhere under it.
fn anchor_crate_name(anchor: &str) -> Option<&str> {
    let mut segments = anchor.split('/');
    segments.find(|s| *s == CRATES_DIR)?;
    let name = segments.next().filter(|s| !s.is_empty())?;
    segments.all(str::is_empty).then_some(name)
}

/// A fact row that matched the prompt or a touched file. `strong` marks a
/// match specific enough to trust: a hyphenated name token, two distinct name
/// tokens, an alias hit, a crate-folder hit, or a touched-file anchor. Anything
/// else is weak.
struct RecallMatch {
    score: usize,
    strong: bool,
    /// Whether the crate-folder bonus is already in `score`.
    path_scored: bool,
    row: Vec<String>,
}

/// Rows whose name or description matches a prompt token, best first: each
/// distinct token that hits counts one, two when it hits the name. An alias
/// phrase in the prompt and a crate-folder anchor named before "crate" add
/// their own bonus and make the match strong. Ties keep index order. One match
/// per from_id; a later anchor row of the same fact can add the crate-folder
/// bonus to it.
fn prompt_token_matches(idx_contents: &str, query: &PromptQuery) -> Vec<RecallMatch> {
    let tokens = &query.tokens;
    let mut scored: Vec<RecallMatch> = Vec::new();
    let mut seen_from: HashMap<String, usize> = HashMap::new();
    for line in idx_contents.lines() {
        if line.is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        let name = cols.get(2).copied().unwrap_or("").to_lowercase();
        let desc = cols.get(3).copied().unwrap_or("").to_lowercase();
        let mut score = 0;
        let mut name_hit_tokens: Vec<&str> = Vec::new();
        let mut hyphenated_name_hit = false;
        for token in tokens {
            if token_matches(token, &name) {
                score += NAME_HIT_SCORE;
                // Tokens that extend one another ("hook", "hooks") hit one
                // concept, so only unrelated tokens count toward strength.
                if !name_hit_tokens
                    .iter()
                    .any(|t| t.starts_with(token.as_str()) || token.starts_with(t))
                {
                    name_hit_tokens.push(token);
                }
                hyphenated_name_hit |= token.contains('-') && contains_at_boundary(&name, token);
            } else if token_matches(token, &desc) {
                score += 1;
            }
        }
        let path_hit = cols
            .first()
            .and_then(|anchor| anchor_crate_name(&anchor.to_lowercase()).map(str::to_string))
            .is_some_and(|name| query.path_tokens.contains(&name));
        if path_hit {
            score += PATH_HIT_SCORE;
        }
        let alias_hit = cols
            .get(ALIAS_COLUMN)
            .is_some_and(|aliases| alias_in_prompt(aliases, &query.lower));
        if alias_hit {
            score += ALIAS_HIT_SCORE;
        }
        if score == 0 {
            continue;
        }
        let from_id = cols.get(1).copied().unwrap_or("").to_string();
        if from_id.is_empty() {
            continue;
        }
        // Another anchor row of a fact already scored: only the crate-folder
        // hit differs per row, so let it upgrade the first row's match.
        if let Some(&at) = seen_from.get(&from_id) {
            if path_hit && !scored[at].path_scored {
                scored[at].strong = true;
                scored[at].path_scored = true;
                scored[at].score += PATH_HIT_SCORE;
            }
            continue;
        }
        seen_from.insert(from_id, scored.len());
        scored.push(RecallMatch {
            score,
            path_scored: path_hit,
            strong: path_hit
                || alias_hit
                || hyphenated_name_hit
                || name_hit_tokens.len() >= STRONG_NAME_TOKENS,
            row: cols.iter().map(|s| s.to_string()).collect(),
        });
    }
    scored.sort_by_key(|m| std::cmp::Reverse(m.score));
    scored
}

/// The one-line note for a prompt that found no fact (`weak == false`) or only
/// weak ones. It names the searched terms, the repo's fact folder to grep, and
/// the capture step. Bounded by `MAX_HINT_TERMS_CHARS` and `MAX_HINT_REPO_CHARS`, so it stays under 400 characters whatever the terms or slug.
fn recall_hint(terms: &[String], repo: &str, weak: bool) -> String {
    let joined = terms
        .iter()
        .take(MAX_HINT_TERMS)
        .map(|t| t.chars().take(MAX_HINT_TERM_CHARS).collect::<String>())
        .collect::<Vec<_>>()
        .join(", ");
    let joined: String = joined.chars().take(MAX_HINT_TERMS_CHARS).collect();
    let slug: String = repo
        .trim_matches('/')
        .chars()
        .take(MAX_HINT_REPO_CHARS)
        .collect();
    let folder = if slug.is_empty() {
        "~/.config/playbook/memory/".to_string()
    } else {
        format!("~/.config/playbook/memory/{slug}/")
    };
    let lead = if weak {
        "only weak matches for"
    } else {
        "no fact matched"
    };
    format!(
        "Memory: {lead} \"{joined}\". Before you say you do not know, grep {folder} and the \
         repo for these terms. If the answer comes from elsewhere, offer to save a fact."
    )
}

/// Absolute paths touched this session, from `edits.jsonl`, in file order.
/// Same record shape `post_edit_track.rs` writes and `memory_capture.rs`'s
/// `unique_paths_recent_first` reads; duplicated here rather than shared
/// cross-module, since a hook binary keeps its reads local. Repeats are
/// dropped, keeping first-seen order. Never panics: a
/// missing, empty, or unreadable file yields an empty list, and any line
/// that fails to parse or lacks a string `path` is skipped.
fn touched_paths(dir: &str) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(Path::new(dir).join("edits.jsonl")) else {
        return Vec::new();
    };
    let mut seen: HashSet<String> = HashSet::new();
    contents
        .lines()
        .filter_map(|raw| {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return None;
            }
            serde_json::from_str::<Value>(trimmed)
                .ok()?
                .get("path")?
                .as_str()
                .map(str::to_string)
        })
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

/// Fact ids already injected this session, one per line. Never panics: a
/// missing or unreadable marker is treated as "nothing seen yet".
fn read_seen(path: &Path) -> HashSet<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Appends newly injected fact ids to the session's dedup marker. Creates
/// the file on first write; a fresh session gets a fresh `session_dir`
/// (keyed by `session_id`), so no explicit reset is needed.
fn append_seen(path: &Path, ids: &[String]) {
    let _ = try_append_seen(path, ids);
}

/// Like `append_seen` but reports whether the lines reached the file, for
/// callers that must not act on a write that failed.
fn try_append_seen(path: &Path, ids: &[String]) -> bool {
    use std::io::Write;
    let lines: String = ids.iter().map(|id| format!("{id}\n")).collect();
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(lines.as_bytes()))
        .is_ok()
}

/// Reads a fact's markdown body from `~/.config/playbook/memory/<file>` (`file` is
/// relative to that root, per `rebuild_memory_graph.rs`'s node construction).
/// Read whole and cut later by `format_recalled_fact`, so one huge fact cannot
/// dominate a turn. `None` on any read failure
/// (deleted, unreadable): the caller skips this one fact rather than
/// treating it as fatal.
fn read_fact_body(file: &str) -> Option<String> {
    let path = memory_dir().join(file);
    let contents = fs::read_to_string(path).ok()?;
    Some(contents)
}

/// Anchors in the graph are repo-relative paths; the tool gives us an
/// (usually absolute) `file_path`, so strip the git worktree root off it. No
/// worktree root, or a `file_path` outside it: fall back to stripping a
/// single leading slash, matching the retired shell original.
fn repo_relative_path(root: &str, raw_path: &str) -> String {
    let prefix = format!("{root}/");
    if !root.is_empty() {
        if let Some(stripped) = raw_path.strip_prefix(&prefix) {
            return stripped.to_string();
        }
    }
    raw_path.strip_prefix('/').unwrap_or(raw_path).to_string()
}

/// The anchor index parsed once: rows split into columns, plus a map from
/// anchor text to row numbers so a lookup does not rescan every line.
struct AnchorIndex<'a> {
    rows: Vec<Vec<&'a str>>,
    by_anchor: HashMap<&'a str, Vec<usize>>,
}

impl<'a> AnchorIndex<'a> {
    fn parse(idx_contents: &'a str) -> Self {
        let mut rows = Vec::new();
        let mut by_anchor: HashMap<&str, Vec<usize>> = HashMap::new();
        for line in idx_contents.lines().filter(|l| !l.is_empty()) {
            let cols: Vec<&str> = line.split('\t').collect();
            by_anchor
                .entry(cols.first().copied().unwrap_or(""))
                .or_default()
                .push(rows.len());
            rows.push(cols);
        }
        AnchorIndex { rows, by_anchor }
    }

    /// Match the exact repo-relative path first, then any anchor that is a
    /// containing directory of it (an anchor of `src/` matches an edit to
    /// `src/deep/b.py`). Deduplicated by the anchoring fact's node id
    /// (column 1), keeping the first row for each, in index order.
    fn matching_rows(&self, relpath: &str) -> Vec<Vec<String>> {
        let mut hits: Vec<usize> = Vec::new();
        let mut collect = |key: &str| {
            if let Some(found) = self.by_anchor.get(key) {
                hits.extend(found);
            }
        };
        collect(relpath);
        for (i, _) in relpath.match_indices('/') {
            collect(&relpath[..i]);
            collect(&relpath[..=i]);
        }
        hits.sort_unstable();
        hits.dedup();
        let mut seen_from: HashSet<&str> = HashSet::new();
        let mut matches = Vec::new();
        for n in hits {
            let cols = &self.rows[n];
            if seen_from.insert(cols.get(1).copied().unwrap_or("")) {
                matches.push(cols.iter().map(|s| s.to_string()).collect());
            }
        }
        matches
    }
}

fn matching_rows(idx_contents: &str, relpath: &str) -> Vec<Vec<String>> {
    AnchorIndex::parse(idx_contents).matching_rows(relpath)
}

fn format_message(root: &str, relpath: &str, matches: &[Vec<String>]) -> String {
    let mut msg = format!("Memory facts anchored to {relpath}:");
    for cols in matches {
        let name = cols.get(2).map(String::as_str).unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let desc = cols.get(3).map(String::as_str).unwrap_or("");
        let neigh = cols.get(4).map(String::as_str).unwrap_or("");
        let from_id = cols.get(1).map(String::as_str).unwrap_or("");
        let anchor = cols.first().map(String::as_str).unwrap_or("");
        let mut line = format!("- {name}");
        if !desc.is_empty() {
            line = format!("{line}: {desc}");
        }
        if !neigh.is_empty() {
            line = format!("{line} ({neigh})");
        }
        line.push_str(staleness_note(root, from_id, anchor));
        msg.push('\n');
        msg.push_str(&line);
    }
    msg
}

/// Appends a soft note if `anchor_relpath` drifted since `from_id` was last
/// touched. An empty `root`, `from_id`, or anchor just skips the marker.
fn staleness_note(root: &str, from_id: &str, anchor_relpath: &str) -> &'static str {
    if root.is_empty() || anchor_relpath.is_empty() || from_id.is_empty() {
        return "";
    }
    let anchor_path = Path::new(root).join(anchor_relpath);
    let result = check_staleness(&mem_dir(), from_id, &anchor_path, &git_last_commit_epoch);
    if result.stale {
        " (may be stale: the anchor changed since this fact was last touched)"
    } else {
        ""
    }
}

/// The default `git_lookup`: the anchor's last commit date, or `None` if
/// untracked, uncommitted, or the clone has no history for that path.
fn git_last_commit_epoch(path: &Path) -> Option<staleness::DateTime> {
    let dir = path.parent()?;
    let path = path.to_string_lossy();
    let args = [
        "--no-optional-locks",
        "log",
        "-1",
        "--format=%ct",
        "--",
        &path,
    ];
    crate::common::git::output(dir, &args)?.parse::<i64>().ok()
}

/// Build the tab-separated anchor index from `memory.db` for the current
/// repo scope. One row per in-scope `anchors` edge, columns anchor, from_id,
/// name, description, neighbours, file, aliases. The `file` column exists solely for
/// `run_prompt`'s body reads; `PreToolUse`'s `format_message` only reads
/// columns 2 through 4, so this addition is safe for that path. Any failure
/// (missing or malformed graph)
/// yields an empty index rather than an error, so a stale or absent graph
/// still leaves this hook silent instead of breaking the edit.
fn build_index(idx_path: &Path) {
    let repo = repo_slug();
    let rows = compute_index_rows(&memory_dir(), &repo);
    write_index_atomically(idx_path, &rows);
}

fn compute_index_rows(mem_dir: &Path, repo: &str) -> Vec<String> {
    let Some(content) = crate::hooks::rebuild_memory_graph::graph_text(mem_dir) else {
        return Vec::new();
    };
    let Ok(graph) = serde_json::from_str::<Value>(&content) else {
        return Vec::new();
    };

    let nodes = graph
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let edges = graph
        .get("edges")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let byid: HashMap<&str, &Value> = nodes
        .iter()
        .filter_map(|n| n.get("id").and_then(Value::as_str).map(|id| (id, n)))
        .collect();

    let inscope: HashSet<&str> = nodes
        .iter()
        .filter(|n| in_scope(n, repo))
        .filter_map(|n| n.get("id").and_then(Value::as_str))
        .collect();

    let neigh = collect_neighbours(&edges, &byid, &inscope);
    let mut rows = collect_anchor_rows(&edges, &byid, &inscope, &neigh);
    append_unanchored_fact_rows(&nodes, &inscope, &mut rows);
    append_alias_column(rows, mem_dir)
}

/// A fact with no `anchors` edge still needs a row, or `run_prompt`'s
/// keyword matching could never find it: not every useful fact (a
/// preference, a gotcha with no single owning file) is anchored to code.
/// Anchor column is left empty, which `matching_rows` (both `PreToolUse` and
/// `run_prompt`'s touched-file matching) never matches against a real
/// repo-relative path, so this addition is invisible to those two callers
/// and changes nothing about their existing behaviour.
fn append_unanchored_fact_rows(nodes: &[Value], inscope: &HashSet<&str>, rows: &mut Vec<String>) {
    let already: HashSet<String> = rows
        .iter()
        .filter_map(|r| r.split('\t').nth(1).map(str::to_string))
        .collect();
    for node in nodes {
        let Some(id) = node.get("id").and_then(Value::as_str) else {
            continue;
        };
        if !inscope.contains(id) || already.contains(id) {
            continue;
        }
        if node.get("scope").and_then(Value::as_str) == Some("code") {
            continue;
        }
        let name = node.get("name").and_then(Value::as_str).unwrap_or("");
        let desc = node
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .replace('\n', " ");
        let file = node.get("file").and_then(Value::as_str).unwrap_or("");
        rows.push(format!("\t{id}\t{name}\t{desc}\t\t{file}"));
    }
}

fn in_scope(node: &Value, repo: &str) -> bool {
    match node.get("scope").and_then(Value::as_str) {
        Some("global") => true,
        Some("org") => node.get("project").and_then(Value::as_str) == repo.split('/').next(),
        Some("project") => node.get("project").and_then(Value::as_str) == Some(repo),
        _ => false,
    }
}

/// `depends_on`/`contradicts` neighbours, keyed by source node id, in the
/// order their edges are stored in `memory.db`.
fn collect_neighbours<'a>(
    edges: &'a [Value],
    byid: &HashMap<&'a str, &'a Value>,
    inscope: &HashSet<&'a str>,
) -> HashMap<&'a str, Vec<(String, String)>> {
    let mut neigh: HashMap<&str, Vec<(String, String)>> = HashMap::new();
    for edge in edges {
        let relation = edge.get("relation").and_then(Value::as_str).unwrap_or("");
        if relation != "depends_on" && relation != "contradicts" {
            continue;
        }
        let Some(from) = edge.get("from").and_then(Value::as_str) else {
            continue;
        };
        if !inscope.contains(from) {
            continue;
        }
        let to = edge.get("to").and_then(Value::as_str).unwrap_or("");
        let name = byid
            .get(to)
            .and_then(|n| n.get("name"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(to);
        neigh
            .entry(from)
            .or_default()
            .push((relation.to_string(), name.to_string()));
    }
    neigh
}

fn collect_anchor_rows(
    edges: &[Value],
    byid: &HashMap<&str, &Value>,
    inscope: &HashSet<&str>,
    neigh: &HashMap<&str, Vec<(String, String)>>,
) -> Vec<String> {
    let mut rows = Vec::new();
    for edge in edges {
        if edge.get("relation").and_then(Value::as_str) != Some("anchors") {
            continue;
        }
        let Some(from) = edge.get("from").and_then(Value::as_str) else {
            continue;
        };
        if !inscope.contains(from) {
            continue;
        }
        let Some(f_node) = byid.get(from) else {
            continue;
        };
        let Some(to) = edge.get("to").and_then(Value::as_str) else {
            continue;
        };
        let Some(c_node) = byid.get(to) else {
            continue;
        };
        let cfile = c_node.get("file").and_then(Value::as_str).unwrap_or("");
        if cfile.is_empty() {
            continue;
        }
        let anchor = cfile.split('#').next().unwrap_or("");
        let name = f_node.get("name").and_then(Value::as_str).unwrap_or("");
        let desc = f_node
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .replace('\n', " ");
        let nb = neigh
            .get(from)
            .map(|pairs| {
                pairs
                    .iter()
                    .map(|(rel, nm)| format!("{rel}:{nm}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let ffile = f_node.get("file").and_then(Value::as_str).unwrap_or("");
        rows.push(format!("{anchor}\t{from}\t{name}\t{desc}\t{nb}\t{ffile}"));
    }
    rows
}

/// Write `rows` to `idx_path` via a temp file in the same directory plus a
/// rename, so a concurrent reader never observes a partially written index.
/// Mirrors `_build_index`'s `<idx>.tmp.<pid>` plus `os.replace`.
fn write_index_atomically(idx_path: &Path, rows: &[String]) {
    let mut content = String::new();
    for row in rows {
        content.push_str(row);
        content.push('\n');
    }
    let _ = crate::common::atomic::write_atomic(idx_path, content);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ceiling for one recall hint, in characters (about 100 tokens).
    const HINT_CAP_CHARS: usize = 400;

    #[test]
    fn cap_message_keeps_short_text_and_cuts_long_text_on_a_line() {
        assert_eq!(cap_message("a\nb", 600), "a\nb");
        let long = format!("head:\n{}", "- some fact line here\n".repeat(60));
        let capped = cap_message(&long, 600);
        assert!(capped.chars().count() <= 600);
        assert!(capped.starts_with("head:\n- some fact"));
        assert!(capped.contains("more anchored facts omitted"));
        let huge_header = "x".repeat(2000);
        assert!(cap_message(&huge_header, 600).chars().count() <= 600);
    }

    #[test]
    fn staleness_budget_is_three_seconds() {
        // Arrange / Act / Assert: pins the chosen constant so a future
        // change to it is a deliberate edit, not an accidental drift.
        assert_eq!(STALENESS_BUDGET, Duration::from_secs(3));
    }

    #[test]
    fn within_budget_before_deadline() {
        // Arrange
        let deadline = Instant::now() + Duration::from_secs(10);

        // Act
        let got = within_staleness_budget(deadline);

        // Assert
        assert!(
            got,
            "a deadline well in the future must still be within budget"
        );
    }

    #[test]
    fn not_within_budget_after_deadline() {
        // Arrange: a deadline already in the past, as the staleness loop
        // would see once the aggregate budget has been spent by earlier,
        // slow per-fact checks.
        let deadline = Instant::now() - Duration::from_secs(1);

        // Act
        let got = within_staleness_budget(deadline);

        // Assert
        assert!(
            !got,
            "an expired deadline must report over budget, so the caller skips \
             the next per-fact staleness check instead of starting another \
             potentially slow git call"
        );
    }

    /// A small index shaped like the real store: every fact has a name and a
    /// description, and the descriptions use everyday words.
    const RECALL_INDEX: &str =
        "\tg/a\tavoid-flaky-tests\tUse when a test fails at random in CI\t\ta.md\n\
        \tg/b\tcli-flag-parsing\tUse when you add a flag to the cli\t\tb.md\n\
        \tg/c\tclient-retries\tUse when the http client retries a request\t\tc.md\n\
        \tg/d\taddress-parsing\tUse when you change how an address is parsed\t\td.md\n\
        \tg/e\tcommit-signing\tUse when you commit and the signature fails\t\te.md\n\
        \tg/f\tthis-does-that\tWhat this does and what that does\t\tf.md\n";

    fn matched_names(prompt: &str) -> Vec<String> {
        scored(RECALL_INDEX, prompt)
            .into_iter()
            .map(|m| m.row[2].clone())
            .collect()
    }

    fn scored(index: &str, prompt: &str) -> Vec<RecallMatch> {
        prompt_token_matches(index, &PromptQuery::new(prompt))
    }

    /// Four anchored rows in the `RECALL_INDEX` shape: a crate folder, two files
    /// inside another crate, and a file outside `crates/`.
    const PATH_INDEX: &str = "crates/code/\tproject/a\tcode-index\tsource index\t\ta.md\n\
        crates/cli/src/x.rs\tproject/b\tcli-helper\tlives in crates\t\tb.md\n\
        crates/cli/src/main.rs\tproject/c\tentry-point\tthe binary start\t\tc.md\n\
        src/lib.rs\tproject/d\tlib-root\tthe library root\t\td.md\n";

    #[test]
    fn a_word_before_crate_matches_the_crate_folder_and_ranks_first() {
        // Act
        let got = scored(PATH_INDEX, "PB code crate");

        // Assert
        assert_eq!(got[0].row[1], "project/a");
        assert!(got[0].strong, "a crate-name path hit is a strong match");
    }

    #[test]
    fn path_tokens_are_only_the_words_before_crate_or_crates() {
        assert_eq!(prompt_path_tokens("PB code crate"), terms(&["code"]));
        assert_eq!(prompt_path_tokens("the Data, crates"), terms(&["data"]));
        assert!(prompt_path_tokens("fix the test").is_empty());
        assert!(prompt_path_tokens("crate").is_empty());
    }

    #[test]
    fn a_prompt_with_no_crate_word_gets_no_path_hit() {
        // "src" is a real path segment but is not named before "crate"
        let got = scored(PATH_INDEX, "fix the bug in src");
        assert!(got.is_empty(), "got {:?}", got.len());
    }

    #[test]
    fn the_word_crate_alone_matches_no_path() {
        let got = scored(PATH_INDEX, "crate");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].row[1], "project/b");
        assert!(!got[0].strong);
    }

    #[test]
    fn a_path_token_does_not_make_a_file_or_folder_segment_a_strong_hit() {
        // "main" is a segment of crates/cli/src/main.rs but not the crate name
        let main = scored(PATH_INDEX, "main crate");
        assert!(main.iter().all(|m| !m.strong));
        let src = scored(PATH_INDEX, "src crate");
        assert!(src.iter().all(|m| !m.strong));
        let rs = scored(PATH_INDEX, "rs crate");
        assert!(rs.iter().all(|m| !m.strong));
    }

    #[test]
    fn a_path_hit_on_a_later_anchor_row_of_the_same_fact_is_kept() {
        // One fact, two anchors: the first row hits only a description word
        let index = "docs/a.md\tproject/a\tsome-notes\tabout the index\t\ta.md\n\
            crates/code/\tproject/a\tsome-notes\tabout the index\t\ta.md\n";
        let got = scored(index, "code crate index");
        assert_eq!(got.len(), 1);
        assert!(got[0].strong, "the crate-folder row must make it strong");
    }

    #[test]
    fn a_file_inside_a_crate_is_not_a_crate_folder_hit() {
        // PATH_INDEX anchors project/b at crates/cli/src/x.rs
        let got = scored(PATH_INDEX, "the cli crate");
        assert!(got.iter().all(|m| !m.strong));
    }

    #[test]
    fn the_crate_bonus_is_added_once_whatever_the_row_order() {
        let file_first = "crates/cli/src/x.rs\tproject/a\tnotes\tabout the index\t\ta.md\n\
            crates/code/\tproject/a\tnotes\tabout the index\t\ta.md\n";
        let dir_first = "crates/code/\tproject/a\tnotes\tabout the index\t\ta.md\n\
            crates/cli/src/x.rs\tproject/a\tnotes\tabout the index\t\ta.md\n";
        let a = scored(file_first, "code crate index");
        let b = scored(dir_first, "code crate index");
        assert_eq!(a[0].score, b[0].score);
    }

    #[test]
    fn a_crate_name_never_matches_by_prefix() {
        // "cod" is a prefix of the crate "code"
        let got = scored(PATH_INDEX, "cod crate");
        assert!(got.iter().all(|m| !m.strong));
    }

    /// Same shape as `PATH_INDEX` plus a seventh column of aliases.
    const ALIAS_INDEX: &str =
        "crates/code/\tproject/a\tcode-index\tsource index\t\ta.md\tpbcode|pb code\n\
        \tproject/b\tunrelated-fact\tsomething else\t\tb.md\t\n\
        \tproject/c\tsix-columns\tno alias column here\t\tc.md\n";

    #[test]
    fn an_alias_in_the_prompt_is_a_strong_match() {
        for prompt in ["what does pbcode do", "the pb code crate", "PBCODE?"] {
            let got = scored(ALIAS_INDEX, prompt);
            assert_eq!(got[0].row[1], "project/a", "{prompt}");
            assert!(got[0].strong, "{prompt}");
        }
    }

    #[test]
    fn an_alias_of_short_or_stop_words_matches_without_other_tokens() {
        // "pb" is under the token length and "code" is a stopword, so the
        // prompt has no searchable token; the alias must still match.
        let index = "\tproject/a\tnotes\tnothing here\t\ta.md\tpb code\n";
        let got = scored(index, "pb code");
        assert_eq!(got.len(), 1);
        assert!(got[0].strong);
    }

    #[test]
    fn an_alias_matches_across_a_newline_or_double_space() {
        let index = "\tproject/a\tnotes\tnothing here\t\ta.md\ttoken flow\n";
        for prompt in ["about token\nflow", "about token  flow"] {
            let got = scored(index, prompt);
            assert_eq!(got.len(), 1, "{prompt:?}");
        }
    }

    #[test]
    fn an_alias_matches_on_word_boundaries_only() {
        let got = scored(ALIAS_INDEX, "what does pbcodex do");
        assert!(got.iter().all(|m| m.row[1] != "project/a"));
        let got = scored(ALIAS_INDEX, "the xpb code");
        assert!(got.iter().all(|m| m.row[1] != "project/a"));
    }

    #[test]
    fn a_six_column_row_still_parses_and_scores() {
        let got = scored(ALIAS_INDEX, "tell me about six-columns");
        assert_eq!(got[0].row[1], "project/c");
        assert!(got[0].strong);
    }

    #[test]
    fn an_empty_alias_column_matches_nothing() {
        let got = scored(ALIAS_INDEX, "something");
        assert!(got.iter().all(|m| !m.strong));
    }

    #[test]
    fn a_missing_fact_file_leaves_the_alias_column_empty() {
        let dir = std::env::temp_dir().join(format!("pb-alias-missing-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let rows = vec!["a\tg/a\tn\td\t\tmissing.md".to_string()];
        let got = append_alias_column(rows, &dir);
        assert_eq!(got, vec!["a\tg/a\tn\td\t\tmissing.md\t"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_alias_column_holds_lowercased_aliases_without_tabs() {
        let dir = std::env::temp_dir().join(format!("pb-alias-read-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("f.md"),
            "---\nname: f\naliases: [PBCode, pb | code]\n---\nbody\n",
        )
        .unwrap();
        let rows = vec!["a\tg/a\tn\td\t\tf.md".to_string()];
        let got = append_alias_column(rows, &dir);
        assert_eq!(got, vec!["a\tg/a\tn\td\t\tf.md\tpbcode|pb code"]);
        let _ = fs::remove_dir_all(&dir);
    }

    fn terms(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    /// The text between the first pair of double quotes in `hint`.
    fn quoted_terms(hint: &str) -> &str {
        hint.split('"').nth(1).unwrap_or("")
    }

    #[test]
    fn a_zero_match_hint_names_the_terms_the_folder_and_the_next_step() {
        // Arrange
        let words = terms(&["crate", "code"]);

        // Act
        let hint = recall_hint(&words, "pragmatic-engineer/playbook", false);

        // Assert
        assert!(hint.starts_with("Memory: no fact matched \"crate, code\""));
        assert!(hint.contains("~/.config/playbook/memory/pragmatic-engineer/playbook/"));
        assert!(hint.contains("grep"));
        assert!(hint.contains("offer to save a fact"));
        assert!(hint.chars().count() <= HINT_CAP_CHARS);
    }

    #[test]
    fn a_hint_names_at_most_six_terms_and_stays_under_the_cap() {
        // Arrange
        let words: Vec<String> = (0..20).map(|n| format!("{n:x<40}")).collect();

        // Act
        let hint = recall_hint(&words, "pragmatic-engineer/playbook", false);

        // Assert
        assert!(quoted_terms(&hint).split(", ").count() <= MAX_HINT_TERMS);
        assert!(hint.chars().count() <= HINT_CAP_CHARS);
    }

    #[test]
    fn a_weak_hint_says_so() {
        // Arrange
        let words = terms(&["crate"]);

        // Act
        let hint = recall_hint(&words, "o/r", true);

        // Assert
        assert!(hint.starts_with("Memory: only weak matches for \"crate\""));
    }

    #[test]
    fn a_very_long_repo_slug_cannot_push_the_hint_past_the_cap() {
        // Arrange
        let repo = format!("{}/{}", "o".repeat(300), "r".repeat(300));
        let words: Vec<String> = (0..20).map(|n| format!("{n:x<40}")).collect();

        // Act
        let hint = recall_hint(&words, &repo, true);

        // Assert
        assert!(hint.chars().count() <= HINT_CAP_CHARS);
        assert!(hint.ends_with("offer to save a fact."), "{hint}");
    }

    #[test]
    fn an_empty_repo_slug_points_at_the_memory_root_without_a_double_slash() {
        // Arrange
        let words = terms(&["zebra"]);

        // Act
        let hint = recall_hint(&words, "", false);

        // Assert
        assert!(!hint.contains("memory//"), "{hint}");
        assert!(hint.contains("grep ~/.config/playbook/memory/ and the repo"));
    }

    #[test]
    fn one_plain_token_on_a_name_is_a_weak_match() {
        // Arrange
        let index = "\tg/a\trust-monorepo-crates-layout\tHow the workspace is laid out\t\ta.md\n";

        // Act
        let got = scored(index, "PB code crate");

        // Assert
        assert_eq!(got.len(), 1);
        assert!(!got[0].strong);
    }

    #[test]
    fn two_distinct_name_tokens_or_one_hyphenated_token_is_a_strong_match() {
        // Arrange
        let index = "\tg/a\talpha-beta-guide\tA guide\t\ta.md\n";

        // Act
        let two_tokens = scored(index, "alpha beta");
        let hyphenated = scored(index, "alpha-beta-guide");

        // Assert
        assert!(two_tokens[0].strong);
        assert!(hyphenated[0].strong);
    }

    #[test]
    fn tokens_that_extend_one_another_count_as_one_name_hit() {
        // Arrange
        let index = "\tg/a\tpre-run-hooks-guide\tA guide\t\ta.md\n";

        // Act
        let got = scored(index, "hook hooks");

        // Assert
        assert_eq!(got.len(), 1);
        assert!(!got[0].strong);
    }

    #[test]
    fn a_hyphenated_token_inside_a_longer_name_part_is_not_strong() {
        // Arrange
        let index = "\tg/a\tpre-run-hook\tA guide\t\ta.md\n";

        // Act
        let inside = scored(index, "re-run");
        let whole = scored(index, "pre-run");

        // Assert
        assert!(!inside[0].strong);
        assert!(whole[0].strong);
    }

    #[test]
    fn a_name_hit_scores_two_and_a_description_hit_scores_one() {
        // Arrange / Act
        let by_name = scored(RECALL_INDEX, "signing");
        let by_description = scored(RECALL_INDEX, "signature");

        // Assert
        assert_eq!(by_name[0].score, 2);
        assert_eq!(by_description[0].score, 1);
    }

    #[test]
    fn a_description_only_match_is_weak() {
        // Arrange / Act
        let got = scored(RECALL_INDEX, "signature");

        // Assert
        assert!(!got[0].strong);
    }

    #[test]
    fn stopwords_are_sorted_and_unique_so_binary_search_is_valid() {
        // Arrange / Act / Assert
        assert!(STOPWORDS.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn generic_prompts_match_almost_nothing() {
        // Arrange: the three prompts that matched 180, 181 and 61 of 261 facts.
        let prompts = [
            "fix the failing test",
            "add a flag to the cli",
            "what does this do",
        ];

        // Act
        let counts: Vec<usize> = prompts.iter().map(|p| matched_names(p).len()).collect();

        // Assert
        assert!(counts.iter().all(|n| *n < 4), "{counts:?}");
    }

    #[test]
    fn a_short_word_matches_a_whole_word_not_a_substring() {
        // Arrange / Act
        let names = matched_names("the cli");

        // Assert: "cli" is in "cli-flag-parsing" but not inside "client-retries".
        assert_eq!(names, vec!["cli-flag-parsing"]);
    }

    #[test]
    fn a_stem_matches_its_longer_form() {
        // Arrange / Act
        let names = matched_names("signing problems");

        // Assert: "signing" hits the name; "problems" hits nothing.
        assert_eq!(names, vec!["commit-signing"]);
    }

    #[test]
    fn matches_are_ranked_by_how_many_prompt_words_hit() {
        // Arrange / Act: "commit" hits e (name) and "signature" hits e (desc).
        let names = matched_names("commit signature http");

        // Assert: e scores 3, c scores 1.
        assert_eq!(names, vec!["commit-signing", "client-retries"]);
    }

    #[test]
    fn a_hyphenated_fact_name_matches_as_a_whole() {
        // Arrange / Act
        let names = matched_names("why does avoid-flaky-tests apply");

        // Assert
        assert_eq!(names, vec!["avoid-flaky-tests"]);
    }

    #[test]
    fn a_recalled_fact_carries_description_and_a_capped_body() {
        // Arrange
        let body = format!("---\nname: x\n---\n{}", "b".repeat(BODY_CAP_CHARS + 50));

        // Act
        let out = format_recalled_fact("x", "Use when y", "", &body, "x.md");

        // Assert
        assert!(out.starts_with("### x\nUse when y\n"));
        assert!(!out.contains("name: x"), "frontmatter should be stripped");
        assert!(out.contains("[cut; full fact: ~/.config/playbook/memory/x.md]"));
        assert!(out.chars().count() < BODY_CAP_CHARS + 200);
    }

    #[test]
    fn a_short_body_is_kept_whole() {
        // Arrange / Act
        let out = format_recalled_fact("x", "", "", "short body", "x.md");

        // Assert
        assert_eq!(out, "### x\nshort body");
    }

    /// The pre-index linear scan, kept as the oracle for equivalence.
    fn reference_matching_rows(idx: &str, relpath: &str) -> Vec<Vec<String>> {
        let mut out = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for line in idx.lines().filter(|l| !l.is_empty()) {
            let cols: Vec<&str> = line.split('\t').collect();
            let anchor = cols.first().copied().unwrap_or("");
            let dirp = if anchor.ends_with('/') {
                anchor.to_string()
            } else {
                format!("{anchor}/")
            };
            if (anchor == relpath || relpath.starts_with(&dirp))
                && seen.insert(cols.get(1).copied().unwrap_or("").to_string())
            {
                out.push(cols.iter().map(|s| s.to_string()).collect());
            }
        }
        out
    }

    const SAMPLE_INDEX: &str = "src/a.rs\tf1\tone\td\t\tf1.md\n\
        src/\tf2\ttwo\td\t\tf2.md\n\
        src\tf3\tthree\td\t\tf3.md\n\
        src/deep/b.py\tf1\tdup\td\t\tf1.md\n\
        \tf4\tunanchored\td\t\tf4.md\n\
        docs//\tf5\tdouble\td\t\tf5.md\n\
        README.md\tf6\tsix\td\t\tf6.md\n";

    #[test]
    fn index_lookup_matches_the_linear_scan_for_every_shape() {
        // Arrange
        let paths = [
            "src/a.rs",
            "src/deep/b.py",
            "src/x",
            "src",
            "docs//x",
            "docs/x",
            "README.md",
            "other/file",
            "/abs/path",
            "",
        ];

        // Act, Assert
        for path in paths {
            assert_eq!(
                matching_rows(SAMPLE_INDEX, path),
                reference_matching_rows(SAMPLE_INDEX, path),
                "path {path:?}"
            );
        }
    }

    #[test]
    fn touched_paths_drops_repeats_and_keeps_first_seen_order() {
        // Arrange
        let dir = crate::common::test_support::scratch_dir("anchors-touched");
        fs::create_dir_all(&dir).unwrap();
        let lines = ["/r/b.rs", "/r/a.rs", "/r/b.rs", "/r/a.rs", "/r/c.rs"]
            .map(|p| format!("{{\"path\":\"{p}\"}}\n"))
            .concat();
        fs::write(dir.join("edits.jsonl"), lines).unwrap();

        // Act
        let got = touched_paths(dir.to_str().unwrap());

        // Assert
        assert_eq!(got, ["/r/b.rs", "/r/a.rs", "/r/c.rs"]);
    }

    #[test]
    fn many_lookups_against_a_large_index_find_every_row() {
        // Arrange
        let idx: String = (0..2000)
            .map(|i| format!("src/f{i}.rs\tid{i}\tname\tdesc\t\tf{i}.md\n"))
            .collect();
        let index = AnchorIndex::parse(&idx);

        // Act
        let hits: usize = (0..20_000)
            .map(|i| index.matching_rows(&format!("src/f{}.rs", i % 4000)).len())
            .sum();

        // Assert
        assert_eq!(hits, 10_000);
    }
}
