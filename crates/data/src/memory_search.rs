// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Ranked search over the fact index in `memory.db`. It returns candidates for
//! the recall scorer, not final matches: the scorer decides strength, caps and
//! what to show.

use crate::memory::{self};

/// A recall hook runs on every prompt, so it waits only briefly for a busy
/// database and then falls back to the scan.
const READ_BUSY: std::time::Duration = std::time::Duration::from_millis(200);
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

/// Candidates a caller gets by default: four times the 3 facts one prompt may
/// recall, so the scorer can reorder and reject without a second query.
pub const DEFAULT_LIMIT: usize = 12;

/// From this many characters a token also matches words that extend it or that
/// it extends, as the recall matcher does.
const MIN_PREFIX_CHARS: usize = 4;

/// Most terms one query may carry, so a long prompt cannot build a huge
/// expression.
const MAX_TERMS: usize = 48;

/// `bm25` column weights for name, description and body.
const WEIGHTS: &str = "10.0, 5.0, 1.0";

/// One candidate fact with the node fields a caller needs to show it. A lower
/// `rank` is a better match (SQLite `bm25`).
#[derive(Clone, Debug, PartialEq)]
pub struct FactHit {
    pub fact_id: String,
    pub name: String,
    pub description: String,
    pub file: String,
    pub rank: f64,
}

/// Why the index cannot answer, so the caller can fall back to another way of
/// finding facts. An empty `Ok` result is a real answer, not one of these.
#[derive(Debug, PartialEq, Eq)]
pub enum SearchError {
    NoDatabase,
    IndexMissing,
    IndexStale,
    Busy,
    Failed(String),
}

/// The fact ids in scope for `repo` that match any of `tokens`, best first,
/// at most `limit`. Ties keep fact id order. `tokens` are lowercased prompt
/// words as the recall matcher produces them. A prompt with no usable token
/// returns nothing without touching the database.
pub fn search_facts(
    db: &Path,
    repo: &str,
    tokens: &[String],
    limit: usize,
) -> Result<Vec<FactHit>, SearchError> {
    let Some(expression) = match_expression(tokens) else {
        return Ok(Vec::new());
    };
    if !db.is_file() {
        return Err(SearchError::NoDatabase);
    }
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(failed)?;
    conn.busy_timeout(READ_BUSY).map_err(failed)?;
    // One read snapshot for the freshness check and the query.
    let tx = conn.unchecked_transaction().map_err(failed)?;
    if !memory::facts_index_exists(&tx).map_err(failed)? {
        return Err(SearchError::IndexMissing);
    }
    if !memory::facts_index_is_fresh(&tx) {
        return Err(SearchError::IndexStale);
    }
    let org = repo.split('/').next().unwrap_or(repo);
    let mut stmt = tx
        .prepare(&format!(
            "SELECT m.fact_id, COALESCE(n.name, ''), COALESCE(n.description, ''), \
                    COALESCE(n.file, ''), bm25(facts_fts, {WEIGHTS}) AS rank \
             FROM facts_fts JOIN facts_fts_map m ON m.rowid = facts_fts.rowid \
             LEFT JOIN nodes n ON n.id = m.fact_id \
             WHERE facts_fts MATCH ?1 \
               AND (m.scope = 'global' \
                    OR (m.scope = 'org' AND m.project = ?2) \
                    OR (m.scope = 'project' AND m.project = ?3)) \
             ORDER BY rank, m.fact_id LIMIT ?4"
        ))
        .map_err(failed)?;
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let rows = stmt
        .query_map(rusqlite::params![expression, org, repo, limit], |r| {
            Ok(FactHit {
                fact_id: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
                file: r.get(3)?,
                rank: r.get(4)?,
            })
        })
        .map_err(failed)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(failed)
}

fn failed(error: rusqlite::Error) -> SearchError {
    match error.sqlite_error_code() {
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
            SearchError::Busy
        }
        _ => SearchError::Failed(error.to_string()),
    }
}

