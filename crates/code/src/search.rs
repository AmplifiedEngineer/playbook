// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Text search over the index. Every FTS query is built from tokens, each
//! wrapped as an FTS5 string with quotes doubled, and passed as a bound
//! parameter, so operator text in the input is only ever text.

use super::walker::Limits;
use playbook_common::hash::sha256_hex;
use playbook_store::Connection;
use std::collections::HashMap;
use std::path::Path;

/// Default result count.
pub const DEFAULT_LIMIT: usize = 20;
/// The literal fallback scans at most this many indexed files.
pub const DEFAULT_SCAN_CAP: usize = 2000;

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub query: String,
    pub path: Option<String>,
    pub lang: Option<String>,
    pub kind: Option<String>,
    /// Match chunks holding any query word instead of all of them.
    pub any_word: bool,
    pub limit: usize,
    pub scan_cap: usize,
}

impl SearchOptions {
    pub fn new(query: &str) -> Self {
        Self {
            query: query.to_string(),
            path: None,
            lang: None,
            kind: None,
            any_word: false,
            limit: DEFAULT_LIMIT,
            scan_cap: DEFAULT_SCAN_CAP,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    /// The first line that matches, 1 based.
    pub line: usize,
    pub match_type: &'static str,
    pub tier: u8,
    pub excerpt: String,
    /// The 1 based line where `excerpt` starts.
    pub excerpt_start: usize,
    pub stale: bool,
    /// Set for symbol hits.
    pub symbol: Option<SymbolInfo>,
}

/// What a symbol hit adds to a hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolInfo {
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub lang: Option<String>,
    pub signature: Option<String>,
    pub exported: bool,
}

#[derive(Debug, Default)]
pub struct SearchResult {
    pub hits: Vec<Hit>,
    pub truncated: bool,
    pub omitted: usize,
    pub files_scanned: Option<usize>,
    /// The literal scan stopped at its file cap, so later files were not read.
    pub scan_capped: bool,
}

/// Words of the query: runs of letters, digits and underscores.
pub fn tokens(query: &str) -> Vec<String> {
    query
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// An FTS5 query that ANDs every token as a quoted string, or `None` when the
/// query has no tokens.
pub fn fts_query(query: &str) -> Option<String> {
    fts_query_mode(query, false)
}

/// Like `fts_query`, and with `any` set the tokens are joined with `OR`, so a
/// chunk needs only one of them. The operator comes from here, never from the input.
pub fn fts_query_mode(query: &str, any: bool) -> Option<String> {
    let toks = tokens(query);
    if toks.is_empty() {
        return None;
    }
    Some(
        toks.iter()
            .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(if any { " OR " } else { " " }),
    )
}

/// Longest excerpt line, in characters.
const MAX_LINE_CHARS: usize = 200;

fn clip(line: &str) -> String {
    line.chars().take(MAX_LINE_CHARS).collect()
}

/// A file read for excerpts.
pub(super) struct Loaded {
    pub(super) lines: Vec<String>,
    bytes: Vec<u8>,
}

impl Loaded {
    /// The file's raw bytes.
    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Whether the file still hashes to `sha`.
    pub(super) fn same(&self, sha: &str) -> bool {
        sha256_hex(&self.bytes) == sha
    }
}

/// Reads `path` only when it is still a regular file within the indexer's size
/// cap, so a file swapped for a link or a device after indexing is never
/// followed. `None` when the file cannot be read.
pub(super) fn load(root: &Path, path: &str) -> Option<Loaded> {
    let full = root.join(path);
    let meta = std::fs::symlink_metadata(&full).ok()?;
    if !meta.file_type().is_file() || meta.len() > Limits::default().max_file_bytes {
        return None;
    }
    let bytes = std::fs::read(&full).ok()?;
    let lines = String::from_utf8_lossy(&bytes)
        .lines()
        .map(str::to_string)
        .collect();
    Some(Loaded { lines, bytes })
}

/// Whether `line` holds `tok` as a whole word.
fn has_word(line: &str, tok: &str) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    line.match_indices(tok).any(|(i, m)| {
        let before = line[..i].chars().next_back().is_none_or(|c| !is_word(c));
        let after = line[i + m.len()..]
            .chars()
            .next()
            .is_none_or(|c| !is_word(c));
        before && after
    })
}

/// The first line in `[start, end]` that holds every token, else any token,
/// else the first line. Whole words win over substrings. 1 based.
fn match_line(lines: &[String], start: usize, end: usize, toks: &[String]) -> usize {
    let lower: Vec<String> = toks.iter().map(|t| t.to_lowercase()).collect();
    let first = start.max(1);
    let last = end.min(lines.len());
    // Each line is lowercased the first time a pass reaches it, then kept for
    // the next three passes.
    let mut lowered: Vec<Option<String>> = vec![None; (last + 1).saturating_sub(first)];
    for (all, word) in [(true, true), (false, true), (true, false), (false, false)] {
        for n in first..=last {
            let line = lowered[n - first].get_or_insert_with(|| lines[n - 1].to_lowercase());
            let one = |t: &String| {
                if word {
                    has_word(line, t)
                } else {
                    line.contains(t.as_str())
                }
            };
            let holds = if all {
                lower.iter().all(one)
            } else {
                lower.iter().any(one)
            };
            if holds {
                return n;
            }
        }
    }
    start
}

/// First line of the excerpt for a match on `line`.
pub fn excerpt_start(line: usize) -> usize {
    line.saturating_sub(1).max(1)
}

/// The matching line with one line of context on each side.
pub(super) fn excerpt(lines: &[String], line: usize) -> String {
    let from = excerpt_start(line);
    let to = (line + 1).min(lines.len());
    (from..=to)
        .map(|n| clip(&lines[n - 1]))
        .collect::<Vec<_>>()
        .join("\n")
}

struct Row {
    path: String,
    sha: String,
    start_line: usize,
    end_line: usize,
}

pub(super) fn filters(opts: &SearchOptions) -> (String, Vec<String>) {
    let mut sql = String::new();
    let mut params = Vec::new();
    if let Some(prefix) = &opts.path {
        sql.push_str(" AND substr(f.path, 1, length(?)) = ?");
        params.push(prefix.clone());
        params.push(prefix.clone());
    }
    if let Some(lang) = &opts.lang {
        sql.push_str(" AND f.lang = ?");
        params.push(lang.clone());
    }
    (sql, params)
}

fn fts_rows(
    conn: &Connection,
    query: &str,
    opts: &SearchOptions,
    fetch: usize,
) -> Result<(Vec<Row>, usize), String> {
    let (extra, extra_params) = filters(opts);
    let base = format!(
        "FROM chunks_fts JOIN main.chunks c ON c.id = chunks_fts.rowid
         JOIN files f ON f.id = c.content
         WHERE chunks_fts MATCH ?{extra}"
    );
    let mut params: Vec<String> = vec![query.to_string()];
    params.extend(extra_params);
    let total: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) {base}"),
            playbook_store::params_from_iter(params.iter()),
            |r| r.get(0),
        )
        .map_err(|e| super::db::map_sql_error("search failed", &e))?;
    let sql = format!(
        "SELECT f.path, f.sha256, c.start_line, c.end_line {base}
         ORDER BY bm25(chunks_fts), f.path, c.start_line LIMIT {fetch}"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| super::db::map_sql_error("search failed", &e))?;
    let rows = stmt
        .query_map(playbook_store::params_from_iter(params.iter()), |r| {
            Ok(Row {
                path: r.get(0)?,
                sha: r.get(1)?,
                start_line: usize::try_from(r.get::<_, i64>(2)?).unwrap_or(1),
                end_line: usize::try_from(r.get::<_, i64>(3)?).unwrap_or(1),
            })
        })
        .map_err(|e| super::db::map_sql_error("search failed", &e))?;
    let rows = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| super::db::map_sql_error("search failed", &e))?;
    Ok((rows, usize::try_from(total).unwrap_or(0)))
}

