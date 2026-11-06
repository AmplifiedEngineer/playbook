// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `context`: retrieval and formatting for a task described in words. It
//! splits terms, finds symbols and text, widens text hits to their enclosing
//! declaration, adds imports and tests, merges overlaps and fills a byte
//! budget. Nothing here calls a model.

use super::search::{fts_hits, load, Loaded, SearchOptions};
use super::source::cut_at_boundary;
use playbook_store::{params, Connection};
use std::collections::HashMap;
use std::path::Path;

pub const DEFAULT_MAX_TOKENS: usize = 4000;
#[cfg(test)]
pub const MIN_TOKENS: usize = 200;
/// The byte ceiling never exceeds this, whatever the budget.
pub const ABS_CEILING: usize = 256 * 1024;
/// Longest single snippet, in lines.
const MAX_SNIPPET_LINES: usize = 120;
/// Files that get their imports added.
const TOP_FILES: usize = 3;
/// Rows read per lookup.
const LOOKUP_LIMIT: usize = 200;
/// Omitted items listed in the output.
const MAX_LISTED_OMITTED: usize = 50;

/// A documentation candidate keeps this share of its score.
const DOC_SCORE_FACTOR: f64 = 0.5;

const STOPWORDS: &[&str] = &[
    "a", "an", "the", "of", "to", "in", "on", "for", "and", "or", "with", "from", "by", "is",
    "are", "be", "this", "that", "it", "as", "at", "how", "what", "where", "when", "why", "add",
    "make", "use", "get", "do", "does", "we", "i",
];

#[derive(Debug, Clone)]
pub struct ContextOptions {
    pub query: String,
    pub max_tokens: usize,
    /// Replaces the byte ceiling, for tests.
    pub ceiling: Option<usize>,
}

impl ContextOptions {
    pub fn new(query: &str) -> Self {
        Self {
            query: query.to_string(),
            max_tokens: DEFAULT_MAX_TOKENS,
            ceiling: None,
        }
    }

    /// Bytes allowed in the output: four per token, never above the absolute ceiling.
    pub fn byte_ceiling(&self) -> usize {
        self.ceiling
            .unwrap_or_else(|| (self.max_tokens * 4).min(ABS_CEILING))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub reason: String,
    pub tier: u8,
    pub text: String,
    pub stale: bool,
}

/// A candidate that did not fit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Omitted {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct ContextResult {
    pub terms: Vec<String>,
    pub snippets: Vec<Snippet>,
    pub omitted: Vec<Omitted>,
    /// How many candidates did not fit, which may exceed the listed `omitted`.
    pub omitted_count: usize,
    /// Candidates whose file could no longer be read.
    pub unreadable: usize,
    pub truncated: bool,
    pub bytes: usize,
}

/// Lowercase words of the query without stopwords, each once, in order.
pub fn terms(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in super::search::tokens(query) {
        let lower = word.to_lowercase();
        if STOPWORDS.contains(&lower.as_str()) || out.contains(&lower) {
            continue;
        }
        out.push(lower);
    }
    out
}

#[derive(Debug, Clone)]
struct Cand {
    path: String,
    sha: String,
    start: usize,
    end: usize,
    reason: String,
    tier: u8,
}

fn sql_err(e: playbook_store::Error) -> String {
    super::db::map_sql_error("context failed", &e)
}

fn to_usize(n: i64) -> usize {
    usize::try_from(n).unwrap_or(0)
}

/// A symbol that belongs to tests: in a test path, inside a `tests` or `test`
/// module segment, or named `test_*`.
fn is_test_symbol(path: &str, qualified: &str, name: &str) -> bool {
    is_test_path(path)
        || qualified
            .split(['.', ':'])
            .any(|seg| matches!(seg, "tests" | "test"))
        || name.starts_with("test_")
}

/// Symbols whose name holds `term`, with a tier from how well the name fits,
/// and how many symbols of the worktree hold it. The scan reads the name index
/// and scopes only the matches through `files` (`CROSS JOIN` keeps that
/// order), so it skips the per-symbol file-list probe of the scoped view. The
/// count comes from the same pass, as a window over the matches before the
/// limit.
fn symbol_cands(conn: &Connection, term: &str) -> Result<(Vec<Cand>, i64), String> {
    let pattern = format!("%{}%", term.replace('\\', "\\\\").replace('_', "\\_"));
    let mut stmt = conn
        .prepare(
            "SELECT f.path, f.sha256, s.name, s.qualified_name, s.start_line, s.end_line,
                    COUNT(*) OVER ()
             FROM (SELECT id FROM main.symbols WHERE lower(name) LIKE ?1 ESCAPE '\\') h
             CROSS JOIN main.symbols s ON s.id = h.id
             CROSS JOIN files f ON f.id = s.content
             ORDER BY (lower(s.name) = ?3) DESC, length(s.name), f.path, s.start_line, s.id
             LIMIT ?2",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![pattern, LOOKUP_LIMIT as i64, term], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                to_usize(r.get(4)?),
                to_usize(r.get(5)?),
                r.get::<_, i64>(6)?,
            ))
        })
        .map_err(sql_err)?;
    let mut out = Vec::new();
    let mut df = 0;
    for row in rows {
        let (path, sha, name, qualified, start, end, matches) = row.map_err(sql_err)?;
        df = matches;
        if is_test_symbol(&path, &qualified, &name) {
            continue;
        }
        let lower = name.to_lowercase();
        let pieces = super::index::identifier_pieces(&name);
        let tier = if lower == term {
            2
        } else if pieces.iter().any(|p| p == term) {
            4
        } else {
            6
        };
        out.push(Cand {
            path,
            sha,
            start,
            end,
            reason: format!("symbol {qualified}"),
            tier,
        });
    }
    Ok((out, df))
}

