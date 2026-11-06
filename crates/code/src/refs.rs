// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Syntax references: identifiers in code whose text equals the target.
//! Comments and strings are skipped because they hold no identifier nodes. Every
//! result is a candidate. When a name is declared in several files, scope and
//! import hints pick the declaring file a reference binds to. A reference that
//! cannot be bound, or a lookup that names no module, is kept and labelled.

use super::lang::{self, Lang};
use super::search::{load, Loaded};
use playbook_store::Connection;
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
    /// Set only when the name is declared in several files and scope cannot
    /// say which declaration this reference means.
    pub label: Option<&'static str>,
}

/// The label for a reference whose declaration scope cannot decide.
pub const CANNOT_KNOW: &str = "cannot know";
/// File stems that stand for their parent directory as a module name.
const ENTRY_STEMS: [&str; 3] = ["mod", "index", "__init__"];
/// File stems that stand for the crate root.
const CRATE_ROOT_STEMS: [&str; 2] = ["lib", "main"];
/// The module word that names the crate root in a Rust path.
const CRATE_WORD: &str = "crate";
/// Declarations in at least this many files make a name ambiguous.
const AMBIGUOUS_DECLARATIONS: usize = 2;

/// A found reference with the hints scope resolution needs.
struct Found {
    r: Ref,
    qualifier: Option<String>,
    import_words: Vec<String>,
}

struct FileScan {
    path: String,
    found: Vec<Found>,
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Module names a file answers to in paths and import statements.
fn module_names(path: &str) -> Vec<String> {
    let p = Path::new(path);
    let stem = p
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    let dir = p
        .parent()
        .and_then(|d| d.file_name())
        .and_then(|d| d.to_str())
        .map(str::to_string);
    let mut names = Vec::new();
    if ENTRY_STEMS.contains(&stem.as_str()) {
        names.extend(dir.clone());
    } else {
        names.push(stem.clone());
    }
    if CRATE_ROOT_STEMS.contains(&stem.as_str()) {
        names.push(CRATE_WORD.to_string());
    }
    if Lang::for_path(path) == Some(Lang::Go) {
        names.extend(dir);
    }
    names
}

/// The module segment written before the name in a target such as `a::run`.
fn target_qualifier(target: &str) -> Option<String> {
    let parts: Vec<&str> = target
        .trim()
        .split([':', '.', '/'])
        .filter(|s| !s.is_empty())
        .collect();
    parts
        .len()
        .checked_sub(2)
        .and_then(|i| parts.get(i))
        .map(|s| s.to_string())
}

/// The identifier written right before the name, as in `a::name` or `a.name`.
fn textual_qualifier(line: &str, byte_col: usize) -> Option<String> {
    let before = line.get(..byte_col)?;
    let before = before
        .strip_suffix("::")
        .or_else(|| before.strip_suffix('.'))?;
    let word: String = before
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    (!word.is_empty()).then_some(word)
}

/// Splits `statement` into identifier words.
fn words_of(statement: &[u8]) -> Vec<String> {
    statement
        .split(|b| !is_word_byte(*b))
        .filter(|w| !w.is_empty())
        .map(|w| String::from_utf8_lossy(w).into_owned())
        .collect()
}

/// Narrows references when the name is declared in several files.
fn resolve_scope(files: Vec<FileScan>, target: &str) -> Vec<Ref> {
    let declaring: Vec<(String, Vec<String>)> = files
        .iter()
        .filter(|f| f.found.iter().any(|x| x.r.role == "declaration"))
        .map(|f| (f.path.clone(), module_names(&f.path)))
        .collect();
    if declaring.len() < AMBIGUOUS_DECLARATIONS {
        return files
            .into_iter()
            .flat_map(|f| f.found.into_iter().map(|x| x.r))
            .collect();
    }
    let only_match = |words: &[String]| -> Option<String> {
        let hits: Vec<&String> = declaring
            .iter()
            .filter(|(_, mods)| mods.iter().any(|m| words.contains(m)))
            .map(|(p, _)| p)
            .collect();
        (hits.len() == 1).then(|| hits[0].clone())
    };
    let selected = target_qualifier(target).and_then(|q| only_match(&[q]));
    let mut out = Vec::new();
    for file in files {
        let declares = declaring.iter().any(|(p, _)| *p == file.path);
        let imported: Vec<String> = file
            .found
            .iter()
            .flat_map(|x| x.import_words.iter().cloned())
            .collect();
        let via_import = only_match(&imported);
        for mut x in file.found {
            let bound = if x.r.role == "declaration" || declares {
                Some(file.path.clone())
            } else {
                x.qualifier
                    .as_ref()
                    .and_then(|q| only_match(std::slice::from_ref(q)))
                    .or_else(|| via_import.clone())
            };
            match (&selected, bound) {
                (Some(s), Some(b)) if *s != b => continue,
                (Some(_), Some(_)) => {}
                _ => x.r.label = Some(CANNOT_KNOW),
            }
            out.push(x.r);
        }
    }
    out
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
        .query_map(playbook_store::params![query, limit], |r| {
            r.get::<_, String>(0)
        })
        .map_err(|e| super::db::map_sql_error("refs failed", &e))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| super::db::map_sql_error("refs failed", &e))
}