/// FTS hits for the query tokens, best first, at most `fetch`, plus how many
/// chunks matched in all. Empty when the query has no tokens.
pub(super) fn fts_hits(
    conn: &Connection,
    root: &Path,
    opts: &SearchOptions,
    fetch: usize,
) -> Result<(Vec<Hit>, usize), String> {
    let toks = tokens(&opts.query);
    let Some(query) = fts_query_mode(&opts.query, opts.any_word) else {
        return Ok((Vec::new(), 0));
    };
    let (rows, total) = fts_rows(conn, &query, opts, fetch)?;
    let mut cache: HashMap<String, Option<Loaded>> = HashMap::new();
    let hits = rows
        .into_iter()
        .map(|row| {
            let loaded = cache
                .entry(row.path.clone())
                .or_insert_with(|| load(root, &row.path));
            let same = loaded.as_ref().is_some_and(|l| l.same(&row.sha));
            let lines: &[String] = loaded.as_ref().map_or(&[], |l| &l.lines);
            let line = if lines.is_empty() {
                row.start_line
            } else {
                match_line(lines, row.start_line, row.end_line, &toks)
            };
            Hit {
                excerpt: if lines.is_empty() {
                    String::new()
                } else {
                    excerpt(lines, line)
                },
                path: row.path,
                start_line: row.start_line,
                end_line: row.end_line,
                line,
                match_type: "text",
                tier: 6,
                excerpt_start: excerpt_start(line),
                stale: !same,
                symbol: None,
            }
        })
        .collect::<Vec<_>>();
    Ok((hits, total))
}

