// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Syntax references: identifiers in code whose text equals the target.
//! Comments and strings are skipped because they hold no identifier nodes. Every
//! result is a candidate, since no scope or type analysis runs.

use super::lang::{self, Lang};
use super::search::{load, Loaded};
use rusqlite::Connection;
use std::path::Path;

/// Default and maximum result counts.
pub const DEFAULT_LIMIT: usize = 50;
/// Most candidate files parsed in one run.
pub const DEFAULT_FILE_CAP: usize = 200;

#[derive(Debug, Clone)]
pub struct RefsOptions {
    pub target: String,
    pub limit: usize,
    pub file_cap: usize,
}

impl RefsOptions {
    pub fn new(target: &str) -> Self {
        Self {
            target: target.to_string(),
            limit: DEFAULT_LIMIT,
            file_cap: DEFAULT_FILE_CAP,
        }
    }
}

/// One place the target appears as an identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ref {
    pub path: String,
    pub line: usize,
    /// 1 based, in characters.
    pub column: usize,
    /// `declaration`, `import` or `use`.
    pub role: &'static str,
    pub text: String,
}

#[derive(Debug, Default)]
pub struct RefsResult {
    pub refs: Vec<Ref>,
    pub truncated: bool,
    pub omitted: usize,
    pub files_scanned: usize,
    /// More candidate files matched than `file_cap` allows.
    pub files_capped: bool,
}

/// The identifier to look for: the last segment of a dotted or scoped name.
pub fn target_name(target: &str) -> &str {
    let t = target.trim();
    let after_colons = t.rsplit("::").next().unwrap_or(t);
    after_colons.rsplit('.').next().unwrap_or(after_colons)
}

/// Most index rows read when looking for candidate files.
const CANDIDATE_LIMIT: usize = 5000;

/// Whether `haystack` holds `needle` with no identifier character on either side.
fn has_word(haystack: &[u8], needle: &[u8]) -> bool {
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80;
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    (0..=haystack.len() - needle.len()).any(|i| {
        &haystack[i..i + needle.len()] == needle
            && (i == 0 || !is_word(haystack[i - 1]))
            && haystack.get(i + needle.len()).is_none_or(|b| !is_word(*b))
    })
}

/// Candidate files: supported files whose text holds the target as a word.
fn candidate_files(conn: &Connection, name: &str, limit: usize) -> Result<Vec<String>, String> {
    let Some(query) = super::search::fts_query(name) else {
        return Ok(Vec::new());
    };
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT f.path FROM chunks_fts
             JOIN main.chunks c ON c.id = chunks_fts.rowid
             JOIN files f ON f.id = c.content
             WHERE chunks_fts MATCH ?1 AND f.lang IS NOT NULL
             ORDER BY f.path LIMIT ?2",
        )
        .map_err(|e| super::db::map_sql_error("refs failed", &e))?;
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let rows = stmt
        .query_map(rusqlite::params![query, limit], |r| r.get::<_, String>(0))
        .map_err(|e| super::db::map_sql_error("refs failed", &e))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| super::db::map_sql_error("refs failed", &e))
}

/// Finds identifier uses of `opts.target` in candidate files.
pub fn find_refs(conn: &Connection, root: &Path, opts: &RefsOptions) -> Result<RefsResult, String> {
    let name = target_name(&opts.target);
    let mut result = RefsResult::default();
    let files = candidate_files(conn, name, CANDIDATE_LIMIT)?;
    let mut total = 0usize;
    for path in files {
        let Some(language) = Lang::for_path(&path) else {
            continue;
        };
        let Some(loaded) = load(root, &path) else {
            continue;
        };
        // The text index folds case and splits identifiers, so check that the
        // file really spells the name before it counts against the cap.
        if !has_word(loaded.bytes(), name.as_bytes()) {
            continue;
        }
        if result.files_scanned >= opts.file_cap {
            result.files_capped = true;
            break;
        }
        result.files_scanned += 1;
        for found in scan_file(language, &loaded, &path, name) {
            total += 1;
            if result.refs.len() < opts.limit {
                result.refs.push(found);
            }
        }
    }
    result.omitted = total.saturating_sub(result.refs.len());
    result.truncated = result.omitted > 0 || result.files_capped;
    Ok(result)
}