/// The FTS5 expression for `tokens`, or `None` when no token can match. Every
/// term is a quoted string, so no prompt text is read as FTS5 syntax.
fn match_expression(tokens: &[String]) -> Option<String> {
    let mut terms: Vec<String> = Vec::new();
    for token in tokens {
        for term in token_terms(&token.to_lowercase()) {
            if terms.len() == MAX_TERMS {
                break;
            }
            terms.push(term);
        }
    }
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

/// A hyphenated token is a phrase, which is how the matcher treats it (a
/// substring of the name). Another token must be plain letters and digits to
/// match a word; from `MIN_PREFIX_CHARS` it also matches longer words
/// (`testing*`) and each shorter prefix as a whole word (`test` for
/// `testing`), the two directions of the matcher's prefix rule.
fn token_terms(token: &str) -> Vec<String> {
    if token.contains('-') {
        return if token.chars().any(char::is_alphanumeric) {
            vec![quote(token)]
        } else {
            Vec::new()
        };
    }
    let length = token.chars().count();
    if length == 0 || !token.chars().all(char::is_alphanumeric) {
        return Vec::new();
    }
    if length < MIN_PREFIX_CHARS {
        return vec![quote(token)];
    }
    let mut terms = vec![format!("{}*", quote(token))];
    for keep in (MIN_PREFIX_CHARS..length).rev() {
        let prefix: String = token.chars().take(keep).collect();
        terms.push(quote(&prefix));
    }
    terms
}

fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{db_in, write_graph, write_graph_with_bodies, NodeRow};
    use playbook_common::test_support::scratch_dir;
    use std::path::PathBuf;

    const REPO: &str = "acme/widgets";

    fn fact(id: &str, scope: &str, project: Option<&str>, name: &str, desc: &str) -> NodeRow {
        NodeRow {
            id: id.into(),
            file: format!("{id}.md"),
            scope: scope.into(),
            project: project.map(str::to_string),
            kind: "project".into(),
            name: Some(name.into()),
            description: Some(desc.into()),
            pinned: false,
        }
    }

    fn global(id: &str, name: &str, desc: &str) -> NodeRow {
        fact(id, "global", None, name, desc)
    }

    fn store(tag: &str, nodes: &[NodeRow]) -> PathBuf {
        let db = db_in(&scratch_dir(tag));
        write_graph(&db, nodes, &[]).unwrap();
        db
    }

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    fn ids(db: &Path, tokens: &str) -> Vec<String> {
        search_facts(db, REPO, &words(tokens), DEFAULT_LIMIT)
            .unwrap()
            .into_iter()
            .map(|h| h.fact_id)
            .collect()
    }

    #[test]
    fn a_word_inside_a_hyphenated_or_underscored_name_is_found() {
        // Arrange
        let db = store(
            "ms-parity",
            &[
                global("hyphen", "pre-run-hook", "d"),
                global("under", "pre_run_hook", "d"),
            ],
        );

        // Act
        let found = ids(&db, "run");

        // Assert
        assert_eq!(found, ["hyphen", "under"]);
    }

    #[test]
    fn a_token_matches_longer_words_and_shorter_words_from_four_characters() {
        // Arrange
        let db = store(
            "ms-prefix",
            &[
                global("short", "test", "d"),
                global("long", "testing", "d"),
                global("other", "unrelated", "d"),
            ],
        );

        // Act, Assert
        assert_eq!(ids(&db, "testing"), ["long", "short"]);
        assert_eq!(ids(&db, "test"), ["long", "short"]);
    }

    #[test]
    fn a_three_character_token_matches_only_the_exact_word() {
        // Arrange
        let db = store(
            "ms-three",
            &[
                global("exact", "pbx", "d"),
                global("longer", "pbxcode", "d"),
            ],
        );

        // Act
        let found = ids(&db, "pbx");

        // Assert
        assert_eq!(found, ["exact"]);
    }

    #[test]
    fn a_name_match_ranks_above_a_description_match() {
        // Arrange
        let db = store(
            "ms-rank",
            &[
                global("desc-only", "zzz", "handles ferry delays"),
                global("name-hit", "ferry-delays", "zzz"),
            ],
        );

        // Act
        let found = ids(&db, "ferry");

        // Assert
        assert_eq!(found, ["name-hit", "desc-only"]);
    }

    #[test]
    fn equal_scores_are_ordered_by_fact_id() {
        // Arrange
        let db = store(
            "ms-ties",
            &[
                global("m", "same text", "d"),
                global("z", "same text", "d"),
                global("a", "same text", "d"),
            ],
        );

        // Act
        let found = ids(&db, "same");

        // Assert
        assert_eq!(found, ["a", "m", "z"]);
    }

    #[test]
    fn only_facts_in_scope_for_the_repo_are_returned() {
        // Arrange
        let db = store(
            "ms-scope",
            &[
                global("g", "shared topic", "d"),
                fact("org", "org", Some("acme"), "shared topic", "d"),
                fact("mine", "project", Some(REPO), "shared topic", "d"),
                fact("other-org", "org", Some("other"), "shared topic", "d"),
                fact("theirs", "project", Some("acme/other"), "shared topic", "d"),
            ],
        );

        // Act
        let found = ids(&db, "shared");

        // Assert
        assert_eq!(found, ["g", "mine", "org"]);
    }

    #[test]
    fn a_store_with_no_facts_returns_nothing_without_error() {
        // Arrange
        let db = store("ms-empty", &[]);

        // Act, Assert
        assert_eq!(ids(&db, "anything"), Vec::<String>::new());
    }

    #[test]
    fn a_prompt_with_no_usable_token_issues_no_query() {
        // Arrange: the database does not exist, so any query would fail.
        let missing = scratch_dir("ms-none").join("memory.db");

        // Act
        let result = search_facts(&missing, REPO, &words("... -- ' @@"), DEFAULT_LIMIT);

        // Assert
        assert_eq!(result, Ok(Vec::new()));
    }

    #[test]
    fn syntax_characters_and_keywords_in_a_prompt_are_plain_text() {
        // Arrange
        let db = store("ms-hostile", &[global("a", "or near", "d")]);
        let hostile = [
            "\"", "*", "OR", "NEAR", "(", ")", "AND", "NOT", "a\"b-c", "x*y", "col:val", "^start",
        ];

        // Act, Assert
        for token in hostile {
            let result = search_facts(&db, REPO, &[token.to_string()], DEFAULT_LIMIT);
            assert!(result.is_ok(), "{token:?} gave {result:?}");
        }
    }

    #[test]
    fn an_accent_is_kept_as_the_matcher_keeps_it() {
        // Arrange
        let db = store("ms-accent", &[global("cafe", "caf\u{e9} menu", "d")]);

        // Act, Assert
        assert_eq!(ids(&db, "caf\u{e9}"), ["cafe"]);
        assert!(ids(&db, "cafe").is_empty());
    }

    #[test]
    fn a_hyphenated_token_matches_a_name_that_holds_it_and_not_a_fragment_of_one() {
        // Arrange
        let db = store("ms-phrase", &[global("hook", "pre-run-hook", "d")]);

        // Act, Assert
        assert_eq!(ids(&db, "pre-run-hook"), ["hook"]);
        assert_eq!(ids(&db, "run-hook"), ["hook"]);
        assert!(ids(&db, "re-run").is_empty());
    }

    #[test]
    fn the_limit_caps_the_rows_returned() {
        // Arrange
        let nodes: Vec<NodeRow> = (0..30)
            .map(|n| global(&format!("f{n:02}"), "common word", "d"))
            .collect();
        let db = store("ms-limit", &nodes);

        // Act
        let hits = search_facts(&db, REPO, &words("common"), DEFAULT_LIMIT).unwrap();

        // Assert
        assert_eq!(hits.len(), DEFAULT_LIMIT);
    }

    #[test]
    fn many_long_tokens_stay_under_the_term_bound() {
        // Arrange
        let tokens: Vec<String> = (0..40).map(|n| format!("extraordinarily{n}")).collect();

        // Act
        let expression = match_expression(&tokens).unwrap();

        // Assert
        assert!(expression.matches(" OR ").count() < MAX_TERMS);
    }

    #[test]
    fn a_missing_database_a_missing_table_and_a_stale_index_each_say_so() {
        // Arrange
        let dir = scratch_dir("ms-trust");
        let missing = db_in(&dir.join("none"));
        let stale = store("ms-stale", &[global("a", "alpha", "d")]);
        Connection::open(&stale)
            .unwrap()
            .execute(
                "UPDATE meta SET value = '99' WHERE key = 'graph_generation'",
                [],
            )
            .unwrap();
        let plain = db_in(&scratch_dir("ms-plain"));
        std::fs::create_dir_all(plain.parent().unwrap()).unwrap();
        Connection::open(&plain)
            .unwrap()
            .execute_batch("CREATE TABLE facts_fts (x TEXT);")
            .unwrap();

        // Act
        let tokens = words("alpha");
        let results = [
            search_facts(&missing, REPO, &tokens, DEFAULT_LIMIT),
            search_facts(&stale, REPO, &tokens, DEFAULT_LIMIT),
            search_facts(&plain, REPO, &tokens, DEFAULT_LIMIT),
        ];

        // Assert
        assert_eq!(results[0], Err(SearchError::NoDatabase));
        assert_eq!(results[1], Err(SearchError::IndexStale));
        assert_eq!(results[2], Err(SearchError::IndexMissing));
    }

    #[test]
    fn a_hit_carries_the_name_description_and_file_of_its_fact() {
        // Arrange
        let db = store(
            "ms-fields",
            &[global("f1", "ferry-notes", "late departures")],
        );

        // Act
        let hits = search_facts(&db, REPO, &words("ferry"), DEFAULT_LIMIT).unwrap();

        // Assert
        let hit = &hits[0];
        assert_eq!(
            (&*hit.fact_id, &*hit.name, &*hit.description, &*hit.file),
            ("f1", "ferry-notes", "late departures", "f1.md")
        );
    }

    #[test]
    fn a_busy_or_locked_database_is_reported_as_busy() {
        // Arrange
        let busy = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(5), None);
        let locked = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(6), None);

        // Act, Assert
        assert_eq!(failed(busy), SearchError::Busy);
        assert_eq!(failed(locked), SearchError::Busy);
    }

    #[test]
    fn a_fact_is_found_by_a_word_only_its_body_holds() {
        // Arrange
        let db = db_in(&scratch_dir("ms-body"));
        let nodes = [global("f1", "ferry-notes", "late departures")];
        write_graph_with_bodies(&db, &nodes, &[], |_| {
            Some("the quokka is not on board".into())
        })
        .unwrap();

        // Act
        let found = ids(&db, "quokka");

        // Assert
        assert_eq!(found, ["f1"]);
    }

    #[test]
    fn a_name_hit_outranks_the_same_word_in_a_body() {
        // Arrange
        let db = db_in(&scratch_dir("ms-body-rank"));
        let nodes = [
            global("in-body", "zzz", "d"),
            global("in-name", "quokka-notes", "d"),
        ];
        write_graph_with_bodies(&db, &nodes, &[], |file| {
            (file == "in-body.md").then(|| "quokka quokka quokka".to_string())
        })
        .unwrap();

        // Act
        let found = ids(&db, "quokka");

        // Assert
        assert_eq!(found, ["in-name", "in-body"]);
    }

    #[test]
    fn a_reader_in_the_window_before_the_body_pass_is_told_the_index_is_stale() {
        // Arrange
        let db = db_in(&scratch_dir("ms-window"));
        memory::stop_before_bodies(true);
        let _ = write_graph_with_bodies(&db, &[global("a", "alpha", "d")], &[], |_| None);
        memory::stop_before_bodies(false);

        // Act
        let result = search_facts(&db, REPO, &words("alpha"), DEFAULT_LIMIT);

        // Assert
        assert_eq!(result, Err(SearchError::IndexStale));
    }
}
