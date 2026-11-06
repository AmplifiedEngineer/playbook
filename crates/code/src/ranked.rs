// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Ranked lookup over symbols, imports and text. Tiers: 1 exact qualified
//! name, 2 exact name, 3 case-insensitive exact name, 4 prefix, 5 import
//! match, 6 text, 7 literal substring. Ties sort by path, then start line.

use super::search::{
    filters, fts_hits, literal_scan, load, Hit, SearchOptions, SearchResult, SymbolInfo,
};
use playbook_store::{params_from_iter, Connection};
use std::collections::HashMap;
use std::path::Path;

fn sql_err(e: playbook_store::Error) -> String {
    super::db::map_sql_error("search failed", &e)
}

fn to_usize(n: i64) -> usize {
    usize::try_from(n).unwrap_or(0)
}

/// The ids of the symbols a query can match: the exact qualified name, or a
/// name that starts with the query. The prefix part is a range on the
/// `lower(name)` index and the LIKE only filters inside that range, because a
/// LIKE alone scans the whole index. `?1` is the query, `?2` its lowercase
/// form and `?3` the escaped LIKE pattern.
const PREFILTER: &str = "SELECT id FROM main.symbols WHERE qualified_name = ?1
     UNION SELECT id FROM main.symbols
           WHERE lower(name) >= ?2 AND lower(name) < ?2 || char(1114111)
             AND lower(name) LIKE ?3 ESCAPE '\\'";

/// Whether `path` on disk still hashes to `sha`. Cached per path.
fn is_current(cache: &mut HashMap<String, bool>, root: &Path, path: &str, sha: &str) -> bool {
    *cache
        .entry(path.to_string())
        .or_insert_with(|| load(root, path).is_some_and(|l| l.same(sha)))
}

/// Symbols matching `opts.query` by tiers 1 to 4, plus how many matched in all.
pub fn symbol_hits(
    conn: &Connection,
    root: &Path,
    opts: &SearchOptions,
    fetch: usize,
) -> Result<(Vec<Hit>, usize), String> {
    let query = opts.query.trim();
    if query.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let (extra, extra_params) = filters(opts);
    let kind = if opts.kind.is_some() {
        " AND s.kind = ?"
    } else {
        ""
    };
    let base = format!(
        "FROM (
            SELECT f.path AS path, f.lang AS lang, f.sha256 AS sha, s.id AS sid, s.name AS name,
                   s.qualified_name AS qname, s.kind AS kind, s.signature AS signature,
                   s.exported AS exported, s.start_line AS start_line, s.end_line AS end_line,
                   CASE WHEN s.qualified_name = ?1 THEN 1
                        WHEN s.name = ?1 THEN 2
                        WHEN lower(s.name) = lower(?1) THEN 3
                        WHEN substr(lower(s.name), 1, length(?1)) = lower(?1) THEN 4 END AS tier
            FROM ({PREFILTER}) h
            CROSS JOIN main.symbols s ON s.id = h.id
            CROSS JOIN files f ON f.id = s.content
            WHERE 1 = 1{extra}{kind}
         ) WHERE tier IS NOT NULL"
    );
    // Tiers 2 to 4 all name a prefix of the symbol's name. The prefilter reads
    // two indexes and scopes only its rows, where the scoped `symbols` view
    // would probe the file list for every symbol of the store.
    let lowered = query.to_ascii_lowercase();
    let like = format!(
        "{}%",
        lowered
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    let mut params: Vec<String> = vec![query.to_string(), lowered, like];
    params.extend(extra_params);
    params.extend(opts.kind.clone());

    let total: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) {base}"),
            params_from_iter(params.iter()),
            |r| r.get(0),
        )
        .map_err(sql_err)?;
    let sql = format!(
        "SELECT path, lang, sha, name, qname, kind, signature, exported, start_line, end_line, tier
         {base} ORDER BY tier, path, start_line, sid LIMIT {fetch}"
    );
    let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
    let rows = stmt
        .query_map(params_from_iter(params.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                SymbolInfo {
                    name: r.get(3)?,
                    qualified_name: r.get(4)?,
                    kind: r.get(5)?,
                    lang: r.get(1)?,
                    signature: r.get(6)?,
                    exported: r.get::<_, i64>(7)? != 0,
                },
                to_usize(r.get(8)?),
                to_usize(r.get(9)?),
                u8::try_from(r.get::<_, i64>(10)?).unwrap_or(4),
            ))
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    let mut cache = HashMap::new();
    let hits = rows
        .into_iter()
        .map(|(path, _lang, sha, info, start, end, tier)| Hit {
            stale: !is_current(&mut cache, root, &path, &sha),
            excerpt: info.signature.clone().unwrap_or_default(),
            excerpt_start: start,
            path,
            start_line: start,
            end_line: end,
            line: start,
            match_type: "symbol",
            tier,
            symbol: Some(info),
        })
        .collect();
    Ok((hits, to_usize(total)))
}