/// Identifier matches in one parsed file, in source order.
fn scan_file(language: Lang, loaded: &Loaded, path: &str, name: &str) -> Vec<Ref> {
    let Ok(parsed) = lang::parse(language, loaded.bytes()) else {
        return Vec::new();
    };
    let kinds = lang::identifier_kinds(language);
    let imports = lang::import_kinds(language);
    let decls = lang::declaration_kinds(language);
    let mut out = Vec::new();
    let mut cursor = parsed.tree.walk();
    let bytes = loaded.bytes();
    // Depth-first without recursion, so a deeply nested file cannot overflow the stack.
    'walk: loop {
        let node = cursor.node();
        if kinds.contains(&node.kind()) && bytes.get(node.byte_range()) == Some(name.as_bytes()) {
            let start = node.start_position();
            let role = role_of(node, imports, decls);
            let line_text = loaded
                .lines
                .get(start.row)
                .map(|l| l.trim_start_matches('\u{feff}'));
            let text = line_text
                .map(|l| l.trim().chars().take(200).collect::<String>())
                .unwrap_or_default();
            let column = line_text.map_or(start.column + 1, |l| {
                let skipped = if start.row == 0 && loaded.lines[0].starts_with('\u{feff}') {
                    3
                } else {
                    0
                };
                char_column(l, start.column.saturating_sub(skipped))
            });
            out.push(Ref {
                path: path.to_string(),
                line: start.row + 1,
                column,
                role,
                text,
            });
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                continue 'walk;
            }
            if !cursor.goto_parent() {
                break 'walk;
            }
        }
    }
    out
}

/// The 1 based character column for a 0 based byte column in `line`.
fn char_column(line: &str, byte_col: usize) -> usize {
    let mut end = byte_col.min(line.len());
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end].chars().count() + 1
}