/// The smallest symbol in `path` that holds `line`, as a range.
fn enclosing(conn: &Connection, path: &str, line: usize) -> Result<Option<(usize, usize)>, String> {
    let line = i64::try_from(line).unwrap_or(i64::MAX);
    let mut stmt = conn
        .prepare(
            "SELECT s.start_line, s.end_line FROM symbols s JOIN files f ON f.id = s.file_id
             WHERE f.path = ?1 AND s.start_line <= ?2 AND s.end_line >= ?2
             ORDER BY (s.end_line - s.start_line), s.id LIMIT 1",
        )
        .map_err(sql_err)?;
    let mut rows = stmt
        .query_map(params![path, line], |r| {
            Ok((to_usize(r.get(0)?), to_usize(r.get(1)?)))
        })
        .map_err(sql_err)?;
    rows.next().transpose().map_err(sql_err)
}

fn sha_of(conn: &Connection, path: &str) -> String {
    conn.query_row("SELECT sha256 FROM files WHERE path = ?1", [path], |r| {
        r.get(0)
    })
    .unwrap_or_default()
}

/// Text hits for `words`, each widened to its enclosing declaration.
fn text_cands(
    conn: &Connection,
    root: &Path,
    words: &[String],
    reason: &str,
    fetch: usize,
) -> Result<Vec<Cand>, String> {
    if words.is_empty() {
        return Ok(Vec::new());
    }
    let mut opts = SearchOptions::new(&words.join(" "));
    opts.any_word = true;
    let (hits, _) = fts_hits(conn, root, &opts, fetch)?;
    let mut out = Vec::new();
    for hit in hits {
        let (start, end) = match enclosing(conn, &hit.path, hit.line)? {
            Some(range) => range,
            None => (hit.line.saturating_sub(3).max(1), hit.line + 3),
        };
        out.push(Cand {
            sha: sha_of(conn, &hit.path),
            path: hit.path,
            start,
            end,
            reason: reason.to_string(),
            tier: 6,
        });
    }
    Ok(out)
}

/// Merges overlapping ranges within a file. Reasons join, the best tier wins.
fn merge(mut cands: Vec<Cand>) -> Vec<Cand> {
    cands.sort_by(|a, b| (&a.path, a.start, a.end).cmp(&(&b.path, b.start, b.end)));
    let mut out: Vec<Cand> = Vec::new();
    for c in cands {
        if let Some(last) = out.last_mut() {
            let union = last.end.max(c.end) - last.start + 1;
            if last.path == c.path && c.start <= last.end && union <= MAX_SNIPPET_LINES {
                last.end = last.end.max(c.end);
                last.tier = last.tier.min(c.tier);
                if !last.reason.split("; ").any(|r| r == c.reason) {
                    last.reason = format!("{}; {}", last.reason, c.reason);
                }
                continue;
            }
        }
        out.push(c);
    }
    out
}

fn is_test_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    let mut parts = lower.split('/').collect::<Vec<_>>();
    let file = parts.pop().unwrap_or("");
    let in_test_dir = parts
        .iter()
        .any(|d| matches!(*d, "test" | "tests" | "spec" | "specs" | "__tests__"));
    let stem = file.split('.').next().unwrap_or("");
    let marked = file.contains(".test.")
        || file.contains(".spec.")
        || stem.ends_with("_test")
        || stem.ends_with("_spec")
        || stem.starts_with("test_");
    in_test_dir || marked
}

/// Import lines of `paths` that mention a term or a top symbol name.
fn import_cands(
    conn: &Connection,
    paths: &[String],
    needles: &[String],
) -> Result<Vec<Cand>, String> {
    let mut out = Vec::new();
    for path in paths {
        let mut stmt = conn
            .prepare(
                "SELECT i.module, i.names, i.line FROM imports i JOIN files f ON f.id = i.file_id
                 WHERE f.path = ?1 ORDER BY i.line",
            )
            .map_err(sql_err)?;
        let rows = stmt
            .query_map([path], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    to_usize(r.get(2)?),
                ))
            })
            .map_err(sql_err)?;
        for row in rows {
            let (module, names, line) = row.map_err(sql_err)?;
            let hay = format!("{} {}", module, names.unwrap_or_default()).to_lowercase();
            if needles.iter().any(|n| hay.contains(n.as_str())) {
                out.push(Cand {
                    path: path.clone(),
                    sha: sha_of(conn, path),
                    start: line,
                    end: line,
                    reason: "import".to_string(),
                    tier: 5,
                });
            }
        }
    }
    Ok(out)
}