/// Finds identifier uses of `opts.target` in candidate files.
pub fn find_refs(conn: &Connection, root: &Path, opts: &RefsOptions) -> Result<RefsResult, String> {
    let name = target_name(&opts.target);
    let mut result = RefsResult::default();
    let files = candidate_files(conn, name, CANDIDATE_LIMIT)?;
    let mut scans = Vec::new();
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
        let found = scan_file(language, &loaded, &path, name);
        scans.push(FileScan { path, found });
    }
    let mut total = 0usize;
    for found in resolve_scope(scans, &opts.target) {
        total += 1;
        if result.refs.len() < opts.limit {
            result.refs.push(found);
        }
    }
    result.omitted = total.saturating_sub(result.refs.len());
    result.truncated = result.omitted > 0 || result.files_capped;
    Ok(result)
}

/// Identifier matches in one parsed file, in source order.
fn scan_file(language: Lang, loaded: &Loaded, path: &str, name: &str) -> Vec<Found> {
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
            let qualifier = line_text.and_then(|l| textual_qualifier(l, start.column));
            let import_words = match import_statement(node, imports) {
                Some(stmt) => words_of(bytes.get(stmt.byte_range()).unwrap_or_default()),
                None => Vec::new(),
            };
            out.push(Found {
                r: Ref {
                    path: path.to_string(),
                    line: start.row + 1,
                    column,
                    role,
                    text,
                    label: None,
                },
                qualifier,
                import_words,
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

/// The import statement that holds `node`, found within a few hops.
fn import_statement<'t>(
    node: tree_sitter::Node<'t>,
    imports: &[&str],
) -> Option<tree_sitter::Node<'t>> {
    let mut up = node.parent();
    let mut hops = 0;
    while let Some(parent) = up {
        if imports.contains(&parent.kind()) {
            return Some(parent);
        }
        hops += 1;
        if hops > 32 {
            break;
        }
        up = parent.parent();
    }
    None
}

fn role_of(node: tree_sitter::Node, imports: &[&str], decls: &[&str]) -> &'static str {
    if import_statement(node, imports).is_some() {
        return "import";
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

    /// `run` is declared in `a.rs` and `b.rs`. `c.rs` imports it from `a`.
    /// `d.rs` calls a `run` that nothing in the file says where it comes from.
    fn same_name_fixture(tag: &str) -> Fx {
        let f = fx(tag);
        f.write("a.rs", "pub fn run() {}\nfn go() { run(); }\n");
        f.write("b.rs", "pub fn run() {}\nfn go() { run(); }\n");
        f.write("c.rs", "use crate::a::run;\nfn go() { run(); }\n");
        f.write("d.rs", "fn go() { run(); }\n");
        f.run();
        f
    }

    fn files_of(r: &RefsResult) -> Vec<&str> {
        r.refs.iter().map(|x| x.path.as_str()).collect()
    }

    #[test]
    fn same_name_in_another_module_is_dropped_and_oracle_negatives_fall_from_3_to_1() {
        let _g = pinned();
        // Arrange: the oracle says a.rs and c.rs hold the true references.
        let f = same_name_fixture("rf-same-name");
        let negatives = |r: &RefsResult| {
            r.refs
                .iter()
                .filter(|x| !matches!(x.path.as_str(), "a.rs" | "c.rs"))
                .count()
        };

        // Act
        let baseline = refs(&f, "run");
        let scoped = refs(&f, "a::run");

        // Assert
        assert_eq!(negatives(&baseline), 3);
        assert_eq!(negatives(&scoped), 1);
        assert_eq!(
            files_of(&scoped),
            vec!["a.rs", "a.rs", "c.rs", "c.rs", "d.rs"]
        );
        assert_eq!(scoped.refs.iter().filter(|x| x.label.is_some()).count(), 1);
        assert_eq!(scoped.refs[4].label, Some(CANNOT_KNOW));
    }

    #[test]
    fn a_name_that_scope_resolves_has_no_label() {
        let _g = pinned();
        // Arrange
        let f = fx("rf-resolved");
        f.write("a.ts", "export function run() {}\n");
        f.write("b.ts", "export function run() {}\n");
        f.write("c.ts", "import { run } from './b';\nrun();\n");
        f.run();

        // Act
        let got = refs(&f, "b.run");

        // Assert
        assert_eq!(files_of(&got), vec!["b.ts", "c.ts", "c.ts"]);
        assert!(got.refs.iter().all(|x| x.label.is_none()));
    }

    #[test]
    fn an_unambiguous_name_has_no_label() {
        let _g = pinned();
        // Arrange
        let f = fx("rf-unambiguous");
        f.write("a.rs", "pub fn run() {}\n");
        f.write("d.rs", "fn go() { run(); }\n");
        f.run();

        // Act
        let got = refs(&f, "run");

        // Assert
        assert_eq!(got.refs.len(), 2);
        assert!(got.refs.iter().all(|x| x.label.is_none()));
    }

    #[test]
    fn an_ambiguous_name_with_no_module_in_the_target_keeps_every_reference_labelled() {
        let _g = pinned();
        // Arrange
        let f = same_name_fixture("rf-ambiguous");

        // Act
        let got = refs(&f, "run");

        // Assert
        assert_eq!(got.refs.len(), 7);
        assert!(got.refs.iter().all(|x| x.label == Some(CANNOT_KNOW)));
    }
}