fn role_of(node: tree_sitter::Node, imports: &[&str], decls: &[&str]) -> &'static str {
    let mut up = node.parent();
    let mut hops = 0;
    while let Some(parent) = up {
        if imports.contains(&parent.kind()) {
            return "import";
        }
        hops += 1;
        if hops > 32 {
            break;
        }
        up = parent.parent();
    }
    match node.parent() {
        Some(p) if decls.contains(&p.kind()) && p.child_by_field_name("name") == Some(node) => {
            "declaration"
        }
        _ => "use",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{fx, pinned, Fx};

    fn refs(f: &Fx, target: &str) -> RefsResult {
        find_refs(&f.conn(), &f.root, &RefsOptions::new(target)).expect("refs")
    }

    fn shown(r: &RefsResult) -> Vec<(&str, usize, &str)> {
        r.refs
            .iter()
            .map(|x| (x.path.as_str(), x.line, x.role))
            .collect()
    }

    #[test]
    fn code_uses_are_found_and_comments_and_strings_are_skipped() {
        let _g = pinned();
        // Arrange: each source has the target in a comment and a string too.
        let cases: [(&str, &str, Vec<usize>); 3] = [
            (
                "a.rs",
                "fn target() {}\nfn run() { target(); let t: target = 1; }\n// target\nconst S: &str = \"target\";\n",
                vec![1, 2, 2],
            ),
            (
                "a.ts",
                "function target() {}\nconst x = target();\nconst y = obj.target;\n// target\nconst s = \"target\";\n",
                vec![1, 2, 3],
            ),
            (
                "a.py",
                "def target():\n    pass\nx = target()\ny = obj.target\n# target\ns = \"target\"\n",
                vec![1, 3, 4],
            ),
        ];
        for (name, src, want) in cases {
            let f = fx(&format!("rf-{name}"));
            f.write(name, src);
            f.run();

            // Act
            let got = refs(&f, "target");

            // Assert
            let lines: Vec<usize> = got.refs.iter().map(|r| r.line).collect();
            assert_eq!(lines, want, "{name}");
            assert_eq!(got.refs[0].role, "declaration", "{name}");
        }
    }

    #[test]
    fn go_uses_declarations_and_imports_are_told_apart_and_comments_and_strings_skipped() {
        let _g = pinned();
        // Arrange
        let f = fx("rf-go");
        f.write("store.go", crate::lang::go::FIXTURE);
        f.run();

        // Act
        let got = refs(&f, "MaxItems");
        let imported = refs(&f, "log");

        // Assert
        assert_eq!(
            shown(&got),
            vec![("store.go", 9, "declaration"), ("store.go", 28, "use"),]
        );
        assert_eq!(
            shown(&imported),
            vec![("store.go", 5, "import"), ("store.go", 29, "use")]
        );
    }

    #[test]
    fn imports_are_labelled_as_imports() {
        let _g = pinned();
        // Arrange
        let f = fx("rf-import");
        f.write("a.py", "from net import target\ntarget()\n");
        f.write("b.ts", "import { target } from './x';\ntarget();\n");
        f.run();

        // Act
        let got = refs(&f, "target");

        // Assert
        assert_eq!(
            shown(&got),
            vec![
                ("a.py", 1, "import"),
                ("a.py", 2, "use"),
                ("b.ts", 1, "import"),
                ("b.ts", 2, "use"),
            ]
        );
    }

    #[test]
    fn roles_cover_rust_imports_paths_jsx_keyword_arguments_and_destructuring() {
        let _g = pinned();
        // Arrange
        let cases: [(&str, &str, &str); 5] = [
            ("a.rs", "use a::target;\n", "import"),
            ("b.rs", "fn f() { a::target(); }\n", "use"),
            ("c.tsx", "const x = <target />;\n", "use"),
            ("d.py", "f(target=1)\n", "use"),
            ("e.rs", "fn f(x: Foo) { let Foo { target } = x; }\n", "use"),
        ];
        for (name, src, role) in cases {
            let f = fx(&format!("rf-role-{name}"));
            f.write(name, src);
            f.run();

            // Act
            let got = refs(&f, "target");

            // Assert
            assert_eq!(got.refs.len(), 1, "{name}");
            assert_eq!(got.refs[0].role, role, "{name}");
        }
    }

    #[test]
    fn files_that_only_share_a_word_piece_do_not_use_the_file_cap() {
        let _g = pinned();
        // Arrange: `getUser` and `user_id` match the text index for `User` but do not spell it.
        let f = fx("rf-pieces");
        f.write("a.ts", "getUser();\n");
        f.write("b.py", "user_id = 1\n");
        f.write("c.ts", "User();\n");
        f.run();
        let mut opts = RefsOptions::new("User");
        opts.file_cap = 1;

        // Act
        let got = find_refs(&f.conn(), &f.root, &opts).expect("refs");

        // Assert
        assert_eq!(got.files_scanned, 1);
        assert!(!got.files_capped);
        assert_eq!(got.refs[0].path, "c.ts");
    }

    #[test]
    fn a_dotted_target_looks_up_its_last_segment() {
        // Arrange, Act, Assert
        assert_eq!(target_name("Pay.run"), "run");
        assert_eq!(target_name("a::b::go"), "go");
        assert_eq!(target_name("plain"), "plain");
    }

    #[test]
    fn result_limits_of_49_50_and_51_matches() {
        let _g = pinned();
        for (count, shown_len, truncated) in
            [(49usize, 49usize, false), (50, 50, false), (51, 50, true)]
        {
            // Arrange
            let f = fx(&format!("rf-limit-{count}"));
            let body: String = (0..count).map(|_| "target();\n".to_string()).collect();
            f.write("a.ts", &body);
            f.run();

            // Act
            let got = refs(&f, "target");

            // Assert
            assert_eq!(got.refs.len(), shown_len, "{count}");
            assert_eq!(got.truncated, truncated, "{count}");
        }
    }

    #[test]
    fn candidate_file_caps_of_199_200_and_201_files() {
        let _g = pinned();
        for (files, scanned, capped) in [
            (199usize, 199usize, false),
            (200, 200, false),
            (201, 200, true),
        ] {
            // Arrange
            let f = fx(&format!("rf-files-{files}"));
            for i in 0..files {
                f.write(&format!("f{i:03}.ts"), "target();\n");
            }
            f.run();
            let mut opts = RefsOptions::new("target");
            opts.limit = 1000;

            // Act
            let got = find_refs(&f.conn(), &f.root, &opts).expect("refs");

            // Assert
            assert_eq!(got.files_scanned, scanned, "{files}");
            assert_eq!(got.truncated, capped, "{files}");
        }
    }

    #[test]
    fn columns_count_characters_not_bytes() {
        let _g = pinned();
        // Arrange
        let f = fx("rf-column");
        f.write("a.ts", "const caf\u{e9} = target;\n");
        f.run();

        // Act
        let got = refs(&f, "target");

        // Assert
        assert_eq!(got.refs[0].column, 14);
    }

    #[test]
    fn a_damaged_text_index_names_rebuild() {
        let _g = pinned();
        // Arrange
        let f = fx("refs-damaged");
        f.write("a.rs", "fn needle() {}\nfn b() { needle(); }\n");
        f.run();
        let conn = f.conn();
        crate::testkit::damage_pages(&f);

        // Act
        let got = find_refs(&conn, &f.root, &RefsOptions::new("needle"));

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
    fn references_come_from_this_worktree_only() {
        let _g = pinned();
        // Arrange
        let (f, conn) = two_worktrees_sharing("rf-scope");

        // Act
        let got = find_refs(&conn, &f.root, &RefsOptions::new("widget")).expect("refs");

        // Assert
        let mut found: Vec<&str> = got.refs.iter().map(|r| r.path.as_str()).collect();
        found.sort_unstable();
        found.dedup();
        assert_eq!(found, vec!["again.rs", "shared.rs"]);
    }
}