/// How much each term tells apart: rare names weigh more than common ones.
/// `dfs` holds, per term, how many symbols of the worktree name it. The total
/// counts the worktree's symbols through `files`, a walk over the
/// `symbols_content` index. Two paths with the same bytes count twice, as
/// they did when each path had its own rows.
fn term_weights(conn: &Connection, dfs: &[i64]) -> Result<Vec<f64>, String> {
    let total: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM files f JOIN main.symbols t ON t.content = f.id",
            [],
            |r| r.get(0),
        )
        .map_err(sql_err)?;
    Ok(dfs
        .iter()
        .map(|df| (2.0 + total as f64 / (1.0 + *df as f64)).ln())
        .collect())
}

/// A documentation file: a prose extension or a `docs` or `doc` directory.
fn is_doc_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    let mut parts = lower.split('/').collect::<Vec<_>>();
    let file = parts.pop().unwrap_or("");
    let prose = [".md", ".mdx", ".rst", ".txt"]
        .iter()
        .any(|ext| file.ends_with(ext));
    prose || parts.iter().any(|d| matches!(*d, "docs" | "doc"))
}

/// Weighted count of the query terms a candidate holds in its file name, its
/// symbol names and its text. A term in a symbol name counts twice.
fn score(
    cand: &Cand,
    words: &[String],
    weights: &[f64],
    root: &Path,
    cache: &mut HashMap<String, Option<Loaded>>,
) -> f64 {
    let loaded = cache
        .entry(cand.path.clone())
        .or_insert_with(|| load(root, &cand.path));
    let body = loaded.as_ref().map_or(String::new(), |l| {
        let end = cand
            .end
            .min(l.lines.len())
            .min(cand.start + MAX_SNIPPET_LINES - 1);
        if cand.start == 0 || cand.start > end {
            String::new()
        } else {
            l.lines[cand.start - 1..end].join("\n")
        }
    });
    let file_name = cand.path.rsplit('/').next().unwrap_or(&cand.path);
    let haystack = format!("{file_name}\n{body}").to_lowercase();
    let names = cand.reason.to_lowercase();
    let total: f64 = words
        .iter()
        .zip(weights)
        .map(|(w, weight)| {
            let mut got = 0.0;
            if haystack.contains(w.as_str()) {
                got += weight;
            }
            if names.contains(w.as_str()) {
                got += weight;
            }
            got
        })
        .sum();
    if is_doc_path(&cand.path) {
        total * DOC_SCORE_FACTOR
    } else {
        total
    }
}

/// Candidates best first: score, then tier, path and line.
fn rank(cands: &[Cand], scores: &[f64]) -> Vec<Cand> {
    let mut idx: Vec<usize> = (0..cands.len()).collect();
    idx.sort_by(|&a, &b| {
        scores[b]
            .total_cmp(&scores[a])
            .then_with(|| cands[a].tier.cmp(&cands[b].tier))
            .then_with(|| cands[a].path.cmp(&cands[b].path))
            .then_with(|| cands[a].start.cmp(&cands[b].start))
    });
    idx.into_iter().map(|i| cands[i].clone()).collect()
}

/// Builds the context for `opts.query`.
pub fn build(
    conn: &Connection,
    root: &Path,
    opts: &ContextOptions,
) -> Result<ContextResult, String> {
    let words = terms(&opts.query);
    let mut result = ContextResult {
        terms: words.clone(),
        ..ContextResult::default()
    };
    if words.is_empty() {
        return Ok(result);
    }
    let mut cands: Vec<Cand> = Vec::new();
    let mut dfs: Vec<i64> = Vec::new();
    for word in &words {
        let (found, df) = symbol_cands(conn, word)?;
        cands.extend(found);
        dfs.push(df);
    }
    let text = text_cands(conn, root, &words, "text", 20)?;
    cands.extend(text.into_iter().filter(|c| !is_test_path(&c.path)));
    let merged = merge(cands);
    let weights = term_weights(conn, &dfs)?;
    let mut cache: HashMap<String, Option<Loaded>> = HashMap::new();
    let scores: Vec<f64> = merged
        .iter()
        .map(|c| score(c, &words, &weights, root, &mut cache))
        .collect();
    let mut ordered = rank(&merged, &scores);

    let mut top_paths: Vec<String> = Vec::new();
    for c in &ordered {
        if !top_paths.contains(&c.path) && top_paths.len() < TOP_FILES {
            top_paths.push(c.path.clone());
        }
    }
    let top_names: Vec<String> = ordered
        .iter()
        .flat_map(|c| c.reason.split("; "))
        .filter_map(|r| r.strip_prefix("symbol "))
        .take(3)
        .map(|q| q.rsplit(['.', ':']).next().unwrap_or(q).to_string())
        .collect();
    let mut needles = words.clone();
    needles.extend(top_names.iter().map(|n| n.to_lowercase()));

    let mut extra = import_cands(conn, &top_paths, &needles)?;
    let tests = text_cands(conn, root, &top_names, "test", 50)?;
    extra.extend(tests.into_iter().filter(|c| is_test_path(&c.path)));
    // An extra inside a snippet already chosen adds its reason to that snippet.
    let mut merged = merged;
    let mut extra: Vec<Cand> = merge(extra)
        .into_iter()
        .filter(|c| {
            let host = merged
                .iter_mut()
                .find(|p| p.path == c.path && p.start <= c.start && c.end <= p.end);
            match host {
                Some(p) => {
                    if !p.reason.split("; ").any(|r| r == c.reason) {
                        p.reason = format!("{}; {}", p.reason, c.reason);
                    }
                    false
                }
                None => true,
            }
        })
        .collect();
    extra.sort_by(|a, b| (a.tier, &a.path, a.start).cmp(&(b.tier, &b.path, b.start)));
    let scores: Vec<f64> = merged
        .iter()
        .map(|c| score(c, &words, &weights, root, &mut cache))
        .collect();
    ordered = rank(&merged, &scores);
    ordered.extend(extra);

    fill(&mut result, root, &ordered, opts.byte_ceiling());
    Ok(result)
}