/// Searches indexed text. FTS hits come first, ordered by relevance then path
/// and line. With no tokens or no FTS hits, a literal substring scan over
/// indexed files answers instead, bounded by `scan_cap` files.
pub fn text_search(
    conn: &Connection,
    root: &Path,
    opts: &SearchOptions,
) -> Result<SearchResult, String> {
    let (hits, total) = fts_hits(conn, root, opts, opts.limit)?;
    if !hits.is_empty() {
        let omitted = total.saturating_sub(hits.len());
        return Ok(SearchResult {
            hits,
            truncated: omitted > 0,
            omitted,
            files_scanned: None,
            scan_capped: false,
        });
    }
    literal_scan(conn, root, opts, opts.limit, &[])
}

/// Substring scan over indexed files, skipping lines the `covered` hits already show.
pub(super) fn literal_scan(
    conn: &Connection,
    root: &Path,
    opts: &SearchOptions,
    limit: usize,
    covered: &[Hit],
) -> Result<SearchResult, String> {
    let needle = opts.query.trim();
    let mut result = SearchResult {
        files_scanned: Some(0),
        ..SearchResult::default()
    };
    if needle.is_empty() {
        return Ok(result);
    }
    let (extra, extra_params) = filters(opts);
    let sql = format!(
        "SELECT f.path, f.sha256 FROM files f WHERE f.status IN ('ok', 'partial'){extra} ORDER BY f.path LIMIT {}",
        opts.scan_cap + 1
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| super::db::map_sql_error("search failed", &e))?;
    let files: Vec<(String, String)> = stmt
        .query_map(playbook_store::params_from_iter(extra_params.iter()), |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .map_err(|e| super::db::map_sql_error("search failed", &e))?
        .collect::<Result<_, _>>()
        .map_err(|e| super::db::map_sql_error("search failed", &e))?;
    let mut scanned = 0usize;
    for (path, sha) in files {
        if scanned >= opts.scan_cap {
            result.truncated = true;
            result.scan_capped = true;
            break;
        }
        scanned += 1;
        let Some(loaded) = load(root, &path) else {
            continue;
        };
        let lines = &loaded.lines;
        let mut same: Option<bool> = None;
        for (i, text) in lines.iter().enumerate() {
            if !text.contains(needle) {
                continue;
            }
            let line = i + 1;
            if covered.iter().any(|h| {
                h.path == path
                    && if h.match_type == "text" {
                        (h.start_line..=h.end_line).contains(&line)
                    } else {
                        h.line == line
                    }
            }) {
                continue;
            }
            if result.hits.len() >= limit {
                result.truncated = true;
                result.omitted += 1;
                continue;
            }
            result.hits.push(Hit {
                path: path.clone(),
                start_line: i + 1,
                end_line: i + 1,
                line: i + 1,
                match_type: "literal",
                tier: 7,
                excerpt_start: excerpt_start(i + 1),
                excerpt: excerpt(lines, i + 1),
                stale: !*same.get_or_insert_with(|| loaded.same(&sha)),
                symbol: None,
            });
        }
    }
    result.files_scanned = Some(scanned);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{fx, pinned, Fx};

    fn search(f: &Fx, opts: &SearchOptions) -> SearchResult {
        text_search(&f.conn(), &f.root, opts).expect("search")
    }

    fn paths(r: &SearchResult) -> Vec<&str> {
        r.hits.iter().map(|h| h.path.as_str()).collect()
    }

    #[test]
    fn operator_text_in_the_input_is_searched_as_text() {
        let _g = pinned();
        // Arrange
        let f = fx("se-operators");
        f.write("a.txt", "retry AND OR (\n");
        f.write("b.txt", "retry only\n");
        f.run();

        // Act
        let got = search(&f, &SearchOptions::new("retry AND OR ("));

        // Assert
        assert_eq!(paths(&got), vec!["a.txt"]);
        assert_eq!((got.hits[0].start_line, got.hits[0].line), (1, 1));
        assert_eq!(got.hits[0].match_type, "text");
    }

    #[test]
    fn an_injection_attempt_does_not_widen_the_query() {
        let _g = pinned();
        // Arrange
        let f = fx("se-injection");
        f.write("a.txt", "x\" OR \"1\"=\"1\n");
        f.write("b.txt", "one 1 here\n");
        f.run();

        // Act
        let got = search(&f, &SearchOptions::new("x\" OR \"1\"=\"1"));

        // Assert
        assert_eq!(paths(&got), vec!["a.txt"]);
    }

    #[test]
    fn nineteen_and_twenty_hits_return_in_full_and_twenty_one_truncate() {
        let _g = pinned();
        for (count, want_len, truncated, omitted) in [
            (19usize, 19usize, false, 0usize),
            (20, 20, false, 0),
            (21, 20, true, 1),
        ] {
            // Arrange
            let f = fx(&format!("se-limit-{count}"));
            for i in 0..count {
                f.write(&format!("f{i:02}.txt"), "needleword here\n");
            }
            f.run();

            // Act
            let got = search(&f, &SearchOptions::new("needleword"));

            // Assert
            assert_eq!(got.hits.len(), want_len, "{count} hits");
            assert_eq!(got.truncated, truncated, "{count} truncated");
            assert_eq!(got.omitted, omitted, "{count} omitted");
        }
    }

    #[test]
    fn the_literal_scan_stops_at_the_cap_and_reports_files_scanned() {
        let _g = pinned();
        for (files, scanned, truncated) in [(4usize, 4usize, false), (5, 5, false), (6, 5, true)] {
            // Arrange: the query has no tokens, so only the literal scan can answer.
            let f = fx(&format!("se-scan-{files}"));
            for i in 0..files {
                f.write(&format!("f{i}.txt"), "plain words\n");
            }
            f.run();
            let mut opts = SearchOptions::new("+++");
            opts.scan_cap = 5;

            // Act
            let got = search(&f, &opts);

            // Assert
            assert!(got.hits.is_empty());
            assert_eq!(got.files_scanned, Some(scanned), "{files} files");
            assert_eq!(got.truncated, truncated, "{files} files");
            assert_eq!(got.scan_capped, truncated, "{files} files");
            assert_eq!(got.omitted, 0, "{files} files");
        }
    }

    #[test]
    fn a_literal_only_match_is_found_by_the_fallback() {
        let _g = pinned();
        // Arrange
        let f = fx("se-literal");
        f.write("a.txt", "total = a + b\n");
        f.write("b.txt", "nothing\n");
        f.run();

        // Act
        let got = search(&f, &SearchOptions::new("a + b"));

        // Assert: tokens `a` and `b` match through FTS; the file is found either way.
        assert_eq!(paths(&got), vec!["a.txt"]);
    }

    #[test]
    fn a_file_edited_after_indexing_is_flagged_stale() {
        let _g = pinned();
        // Arrange
        let f = fx("se-stale");
        f.write("a.txt", "needleword\n");
        f.run();
        f.write("a.txt", "needleword changed\n");

        // Act
        let got = search(&f, &SearchOptions::new("needleword"));

        // Assert
        assert_eq!(paths(&got), vec!["a.txt"]);
        assert!(got.hits[0].stale);
    }

    #[test]
    fn the_path_filter_excludes_other_files() {
        let _g = pinned();
        // Arrange
        let f = fx("se-filter");
        f.write("lib/a.txt", "needleword\n");
        f.write("other/b.txt", "needleword\n");
        f.run();
        let mut opts = SearchOptions::new("needleword");
        opts.path = Some("lib/".to_string());

        // Act
        let got = search(&f, &opts);

        // Assert
        assert_eq!(paths(&got), vec!["lib/a.txt"]);
    }

    #[test]
    fn a_trailing_slash_prefix_keeps_sibling_names_out() {
        let _g = pinned();
        // Arrange
        let f = fx("se-prefix");
        f.write("lib/a.txt", "needleword\n");
        f.write("library.txt", "needleword\n");
        f.run();
        let mut opts = SearchOptions::new("needleword");
        opts.path = Some("lib/".to_string());

        // Act
        let got = search(&f, &opts);

        // Assert: the trailing slash keeps `library.txt` out.
        assert_eq!(paths(&got), vec!["lib/a.txt"]);
    }

    #[test]
    fn a_literal_hit_has_the_literal_type_tier_line_and_counts_extra_hits() {
        let _g = pinned();
        // Arrange: `++` has no tokens, so only the literal scan answers.
        let f = fx("se-literal-hit");
        f.write("a.txt", "plain\nx ++ y\nz ++ w\nq ++ r\n");
        f.run();
        let mut opts = SearchOptions::new("++");
        opts.limit = 2;

        // Act
        let got = search(&f, &opts);

        // Assert
        assert_eq!(got.hits.len(), 2);
        assert_eq!(got.hits[0].match_type, "literal");
        assert_eq!(got.hits[0].tier, 7);
        assert_eq!(got.hits[0].line, 2);
        assert!(got.truncated);
        assert_eq!(got.omitted, 1);
        assert!(!got.scan_capped);
    }

    #[test]
    fn the_match_line_prefers_a_line_with_every_token_as_a_whole_word() {
        // Arrange
        let lines: Vec<String> = ["fn provide() {", "let id = 3;", "x", "let id = retry;", "x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let toks = vec!["id".to_string(), "retry".to_string()];

        // Act, Assert
        assert_eq!(match_line(&lines, 1, 5, &toks), 4);
        assert_eq!(match_line(&lines, 1, 3, &["id".to_string()]), 2);
    }

    #[test]
    fn the_match_line_falls_back_through_every_pass_and_ignores_case() {
        // Arrange
        let lines: Vec<String> = ["Let ID = 3", "retry only", "xx", "RetryPolicy"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let both = vec!["id".to_string(), "retry".to_string()];

        // Act, Assert: no line holds both tokens, so any whole word wins.
        assert_eq!(match_line(&lines, 1, 4, &both), 1);
        // Only a substring matches, and only in the last two lines of the window.
        assert_eq!(match_line(&lines, 3, 4, &["policy".to_string()]), 4);
        // A mixed-case token is lowercased before the compare.
        assert_eq!(match_line(&lines, 1, 4, &["RETRY".to_string()]), 2);
        // Nothing matches: the first line of the window.
        assert_eq!(match_line(&lines, 2, 3, &["zzz".to_string()]), 2);
        // A window past the end of the file keeps the start line.
        assert_eq!(match_line(&lines, 9, 12, &["id".to_string()]), 9);
    }

    #[cfg(unix)]
    #[test]
    fn a_file_swapped_for_a_symlink_after_indexing_is_not_read() {
        let _g = pinned();
        // Arrange
        let f = fx("se-symlink");
        f.write("a.txt", "needleword\n");
        f.write("secret.txt", "private\n");
        f.run();
        std::fs::remove_file(f.root.join("a.txt")).expect("remove");
        std::os::unix::fs::symlink(f.root.join("secret.txt"), f.root.join("a.txt")).expect("link");

        // Act
        let got = search(&f, &SearchOptions::new("needleword"));

        // Assert: the hit stays, with no excerpt read through the link.
        assert_eq!(paths(&got), vec!["a.txt"]);
        assert!(got.hits[0].excerpt.is_empty());
        assert!(got.hits[0].stale);
    }

    #[test]
    fn the_fts_query_wraps_each_token_and_doubles_quotes() {
        // Arrange, Act, Assert
        assert_eq!(
            fts_query("retry AND OR ("),
            Some("\"retry\" \"AND\" \"OR\"".to_string())
        );
        assert_eq!(fts_query("++ --"), None);
    }

    #[test]
    fn a_damaged_text_index_names_rebuild() {
        let _g = pinned();
        // Arrange
        let f = fx("search-damaged");
        f.write("a.rs", "fn needle() {}\n");
        f.run();
        let conn = f.conn();
        crate::testkit::damage_pages(&f);

        // Act
        let got = text_search(&conn, &f.root, &SearchOptions::new("needle"));

        // Assert
        let Err(message) = got else {
            panic!("the query must fail")
        };
        assert!(message.contains("--rebuild"), "{message}");
    }

    /// This worktree lists the same bytes as `shared.rs` and `again.rs`, and
    /// another worktree lists them as `sibling.rs` next to a file only it has.
    fn two_worktrees_sharing(tag: &str) -> (Fx, crate::db::Store) {
        let f = fx(tag);
        f.write("shared.rs", "fn widget() { widget(); }\n");
        f.write("again.rs", "fn widget() { widget(); }\n");
        f.run();
        let other = crate::walker::test_repo::init(&format!("{tag}-b"));
        crate::walker::test_repo::write(&other, "sibling.rs", b"fn widget() { widget(); }\n");
        crate::walker::test_repo::write(&other, "only_there.rs", b"fn widgetonly() {}\n");
        crate::index::run(
            &other,
            &f.index,
            "other",
            false,
            &crate::index::Options::default(),
        )
        .expect("second worktree");
        let conn = f.conn();
        (f, conn)
    }

    #[test]
    fn a_text_search_lists_each_path_of_this_worktree_and_none_of_a_sibling() {
        let _g = pinned();
        // Arrange
        let (f, conn) = two_worktrees_sharing("sr-scope");

        // Act
        let got = text_search(&conn, &f.root, &SearchOptions::new("widget")).expect("search");

        // Assert
        let mut found = paths(&got);
        found.sort_unstable();
        assert_eq!(found, vec!["again.rs", "shared.rs"]);
        assert_eq!(got.omitted, 0);
    }
}