/// Imports of the query as a module, or of the query as an imported name.
fn import_hits(
    conn: &Connection,
    root: &Path,
    opts: &SearchOptions,
    fetch: usize,
) -> Result<(Vec<Hit>, usize), String> {
    let query = opts.query.trim();
    if query.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let (extra, extra_params) = filters(opts);
    let base = format!(
        "FROM imports i JOIN files f ON f.id = i.file_id
         WHERE (i.module = ?1
                OR instr(', ' || COALESCE(i.names, '') || ', ', ', ' || ?1 || ', ') > 0
                OR instr(', ' || COALESCE(i.names, '') || ', ', ' as ' || ?1 || ', ') > 0
                OR instr(', ' || COALESCE(i.names, '') || ', ', ', ' || ?1 || ' as ') > 0){extra}"
    );
    let mut params: Vec<String> = vec![query.to_string()];
    params.extend(extra_params);
    let total: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) {base}"),
            params_from_iter(params.iter()),
            |r| r.get(0),
        )
        .map_err(sql_err)?;
    let sql = format!(
        "SELECT f.path, f.sha256, i.module, i.names, i.line {base}
         ORDER BY f.path, i.line, i.rowid LIMIT {fetch}"
    );
    let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
    let rows = stmt
        .query_map(params_from_iter(params.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                to_usize(r.get(4)?),
            ))
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    let mut cache = HashMap::new();
    let hits = rows
        .into_iter()
        .map(|(path, sha, module, names, line)| Hit {
            stale: !is_current(&mut cache, root, &path, &sha),
            excerpt: match names {
                Some(n) => format!("{module}: {n}"),
                None => module,
            },
            excerpt_start: line,
            path,
            start_line: line,
            end_line: line,
            line,
            match_type: "import",
            tier: 5,
            symbol: None,
        })
        .collect();
    Ok((hits, to_usize(total)))
}

/// Ranked search over every tier, at most `opts.limit` results.
pub fn ranked_search(
    conn: &Connection,
    root: &Path,
    opts: &SearchOptions,
) -> Result<SearchResult, String> {
    let limit = opts.limit;
    let mut result = SearchResult::default();
    // A tier that finds nothing to show because the limit is full may still
    // hold matches, so the result is flagged truncated.
    let mut skipped_tier = false;

    let (hits, total) = symbol_hits(conn, root, opts, limit)?;
    result.omitted += total.saturating_sub(hits.len());
    result.hits.extend(hits);

    let remaining = limit - result.hits.len();
    if remaining > 0 {
        let (hits, total) = import_hits(conn, root, opts, remaining)?;
        result.omitted += total.saturating_sub(hits.len());
        result.hits.extend(hits);
    } else {
        skipped_tier = true;
    }

    let remaining = limit - result.hits.len();
    if remaining > 0 {
        let (hits, total) = fts_hits(conn, root, opts, remaining)?;
        result.omitted += total.saturating_sub(hits.len());
        let shown: Vec<Hit> = hits
            .into_iter()
            .filter(|h| !result.hits.iter().any(|seen| covers(seen, h)))
            .collect();
        result.hits.extend(shown);
    } else {
        skipped_tier = true;
    }

    let remaining = limit - result.hits.len();
    if remaining > 0 {
        let literal = literal_scan(conn, root, opts, remaining, &result.hits)?;
        result.omitted += literal.omitted;
        result.hits.extend(literal.hits);
        result.files_scanned = literal.files_scanned;
        result.scan_capped = literal.scan_capped;
    } else {
        skipped_tier = true;
    }
    result.truncated = result.omitted > 0 || result.scan_capped || skipped_tier;
    Ok(result)
}