/// Room kept for the closing `N more candidates did not fit` line.
const OMITTED_LINE_RESERVE: usize = 48;

fn header_for(path: &str, start: usize, end: usize, stale: bool, reason: &str) -> String {
    let stale = if stale { " (stale)" } else { "" };
    format!("== {path}:{start}-{end}{stale}  {reason}\n")
}

/// The line that introduces a snippet in the text output.
pub fn snippet_header(s: &Snippet) -> String {
    header_for(&s.path, s.start_line, s.end_line, s.stale, &s.reason)
}

fn omitted_line(count: usize) -> String {
    format!("{count} more candidates did not fit:\n")
}

/// Reads each candidate from disk in order until the byte ceiling is reached.
/// Each snippet is charged its header, its text and its newline, so the text
/// output fits the ceiling as a whole.
fn fill(result: &mut ContextResult, root: &Path, ordered: &[Cand], ceiling: usize) {
    let mut cache: HashMap<String, Option<Loaded>> = HashMap::new();
    let mut full = false;
    let mut used = 0usize;
    let mut stale_cache: HashMap<String, bool> = HashMap::new();
    for (idx, cand) in ordered.iter().enumerate() {
        let omit = |result: &mut ContextResult, cand: &Cand| {
            result.omitted_count += 1;
            if result.omitted.len() < MAX_LISTED_OMITTED {
                result.omitted.push(Omitted {
                    path: cand.path.clone(),
                    start_line: cand.start,
                    end_line: cand.end,
                    reason: cand.reason.clone(),
                });
            }
        };
        if full {
            omit(result, cand);
            continue;
        }
        let loaded = cache
            .entry(cand.path.clone())
            .or_insert_with(|| load(root, &cand.path));
        let Some(loaded) = loaded.as_ref() else {
            result.unreadable += 1;
            continue;
        };
        let end = cand
            .end
            .min(loaded.lines.len())
            .min(cand.start + MAX_SNIPPET_LINES - 1);
        if cand.start == 0 || cand.start > end {
            omit(result, cand);
            continue;
        }
        let stale = *stale_cache
            .entry(cand.path.clone())
            .or_insert_with(|| !loaded.same(&cand.sha));
        let header = header_for(&cand.path, cand.start, end, stale, &cand.reason);
        let text = loaded.lines[cand.start - 1..end].join("\n");
        let reserve = if idx + 1 < ordered.len() {
            OMITTED_LINE_RESERVE
        } else {
            0
        };
        let room = ceiling.saturating_sub(used + header.len() + 1 + reserve);
        let (text, cut) = if text.len() > room {
            (cut_at_boundary(&text, room).to_string(), true)
        } else {
            (text, false)
        };
        // A cut snippet ends on the last line it still shows.
        let end = if cut {
            cand.start + text.matches('\n').count()
        } else {
            end
        };
        if text.is_empty() {
            full = true;
            result.truncated = true;
            omit(result, cand);
            continue;
        }
        used += header.len() + text.len() + 1;
        result.bytes += text.len();
        result.snippets.push(Snippet {
            path: cand.path.clone(),
            start_line: cand.start,
            end_line: end,
            reason: cand.reason.clone(),
            tier: cand.tier,
            stale,
            text,
        });
        if cut || used >= ceiling {
            full = true;
            if cut {
                result.truncated = true;
            }
        }
    }
    // The closing line must fit too: give back the last snippets until it does.
    while result.omitted_count > 0
        && used + omitted_line(result.omitted_count).len() > ceiling
        && !result.snippets.is_empty()
    {
        let Some(last) = result.snippets.pop() else {
            break;
        };
        used -= snippet_header(&last).len() + last.text.len() + 1;
        result.bytes -= last.text.len();
        result.omitted_count += 1;
        if result.omitted.len() < MAX_LISTED_OMITTED {
            result.omitted.insert(
                0,
                Omitted {
                    path: last.path,
                    start_line: last.start_line,
                    end_line: last.end_line,
                    reason: last.reason,
                },
            );
        }
    }
    if result.omitted_count > 0 {
        result.truncated = true;
    }
}

/// The text form of a result, never longer than `ceiling` bytes. `escape`
/// makes a snippet safe to print. Omitted candidates are listed only while
/// they fit, and the last whole line within the ceiling is the final guard.
pub fn render_text(
    result: &ContextResult,
    ceiling: usize,
    escape: impl Fn(&str) -> String,
) -> String {
    if result.snippets.is_empty() {
        return "no matching code\n".to_string();
    }
    let mut text = String::new();
    for s in &result.snippets {
        text.push_str(&snippet_header(s));
        text.push_str(&escape(&s.text));
        text.push('\n');
    }
    if result.omitted_count > 0 {
        let closing = omitted_line(result.omitted_count);
        text.push_str(&closing);
        for o in &result.omitted {
            let line = format!(
                "  {}:{}-{}  {}\n",
                o.path, o.start_line, o.end_line, o.reason
            );
            if text.len() + line.len() > ceiling {
                break;
            }
            text.push_str(&line);
        }
    }
    if text.len() > ceiling {
        let kept = cut_at_boundary(&text, ceiling);
        let whole = kept.rfind('\n').map_or(0, |i| i + 1);
        text.truncate(whole);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{fx, pinned, Fx};

    /// Header and newline bytes around the single `retry` snippet of `a.ts`.
    const SNIPPET_OVERHEAD: usize = "== a.ts:1-3  symbol retry; text\n".len() + 1;

    fn ctx(f: &Fx, q: &str) -> ContextResult {
        build(&f.conn(), &f.root, &ContextOptions::new(q)).expect("context")
    }

    #[test]
    fn stopwords_are_dropped_and_terms_keep_their_order() {
        // Arrange, Act, Assert
        assert_eq!(terms("the payment of retries"), vec!["payment", "retries"]);
        assert_eq!(
            terms("add retry support to payment processing"),
            vec!["retry", "support", "payment", "processing"]
        );
    }

    #[test]
    fn the_first_snippets_are_the_matching_declarations() {
        let _g = pinned();
        // Arrange
        let f = fx("cx-first");
        f.write(
            "pay.ts",
            "export function processPayment() {\n  return 1;\n}\n",
        );
        f.write(
            "retry.ts",
            "export function withRetry() {\n  return 2;\n}\n",
        );
        f.write("other.ts", "export function unrelated() {}\n");
        f.run();

        // Act
        let got = ctx(&f, "add retry support to payment processing");

        // Assert
        let paths: Vec<&str> = got.snippets.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(paths[..2], ["pay.ts", "retry.ts"]);
        assert!(!paths.contains(&"other.ts"));
        assert!(got.snippets[0].reason.starts_with("symbol "));
    }

    #[test]
    fn overlapping_hits_in_one_function_merge_into_one_snippet() {
        let _g = pinned();
        // Arrange: the name matches `retry` and the body matches it as text too.
        let f = fx("cx-merge");
        f.write(
            "a.ts",
            "function retryLoop() {\n  // retry again\n  retry();\n}\n",
        );
        f.run();

        // Act
        let got = ctx(&f, "retry");

        // Assert
        assert_eq!(got.snippets.len(), 1);
        assert_eq!(
            (got.snippets[0].start_line, got.snippets[0].end_line),
            (1, 4)
        );
        assert!(got.snippets[0].reason.contains("symbol retryLoop"));
        assert!(got.snippets[0].reason.contains("text"));
    }

    #[test]
    fn imports_of_a_top_file_and_only_matching_tests_are_added() {
        let _g = pinned();
        // Arrange
        let f = fx("cx-extras");
        f.write(
            "pay.ts",
            "import { helper } from './paymentGateway';\nimport { other } from './unrelated';\n\n\n\n\n\n\n\nexport function processPayment() {}\n",
        );
        f.write(
            "pay.test.ts",
            "test('processPayment works', () => processPayment());\n",
        );
        f.write("misc.test.ts", "test('nothing', () => 1);\n");
        f.run();

        // Act
        let got = ctx(&f, "payment");

        // Assert
        let reasons: Vec<(&str, &str)> = got
            .snippets
            .iter()
            .map(|s| (s.path.as_str(), s.reason.as_str()))
            .collect();
        assert!(
            reasons
                .iter()
                .any(|(p, r)| *p == "pay.ts" && r.contains("import")),
            "{reasons:?}"
        );
        assert!(
            reasons
                .iter()
                .any(|(p, r)| *p == "pay.test.ts" && *r == "test"),
            "{reasons:?}"
        );
        assert!(
            !reasons.iter().any(|(p, _)| *p == "misc.test.ts"),
            "{reasons:?}"
        );
        let imports = got
            .snippets
            .iter()
            .filter(|s| s.reason.contains("import"))
            .count();
        assert_eq!(imports, 1);
    }

    #[test]
    fn the_budget_has_a_minimum_and_a_ceiling_of_four_bytes_per_token() {
        let _g = pinned();
        // Arrange
        let f = fx("cx-budget");
        for i in 0..6 {
            f.write(
                &format!("f{i}.ts"),
                &format!("function retry{i}() {{\n{}}}\n", "  work();\n".repeat(40)),
            );
        }
        f.run();
        let mut opts = ContextOptions::new("retry");
        opts.max_tokens = MIN_TOKENS;

        // Act
        let got = build(&f.conn(), &f.root, &opts).expect("context");

        // Assert
        assert!(got.bytes <= 800, "{}", got.bytes);
        assert!(!got.omitted.is_empty());
        assert!(got.truncated);
    }

    #[test]
    fn an_injected_ceiling_stops_output_at_1024_bytes() {
        let _g = pinned();
        for (body_bytes, truncated) in [(1023usize, false), (1024, false), (1025, true)] {
            // Arrange: one function whose snippet text is exactly `body_bytes` long.
            let f = fx(&format!("cx-ceiling-{body_bytes}"));
            let head = "function retry() {";
            let tail = "}";
            let filler = body_bytes - head.len() - tail.len() - 2;
            f.write("a.ts", &format!("{head}\n{}\n{tail}\n", "x".repeat(filler)));
            f.run();
            let mut opts = ContextOptions::new("retry");
            opts.ceiling = Some(1024 + SNIPPET_OVERHEAD);

            // Act
            let got = build(&f.conn(), &f.root, &opts).expect("context");

            // Assert
            assert_eq!(got.bytes, body_bytes.min(1024), "{body_bytes}");
            assert_eq!(got.truncated, truncated, "{body_bytes}");
        }
    }

    #[test]
    fn a_multi_term_query_finds_text_where_the_terms_sit_on_different_lines() {
        let _g = pinned();
        // Arrange
        let f = fx("cx-any-word");
        f.write(
            "notes.txt",
            "gather alpha here\nsome filler line\nand omega over there\n",
        );
        f.run();

        // Act
        let got = ctx(&f, "alpha omega");

        // Assert
        assert!(got
            .snippets
            .iter()
            .any(|s| s.path == "notes.txt" && s.reason == "text"));
    }

    #[test]
    fn a_cut_inside_a_multibyte_character_keeps_valid_text_and_a_true_end_line() {
        let _g = pinned();
        // Arrange: each e-acute is two bytes, so a 5 byte ceiling lands inside the third.
        let f = fx("cx-multibyte");
        f.write(
            "a.ts",
            "function retry() {\n  // \u{e9}\u{e9}\u{e9}\u{e9}\n}\n",
        );
        f.run();
        let mut opts = ContextOptions::new("retry");
        opts.ceiling = Some(25 + SNIPPET_OVERHEAD);

        // Act
        let got = build(&f.conn(), &f.root, &opts).expect("context");

        // Assert
        assert!(got.bytes <= 25, "{}", got.bytes);
        assert!(got.truncated);
        let s = &got.snippets[0];
        assert_eq!(s.end_line, s.start_line + s.text.matches('\n').count());
    }

    #[test]
    fn a_long_declaration_is_capped_at_120_lines() {
        let _g = pinned();
        // Arrange
        let f = fx("cx-line-cap");
        let body = "  work();\n".repeat(200);
        f.write("a.ts", &format!("function retry() {{\n{body}}}\n"));
        f.run();

        // Act
        let got = ctx(&f, "retry");

        // Assert
        let s = &got.snippets[0];
        assert_eq!(s.end_line - s.start_line + 1, 120);
    }

    #[test]
    fn paths_are_tests_by_directory_or_name_pattern_only() {
        // Arrange, Act, Assert
        for yes in [
            "tests/a.rs",
            "src/__tests__/a.ts",
            "a.test.ts",
            "b/a.spec.js",
            "x_test.go",
            "test_x.py",
        ] {
            assert!(is_test_path(yes), "{yes}");
        }
        for no in [
            "src/inspect.ts",
            "respect.py",
            "src/specification/a.rs",
            "latest.ts",
            "contest/a.rs",
        ] {
            assert!(!is_test_path(no), "{no}");
        }
    }

    const ANCHORS_RS: &str = "pub fn run_prompt(text: &str) -> usize {\n    // match prompt words to fact names\n    prompt_tokens(text).len()\n}\n\npub fn prompt_tokens(text: &str) -> Vec<String> {\n    text.split(' ').map(str::to_string).collect()\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn matched_names() {\n        // match names\n    }\n}\n";

    fn rank_fixture(tag: &str) -> Fx {
        let f = fx(tag);
        f.write("src/hooks/anchors.rs", ANCHORS_RS);
        f.write(
            "tests/anchors.rs",
            "#[test]\nfn match_names_in_prompt() {\n    // match names prompt\n}\n",
        );
        f
    }

    #[test]
    fn a_core_function_outranks_test_symbols_for_the_same_words() {
        let _g = pinned();
        // Arrange
        let f = rank_fixture("cx-rank-tests");
        f.run();

        // Act
        let got = ctx(&f, "how the memory hook matches prompt words to fact names");

        // Assert
        assert!(
            got.snippets[0].reason.contains("run_prompt"),
            "{:?}",
            got.snippets
        );
        let first_test = got
            .snippets
            .iter()
            .position(|s| s.reason.contains("tests::matched_names"));
        assert!(first_test.is_none_or(|i| i >= 2), "{:?}", got.snippets);
        assert!(got
            .snippets
            .iter()
            .take(2)
            .any(|s| s.reason.contains("prompt_tokens")));
    }

    #[test]
    fn the_rust_extractor_qualifies_test_module_members_with_the_module_name() {
        let _g = pinned();
        // Arrange
        let f = rank_fixture("cx-qualified");
        f.run();

        // Act
        let q: String = f
            .conn()
            .query_row(
                "SELECT qualified_name FROM symbols WHERE name = 'matched_names'",
                [],
                |r| r.get(0),
            )
            .expect("row");

        // Assert
        assert_eq!(q, "tests::matched_names");
    }

    #[test]
    fn test_symbols_are_told_apart_by_path_module_segment_or_name_prefix() {
        // Arrange, Act, Assert
        for (path, qualified, name, want) in [
            ("src/a.rs", "tests::x", "x", true),
            ("src/a.rs", "a.test.helper", "helper", true),
            ("src/a.rs", "test_parse", "test_parse", true),
            ("src/tests/x.rs", "x", "x", true),
            ("src/a.rs", "contest", "contest", false),
            ("src/a.rs", "latest_value", "latest_value", false),
        ] {
            assert_eq!(is_test_symbol(path, qualified, name), want, "{qualified}");
        }
    }

    #[test]
    fn a_short_exact_name_survives_the_candidate_cap() {
        let _g = pinned();
        // Arrange: 250 longer names sort before the one we want by path.
        let f = fx("cx-cap");
        let many: String = (0..250)
            .map(|n| format!("pub fn aaa_prompt_{n}() {{}}\n"))
            .collect();
        f.write("src/a.rs", &many);
        f.write("src/z.rs", "pub fn run_prompt() {}\n");
        f.run();

        // Act
        let (got, _) = symbol_cands(&f.conn(), "prompt").expect("cands");

        // Assert
        assert!(got.iter().any(|c| c.reason == "symbol run_prompt"));
    }

    #[test]
    fn a_directory_word_and_an_adr_do_not_outrank_core_code() {
        let _g = pinned();
        // Arrange
        let f = rank_fixture("cx-dir-adr");
        let padding = "    let _ = text.len();\n".repeat(20);
        f.write(
            "src/hooks/anchors.rs",
            &ANCHORS_RS.replacen(
                "    prompt_tokens(text).len()\n",
                &format!("{padding}    prompt_tokens(text).len()\n"),
                1,
            ),
        );
        f.write("src/hooks/other.rs", "pub fn unrelated_thing() {}\n");
        f.write(
            "docs/adr/0001-anchors.md",
            "# Anchors\n\nHow the memory hook matches prompt words to fact names.\n",
        );
        f.run();

        // Act
        let got = ctx(&f, "how the memory hook matches prompt words to fact names");

        // Assert
        let pos = |needle: &str| got.snippets.iter().position(|s| s.path.contains(needle));
        let run = pos("hooks/anchors.rs").expect("core snippet");
        assert_eq!(run, 0, "{:?}", got.snippets);
        if let Some(other) = pos("hooks/other.rs") {
            assert!(other > run);
        }
        let adr = pos("0001-anchors.md").expect("adr snippet");
        assert!(adr > run);
        let noise: usize = got
            .snippets
            .iter()
            .filter(|s| is_test_path(&s.path) || is_doc_path(&s.path))
            .map(|s| s.text.len())
            .sum();
        assert!(noise * 5 <= got.bytes, "{noise} of {}", got.bytes);
    }

    #[test]
    fn doc_paths_are_prose_files_or_docs_directories_only() {
        // Arrange, Act, Assert
        for yes in ["docs/x.rs", "README.md", "a/doc/b.py", "notes.txt", "x.rst"] {
            assert!(is_doc_path(yes), "{yes}");
        }
        for no in ["src/doctor.rs", "src/docs.rs", "src/a.rs"] {
            assert!(!is_doc_path(no), "{no}");
        }
    }

    #[test]
    fn a_file_name_holding_the_word_outscores_a_directory_holding_it() {
        let _g = pinned();
        // Arrange
        let f = fx("cx-file-name");
        let body = "pub fn wire() {}\n";
        f.write("src/hook.rs", body);
        f.write("src/hooks/x.rs", body);
        f.run();
        let words = vec!["hook".to_string()];
        let weights = vec![1.0];
        let cand = |path: &str| Cand {
            path: path.to_string(),
            sha: String::new(),
            start: 1,
            end: 1,
            reason: "text".to_string(),
            tier: 6,
        };
        let mut cache = HashMap::new();

        // Act
        let named = score(&cand("src/hook.rs"), &words, &weights, &f.root, &mut cache);
        let dir = score(
            &cand("src/hooks/x.rs"),
            &words,
            &weights,
            &f.root,
            &mut cache,
        );

        // Assert
        assert!(named > dir, "{named} vs {dir}");
    }

    fn candidates(n: usize) -> ContextResult {
        let mut r = ContextResult::default();
        for i in 0..n {
            r.snippets.push(Snippet {
                path: format!("src/f{i}.rs"),
                start_line: 1,
                end_line: 3,
                reason: "symbol run".to_string(),
                tier: 2,
                text: "fn run() {\n    work();\n}".to_string(),
                stale: false,
            });
        }
        r
    }

    #[test]
    fn rendered_text_stays_within_the_ceiling_and_ends_on_the_omitted_line() {
        // Arrange
        let f = fx("cx-render-many");
        for i in 0..40 {
            f.write(
                &format!("src/f{i}.ts"),
                &format!("function retry{i}() {{\n{}}}\n", "  work();\n".repeat(6)),
            );
        }
        f.run();
        let mut opts = ContextOptions::new("retry");
        opts.ceiling = Some(1024);
        let _g = pinned();

        // Act
        let got = build(&f.conn(), &f.root, &opts).expect("context");
        let text = render_text(&got, 1024, str::to_string);

        // Assert
        assert!(!text.is_empty());
        assert!(text.len() <= 1024, "{}", text.len());
        assert!(got.omitted_count > 0);
        assert!(text.contains("more candidates did not fit:\n"));
    }

    #[test]
    fn one_large_snippet_is_cut_to_a_header_and_whole_lines_within_the_ceiling() {
        let _g = pinned();
        // Arrange
        let f = fx("cx-render-big");
        f.write(
            "a.ts",
            &format!("function retry() {{\n{}}}\n", "  work();\n".repeat(200)),
        );
        f.run();
        let mut opts = ContextOptions::new("retry");
        opts.ceiling = Some(800);

        // Act
        let got = build(&f.conn(), &f.root, &opts).expect("context");
        let text = render_text(&got, 800, str::to_string);

        // Assert
        assert!(text.starts_with("== a.ts:1-"), "{text}");
        assert!(text.len() <= 800, "{}", text.len());
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn a_result_with_no_snippets_renders_the_no_match_line() {
        // Arrange, Act
        let text = render_text(&ContextResult::default(), 100, str::to_string);

        // Assert
        assert_eq!(text, "no matching code\n");
    }

    #[test]
    fn an_escape_that_doubles_every_byte_still_fits_the_ceiling() {
        // Arrange
        let mut r = candidates(3);
        r.omitted_count = 2;

        // Act
        let text = render_text(&r, 120, |t| t.repeat(2));

        // Assert
        assert!(text.len() <= 120, "{}", text.len());
        assert!(text.is_empty() || text.ends_with('\n'));
    }

    #[test]
    fn term_weights_count_a_declaration_once_per_path() {
        let _g = pinned();
        // Arrange
        let f = fx("cx-df-per-path");
        f.write("a.rs", "fn widget() {}\n");
        f.write("b.rs", "fn widget() {}\n");
        f.write("c.rs", "fn other() {}\n");
        f.run();
        let conn = f.conn();

        // Act
        let (_, df) = symbol_cands(&conn, "widget").expect("cands");
        let weights = term_weights(&conn, &[df]).expect("weights");

        // Assert: three symbols in total, two of them named widget.
        let expected = (2.0 + 3.0 / (1.0 + 2.0_f64)).ln();
        assert!((weights[0] - expected).abs() < 1e-9, "{weights:?}");
    }

    #[test]
    fn term_weights_ignore_the_symbols_of_a_sibling_worktree() {
        let _g = pinned();
        // Arrange
        let alone = fx("cx-weights-alone");
        let shared = fx("cx-weights-shared");
        for f in [&alone, &shared] {
            f.write("a.rs", "fn widget() {}\n");
            f.write("b.rs", "fn widget() {}\nfn other() {}\n");
            f.run();
        }
        let sibling = crate::walker::test_repo::init("cx-weights-sibling");
        for i in 0..50 {
            crate::walker::test_repo::write(
                &sibling,
                &format!("w{i}.rs"),
                format!("fn widget() {{}}\nfn only{i}() {{}}\n").as_bytes(),
            );
        }
        crate::index::run(
            &sibling,
            &shared.index,
            "sibling",
            false,
            &crate::index::Options::default(),
        )
        .expect("sibling");

        // Act
        let weigh = |f: &Fx| {
            let conn = f.conn();
            let (_, df) = symbol_cands(&conn, "widget").expect("cands");
            term_weights(&conn, &[df]).expect("weights")
        };

        // Assert
        assert_eq!(weigh(&shared), weigh(&alone));
    }

    #[test]
    fn a_damaged_symbol_table_names_rebuild() {
        let _g = pinned();
        // Arrange
        let f = fx("context-damaged");
        f.write("a.rs", "fn needle() {}\n");
        f.run();
        let conn = f.conn();
        crate::testkit::damage_pages(&f);

        // Act
        let got = build(&conn, &f.root, &ContextOptions::new("needle"));

        // Assert
        let Err(message) = got else {
            panic!("the query must fail")
        };
        assert!(message.contains("--rebuild"), "{message}");
    }
}