/// Whether `seen` already shows the place `hit` points at.
fn covers(seen: &Hit, hit: &Hit) -> bool {
    seen.path == hit.path
        && if seen.match_type == "text" {
            (seen.start_line..=seen.end_line).contains(&hit.line)
        } else {
            hit.start_line <= seen.line && seen.line <= hit.end_line
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{fx, pinned, Fx};

    fn rank(f: &Fx, opts: &SearchOptions) -> SearchResult {
        ranked_search(&f.conn(), &f.root, opts).expect("search")
    }

    fn tiers(r: &SearchResult) -> Vec<(u8, &str)> {
        r.hits.iter().map(|h| (h.tier, h.path.as_str())).collect()
    }

    #[test]
    fn a_qualified_match_ranks_before_a_name_only_match() {
        let _g = pinned();
        // Arrange
        let f = fx("rk-qualified");
        f.write(
            "svc.ts",
            "export class PaymentService {\n  processPayment() {}\n}\n",
        );
        f.write("util.ts", "export function processPayment() {}\n");
        f.run();

        // Act
        let both = rank(&f, &SearchOptions::new("PaymentService.processPayment"));
        let plain = rank(&f, &SearchOptions::new("processPayment"));

        // Assert
        assert_eq!(tiers(&both)[0], (1, "svc.ts"));
        // A top level function's qualified name is its name, so it ranks first.
        assert_eq!(tiers(&plain)[..2], [(1, "util.ts"), (2, "svc.ts")]);
    }

    #[test]
    fn the_kind_filter_drops_other_kinds() {
        let _g = pinned();
        // Arrange
        let f = fx("rk-kind");
        f.write("a.ts", "class Thing {}\n");
        f.write("b.ts", "function Thing() {}\n");
        f.run();
        let mut opts = SearchOptions::new("Thing");
        opts.kind = Some("function".to_string());

        // Act
        let got = symbol_hits(&f.conn(), &f.root, &opts, 10).expect("hits").0;

        // Assert
        let kinds: Vec<&str> = got
            .iter()
            .filter_map(|h| h.symbol.as_ref().map(|s| s.kind.as_str()))
            .collect();
        assert_eq!(kinds, vec!["function"]);
    }

    #[test]
    fn case_only_and_prefix_matches_rank_as_tiers_three_and_four() {
        let _g = pinned();
        // Arrange
        let f = fx("rk-case-prefix");
        f.write("a.rs", "fn Handler() {}\n");
        f.write("b.rs", "fn handler_extra() {}\n");
        f.run();

        // Act
        let got = symbol_hits(&f.conn(), &f.root, &SearchOptions::new("handler"), 10)
            .expect("hits")
            .0;

        // Assert
        assert_eq!(tiers_of(&got), vec![(3, "a.rs"), (4, "b.rs")]);
    }

    fn tiers_of(hits: &[Hit]) -> Vec<(u8, &str)> {
        hits.iter().map(|h| (h.tier, h.path.as_str())).collect()
    }

    #[test]
    fn import_text_and_literal_hits_come_back_in_tier_order() {
        let _g = pinned();
        // Arrange: `retry` is an imported name in a.py, a word in b.txt, and a
        // substring of `retrying` in c.txt.
        let f = fx("rk-tiers");
        f.write("a.py", "from net import retry\n");
        f.write("b.txt", "we retry here\n");
        f.write("c.txt", "retrying forever\n");
        f.run();

        // Act
        let got = rank(&f, &SearchOptions::new("retry"));

        // Assert
        let order: Vec<(u8, &str)> = tiers(&got);
        assert!(order.contains(&(5, "a.py")), "{order:?}");
        assert!(order.contains(&(6, "b.txt")), "{order:?}");
        assert!(order.contains(&(7, "c.txt")), "{order:?}");
        let only: Vec<u8> = got.hits.iter().map(|h| h.tier).collect();
        let mut sorted = only.clone();
        sorted.sort_unstable();
        assert_eq!(only, sorted);
    }

    #[test]
    fn ties_sort_by_path_then_line() {
        let _g = pinned();
        // Arrange
        let f = fx("rk-ties");
        f.write("b/x.ts", "\n\nfunction dup() {}\n");
        f.write("a/y.ts", "\n\n\n\n\n\n\n\nfunction dup() {}\n");
        f.write("a/z.ts", "function dup() {}\nfunction dup() {}\n");
        f.run();

        // Act
        let got = symbol_hits(&f.conn(), &f.root, &SearchOptions::new("dup"), 10)
            .expect("hits")
            .0;

        // Assert
        let places: Vec<(&str, usize)> = got.iter().map(|h| (h.path.as_str(), h.line)).collect();
        assert_eq!(
            places,
            vec![("a/y.ts", 9), ("a/z.ts", 1), ("a/z.ts", 2), ("b/x.ts", 3)]
        );
    }

    #[test]
    fn lang_and_path_filters_keep_only_matching_rows() {
        let _g = pinned();
        // Arrange
        let f = fx("rk-filters");
        f.write("lib/a.py", "def target():\n    pass\n");
        f.write("lib/b.ts", "function target() {}\n");
        f.write("other/c.py", "def target():\n    pass\n");
        f.run();
        let mut opts = SearchOptions::new("target");
        opts.lang = Some("python".to_string());
        opts.path = Some("lib/".to_string());

        // Act
        let got = symbol_hits(&f.conn(), &f.root, &opts, 10).expect("hits").0;

        // Assert
        assert_eq!(tiers_of(&got), vec![(1, "lib/a.py")]);
    }

    #[test]
    fn a_limit_truncates_and_counts_the_rest() {
        let _g = pinned();
        // Arrange
        let f = fx("rk-limit");
        for i in 0..3 {
            f.write(&format!("f{i}.rs"), "fn same() {}\n");
        }
        f.run();
        let mut opts = SearchOptions::new("same");
        opts.limit = 2;

        // Act
        let got = rank(&f, &opts);

        // Assert
        assert_eq!(got.hits.len(), 2);
        assert!(got.truncated);
        assert_eq!(got.omitted, 1);
    }

    #[test]
    fn a_full_limit_still_flags_the_skipped_tiers() {
        let _g = pinned();
        // Arrange: one symbol fills the limit, and b.py matches only as a substring.
        let f = fx("rk-skipped");
        f.write("a.py", "def retry():\n    pass\n");
        f.write("b.py", "x = retry()\n");
        f.run();
        let mut opts = SearchOptions::new("retr");
        opts.limit = 1;

        // Act
        let got = rank(&f, &opts);

        // Assert
        assert_eq!(got.hits.len(), 1);
        assert!(got.truncated);
    }

    #[test]
    fn a_symbol_line_is_not_repeated_as_a_text_or_literal_hit() {
        let _g = pinned();
        // Arrange
        let f = fx("rk-dedup");
        f.write("a.rs", "fn same() {}\n");
        f.run();

        // Act
        let got = rank(&f, &SearchOptions::new("same"));

        // Assert
        assert_eq!(tiers(&got), vec![(1, "a.rs")]);
    }

    #[test]
    fn import_hits_match_modules_and_aliased_names_and_report_staleness() {
        let _g = pinned();
        // Arrange
        let f = fx("rk-imports");
        f.write("a.py", "import numpy as np\nfrom net import retry as r\n");
        f.run();
        f.write(
            "a.py",
            "import numpy as np\nfrom net import retry as r\n# edited\n",
        );

        // Act
        let found = |q: &str| {
            import_hits(&f.conn(), &f.root, &SearchOptions::new(q), 10)
                .expect("hits")
                .0
                .len()
        };

        // Assert
        for q in ["numpy", "np", "net", "retry", "r"] {
            assert_eq!(found(q), 1, "{q}");
        }
        let hit = import_hits(&f.conn(), &f.root, &SearchOptions::new("net"), 10)
            .expect("hits")
            .0;
        assert!(hit[0].stale);
    }

    #[test]
    fn a_symbol_hit_is_stale_after_the_file_changes() {
        let _g = pinned();
        // Arrange
        let f = fx("rk-stale");
        f.write("a.rs", "fn a() {}\n");
        f.run();
        f.write("a.rs", "fn a() {}\n// edited\n");

        // Act
        let got = symbol_hits(&f.conn(), &f.root, &SearchOptions::new("a"), 10)
            .expect("hits")
            .0;

        // Assert
        assert!(got[0].stale);
    }

    #[test]
    fn repeated_runs_give_identical_results() {
        let _g = pinned();
        // Arrange
        let f = fx("rk-stable");
        f.write("a.rs", "fn stable() {}\n");
        f.write("b.rs", "fn stable_two() {}\n");
        f.run();

        // Act
        let first = tiers(&rank(&f, &SearchOptions::new("stable"))).len();
        let a = format!("{:?}", rank(&f, &SearchOptions::new("stable")).hits);
        let b = format!("{:?}", rank(&f, &SearchOptions::new("stable")).hits);

        // Assert
        assert!(first >= 1);
        assert_eq!(a, b);
    }

    #[test]
    fn a_prefix_query_with_a_wildcard_character_matches_only_its_own_prefix() {
        let _g = pinned();
        // Arrange
        let f = fx("ranked-wildcards");
        f.write(
            "a.rs",
            "fn run_lock_path() {}\nfn runXlock() {}\nfn Run_Lock() {}\nfn other() {}\n",
        );
        f.run();

        // Act
        let (hits, total) =
            symbol_hits(&f.conn(), &f.root, &SearchOptions::new("run_lock"), 10).expect("hits");

        // Assert
        let mut names: Vec<&str> = hits
            .iter()
            .filter_map(|h| h.symbol.as_ref().map(|s| s.name.as_str()))
            .collect();
        names.sort_unstable();
        assert_eq!(names, vec!["Run_Lock", "run_lock_path"]);
        assert_eq!(total, 2);
    }

    #[test]
    fn a_damaged_symbol_table_names_rebuild() {
        let _g = pinned();
        // Arrange
        let f = fx("ranked-damaged");
        f.write("a.rs", "fn needle() {}\n");
        f.run();
        let conn = f.conn();
        crate::testkit::damage_pages(&f);

        // Act
        let got = ranked_search(&conn, &f.root, &SearchOptions::new("needle"));

        // Assert
        let Err(message) = got else {
            panic!("the query must fail")
        };
        assert!(message.contains("--rebuild"), "{message}");
    }

    #[test]
    fn the_prefix_prefilter_searches_the_name_index_instead_of_scanning_it() {
        let _g = pinned();
        // Arrange
        let f = fx("rk-plan");
        f.write("a.rs", "fn widget() {}\n");
        f.run();
        let conn = f.conn();

        // Act
        let mut stmt = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {PREFILTER}"))
            .expect("prepare");
        let plan: Vec<String> = stmt
            .query_map(["widget", "widget", "widget%"], |r| r.get::<_, String>(3))
            .expect("plan")
            .map(|r| r.expect("row"))
            .collect();

        // Assert
        assert!(
            plan.iter().any(|p| p.contains("symbols_name_lower")),
            "{plan:?}"
        );
        assert!(
            plan.iter().all(|p| !p.starts_with("SCAN main.symbols")),
            "{plan:?}"
        );
    }

    /// The fixture of the scoping tests: `shared.rs` and `again.rs` hold the
    /// same bytes in this worktree, and another worktree lists the same bytes
    /// as `sibling.rs` next to a symbol only it has.
    fn two_worktrees_sharing(tag: &str) -> (Fx, crate::db::Store) {
        let f = fx(tag);
        f.write("shared.rs", "fn widget() {}\n");
        f.write("again.rs", "fn widget() {}\n");
        f.run();
        let other = crate::walker::test_repo::init(&format!("{tag}-b"));
        crate::walker::test_repo::write(&other, "sibling.rs", b"fn widget() {}\n");
        crate::walker::test_repo::write(&other, "only_there.rs", b"fn widgetry() {}\n");
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
    fn a_symbol_search_lists_each_path_of_this_worktree_once_and_none_of_a_sibling() {
        let _g = pinned();
        // Arrange
        let (f, conn) = two_worktrees_sharing("rk-scope");
        let opts = SearchOptions::new("widget");

        // Act
        let got = ranked_search(&conn, &f.root, &opts).expect("search");

        // Assert
        let mut paths: Vec<&str> = got.hits.iter().map(|h| h.path.as_str()).collect();
        paths.sort_unstable();
        assert_eq!(paths, vec!["again.rs", "shared.rs"]);
        assert_eq!(got.omitted, 0);
    }
}
