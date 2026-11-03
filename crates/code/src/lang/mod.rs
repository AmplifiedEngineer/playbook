// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Language registry and parsing. Adding a language is one `Lang` entry and
//! one extractor module. Extractors walk the tree with plain node access and
//! use no Tree-sitter queries.

pub mod go;
pub mod python;
pub mod rust;
pub mod typescript;

use tree_sitter::{Parser, Tree};

/// A language the index can parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    Python,
    Go,
}

/// Every supported language, in a stable order.
pub const ALL: [Lang; 6] = [
    Lang::Rust,
    Lang::TypeScript,
    Lang::Tsx,
    Lang::JavaScript,
    Lang::Python,
    Lang::Go,
];

impl Lang {
    /// The language of a repo relative path, by extension, or `None`.
    pub fn for_path(path: &str) -> Option<Lang> {
        let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
        match ext.as_str() {
            "rs" => Some(Lang::Rust),
            "ts" | "mts" | "cts" => Some(Lang::TypeScript),
            "tsx" => Some(Lang::Tsx),
            "js" | "jsx" | "mjs" | "cjs" => Some(Lang::JavaScript),
            "py" | "pyi" => Some(Lang::Python),
            "go" => Some(Lang::Go),
            _ => None,
        }
    }

    /// The name stored in the index and used by `--lang`. TSX files are
    /// `typescript`, because users do not tell the two apart.
    pub fn name(self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::TypeScript | Lang::Tsx => "typescript",
            Lang::JavaScript => "javascript",
            Lang::Python => "python",
            Lang::Go => "go",
        }
    }

    /// The grammar a file is parsed with. Unlike `name`, it keeps `tsx` apart
    /// from `typescript`, because the two grammars read the same bytes
    /// differently.
    pub fn grammar_id(self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::TypeScript => "typescript",
            Lang::Tsx => "tsx",
            Lang::JavaScript => "javascript",
            Lang::Python => "python",
            Lang::Go => "go",
        }
    }

    fn grammar(self) -> tree_sitter::Language {
        match self {
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::Go => tree_sitter_go::LANGUAGE.into(),
        }
    }
}

/// The grammar id stored for a file that no grammar parses. Such a file still
/// gets chunks and full text rows.
pub const TEXT_GRAMMAR: &str = "text";

/// Bump this number whenever extractor output changes for the same bytes. The
/// digest test in `index.rs` fails until it is bumped, so stored content is
/// never trusted across two different extractors.
pub const EXTRACTOR_REVISION: u32 = 1;

/// The text of this crate's manifest, read at build time for the grammar pins.
const CARGO_TOML: &str = include_str!("../../Cargo.toml");

/// Hash of the `tree-sitter*` dependency lines of a `Cargo.toml` text. A
/// grammar upgrade changes the value, so content parsed by the old grammar is
/// parsed again.
pub fn hash_pins(text: &str) -> String {
    let pins: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("tree-sitter"))
        .collect();
    playbook_common::hash::sha256_hex(pins.join("\n").as_bytes())
}

/// Hash of the extractor sources. Debug builds use it in place of
/// `EXTRACTOR_REVISION`, so a developer who edits an extractor never reads
/// stale content from the same store.
#[cfg(debug_assertions)]
fn sources_hash(sources: &[&str]) -> String {
    playbook_common::hash::sha256_hex(sources.join("\u{1f}").as_bytes())
}

#[cfg(debug_assertions)]
fn extractor_revision() -> String {
    sources_hash(&[
        include_str!("mod.rs"),
        include_str!("go.rs"),
        include_str!("python.rs"),
        include_str!("rust.rs"),
        include_str!("typescript.rs"),
    ])
}

#[cfg(not(debug_assertions))]
fn extractor_revision() -> String {
    EXTRACTOR_REVISION.to_string()
}

/// Names the parser that produced stored content: the tool version, the
/// extractor revision and the grammar pins. Equal keys mean equal output for
/// equal bytes.
pub fn parser_key() -> String {
    static KEY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    KEY.get_or_init(|| {
        format!(
            "{}+{}+{}",
            crate::host::tool_version(),
            extractor_revision(),
            hash_pins(CARGO_TOML)
        )
    })
    .clone()
}

/// A parsed file.
pub struct Parsed {
    pub tree: Tree,
    /// The tree holds syntax errors. Symbols that were extracted are kept.
    pub partial: bool,
}

/// Parses `bytes` as `lang`. A leading UTF-8 byte order mark is blanked with
/// spaces first, so byte offsets still match the file.
pub fn parse(lang: Lang, bytes: &[u8]) -> Result<Parsed, String> {
    let mut parser = Parser::new();
    parser
        .set_language(&lang.grammar())
        .map_err(|e| format!("could not load the {} grammar: {e}", lang.name()))?;
    let blanked;
    let source = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        let mut copy = bytes.to_vec();
        copy[..3].fill(b' ');
        blanked = copy;
        &blanked[..]
    } else {
        bytes
    };
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| format!("the {} parser gave up", lang.name()))?;
    let partial = tree.root_node().has_error();
    Ok(Parsed { tree, partial })
}

/// `prefix.name`, or just `name` at the top level. The qualified-name rule for
/// languages whose scopes join with a dot; Rust joins with `::` in its own `join`.
pub(crate) fn join_dotted(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}.{name}")
    }
}

/// The symbol with this qualified name; panics with the list when absent.
#[cfg(test)]
pub(crate) fn find<'a>(symbols: &'a [Symbol], qualified: &str) -> &'a Symbol {
    symbols
        .iter()
        .find(|s| s.qualified_name == qualified)
        .unwrap_or_else(|| panic!("no symbol {qualified} in {symbols:#?}"))
}

/// One declaration found in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub qualified_name: String,
    pub kind: &'static str,
    pub signature: Option<String>,
    /// The signature was cut at the length cap.
    pub signature_cut: bool,
    pub start_byte: usize,
    pub end_byte: usize,
    /// 1 based.
    pub start_line: usize,
    pub end_line: usize,
    /// Index of the enclosing symbol in the same list.
    pub parent: Option<usize>,
    pub exported: bool,
}

/// One import statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub module: String,
    pub names: Option<String>,
    /// 1 based.
    pub line: usize,
}

#[derive(Debug, Default)]
pub struct Extracted {
    pub symbols: Vec<Symbol>,
    pub imports: Vec<Import>,
}

/// Longest stored signature, in characters.
pub const MAX_SIGNATURE_CHARS: usize = 200;

/// Collapses whitespace and caps the length. The flag says it was cut.
pub fn clean_signature(raw: &str) -> (String, bool) {
    let mut out = String::new();
    let mut count = 0usize;
    for word in raw.split_whitespace() {
        let sep = usize::from(!out.is_empty());
        for ch in std::iter::repeat_n(' ', sep).chain(word.chars()) {
            if count == MAX_SIGNATURE_CHARS {
                return (out, true);
            }
            out.push(ch);
            count += 1;
        }
    }
    (out, false)
}

/// Node kinds that hold an identifier a reference search can match.
pub fn identifier_kinds(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Rust => &[
            "identifier",
            "type_identifier",
            "field_identifier",
            "shorthand_field_identifier",
        ],
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => &[
            "identifier",
            "property_identifier",
            "type_identifier",
            "shorthand_property_identifier",
            "shorthand_property_identifier_pattern",
        ],
        Lang::Python => &["identifier"],
        Lang::Go => &[
            "identifier",
            "type_identifier",
            "field_identifier",
            "package_identifier",
        ],
    }
}

/// Parent node kinds whose `name` field declares something.
pub fn declaration_kinds(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Rust => &[
            "function_item",
            "function_signature_item",
            "struct_item",
            "enum_item",
            "union_item",
            "trait_item",
            "type_item",
            "const_item",
            "static_item",
            "mod_item",
            "macro_definition",
            "associated_type",
        ],
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => &[
            "function_declaration",
            "generator_function_declaration",
            "function_signature",
            "class_declaration",
            "abstract_class_declaration",
            "interface_declaration",
            "type_alias_declaration",
            "enum_declaration",
            "internal_module",
            "module",
            "variable_declarator",
            "method_definition",
            "method_signature",
            "abstract_method_signature",
            "public_field_definition",
        ],
        Lang::Python => &["function_definition", "class_definition"],
        Lang::Go => &[
            "function_declaration",
            "method_declaration",
            "method_elem",
            "type_spec",
            "type_alias",
            "const_spec",
            "var_spec",
        ],
    }
}

/// Node kinds whose subtree is an import, so names inside are import uses.
pub fn import_kinds(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Rust => &["use_declaration"],
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => &["import_statement"],
        Lang::Python => &[
            "import_statement",
            "import_from_statement",
            "future_import_statement",
        ],
        Lang::Go => &["import_declaration"],
    }
}

/// Extracts symbols and imports from a parsed file.
pub fn extract(lang: Lang, parsed: &Parsed, source: &[u8]) -> Extracted {
    match lang {
        Lang::Rust => rust::extract(parsed.tree.root_node(), source),
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => {
            typescript::extract(parsed.tree.root_node(), source)
        }
        Lang::Python => python::extract(parsed.tree.root_node(), source),
        Lang::Go => go::extract(parsed.tree.root_node(), source),
    }
}

/// Shared edge cases every extractor runs: empty input, a byte order mark with
/// CRLF endings, invalid UTF-8 inside a comment, and a syntax error after a
/// good declaration. `decl` is one valid declaration named `name`, `broken` is
/// text that cannot parse, and `comment` starts a line comment.
#[cfg(test)]
pub(crate) fn edge_cases(lang: Lang, decl: &str, name: &str, broken: &str, comment: &str) {
    let run = |bytes: &[u8]| {
        let parsed = parse(lang, bytes).expect("parse");
        let out = extract(lang, &parsed, bytes);
        (parsed.partial, out)
    };

    let (partial, out) = run(b"");
    assert!(!partial && out.symbols.is_empty(), "{lang:?} empty");

    let bom = format!("\u{feff}\r\n{}\r\n", decl.replace('\n', "\r\n"));
    let (partial, out) = run(bom.as_bytes());
    assert!(!partial, "{lang:?} bom and crlf");
    let found = out
        .symbols
        .iter()
        .find(|s| s.name == name)
        .expect("decl after bom");
    assert_eq!(found.start_line, 2, "{lang:?} bom line");

    let mut bytes = format!("{comment} ").into_bytes();
    bytes.extend_from_slice(&[0xFF, 0xFE]);
    bytes.extend_from_slice(format!("\n{decl}\n").as_bytes());
    let (_, out) = run(&bytes);
    let found = out
        .symbols
        .iter()
        .find(|s| s.name == name)
        .expect("decl after bad bytes");
    assert_eq!(found.start_line, 2, "{lang:?} invalid utf8 line");

    let tail = format!("{decl}\n{broken}\n");
    let (partial, out) = run(tail.as_bytes());
    assert!(partial, "{lang:?} partial");
    assert!(
        out.symbols.iter().any(|s| s.name == name),
        "{lang:?} keeps earlier symbols"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Sample {
        lang: Lang,
        root_kind: &'static str,
        valid: &'static str,
        broken: &'static str,
        /// The kind of the first named child of the root in `valid`.
        first_kind: &'static str,
    }

    const SAMPLES: [Sample; 6] = [
        Sample {
            lang: Lang::Rust,
            root_kind: "source_file",
            valid: "fn main() {}\n",
            broken: "fn main( {",
            first_kind: "function_item",
        },
        Sample {
            lang: Lang::TypeScript,
            root_kind: "program",
            valid: "function f(): number { return 1 }\n",
            broken: "function f( {",
            first_kind: "function_declaration",
        },
        Sample {
            lang: Lang::Tsx,
            root_kind: "program",
            valid: "const A = () => <div/>;\n",
            broken: "const A = <div",
            first_kind: "lexical_declaration",
        },
        Sample {
            lang: Lang::JavaScript,
            root_kind: "program",
            valid: "function f() { return 1 }\n",
            broken: "function f( {",
            first_kind: "function_declaration",
        },
        Sample {
            lang: Lang::Python,
            root_kind: "module",
            valid: "def f():\n    return 1\n",
            broken: "def f(:\n",
            first_kind: "function_definition",
        },
        Sample {
            lang: Lang::Go,
            root_kind: "source_file",
            valid: "func f() int { return 1 }\n",
            broken: "func f( {",
            first_kind: "function_declaration",
        },
    ];

    #[test]
    fn every_grammar_parses_a_valid_sample_without_errors() {
        for s in &SAMPLES {
            // Arrange, Act
            let parsed = parse(s.lang, s.valid.as_bytes()).expect("parse");

            // Assert
            let root = parsed.tree.root_node();
            assert_eq!(root.kind(), s.root_kind, "{:?}", s.lang);
            assert!(!parsed.partial, "{:?}", s.lang);
            assert_eq!(root.named_child(0).map(|n| n.kind()), Some(s.first_kind));
        }
    }

    #[test]
    fn broken_syntax_is_partial_but_still_gives_a_tree() {
        for s in &SAMPLES {
            // Arrange, Act
            let parsed = parse(s.lang, s.broken.as_bytes()).expect("parse");

            // Assert
            assert!(parsed.partial, "{:?}", s.lang);
            assert_eq!(parsed.tree.root_node().kind(), s.root_kind, "{:?}", s.lang);
        }
    }

    #[test]
    fn an_empty_file_gives_an_empty_root_for_every_grammar() {
        for s in &SAMPLES {
            // Arrange, Act
            let parsed = parse(s.lang, b"").expect("parse");

            // Assert
            assert_eq!(parsed.tree.root_node().child_count(), 0, "{:?}", s.lang);
            assert!(!parsed.partial, "{:?}", s.lang);
        }
    }

    #[test]
    fn crlf_endings_and_a_byte_order_mark_keep_line_numbers_right() {
        for s in &SAMPLES {
            // Arrange: a blank first line, then the sample, all with CRLF and a BOM.
            let text = format!("\u{feff}\r\n{}", s.valid.replace('\n', "\r\n"));

            // Act
            let parsed = parse(s.lang, text.as_bytes()).expect("parse");

            // Assert
            let first = parsed.tree.root_node().named_child(0).expect("first node");
            assert_eq!(first.start_position().row, 1, "{:?}", s.lang);
            assert_eq!(first.kind(), s.first_kind, "{:?}", s.lang);
            assert!(!parsed.partial, "{:?}", s.lang);
        }
    }

    #[test]
    fn extensions_map_to_languages_and_unknown_ones_to_none() {
        // Arrange, Act, Assert
        let cases = [
            ("a.rs", Some(Lang::Rust)),
            ("a.ts", Some(Lang::TypeScript)),
            ("a.mts", Some(Lang::TypeScript)),
            ("a.cts", Some(Lang::TypeScript)),
            ("a.tsx", Some(Lang::Tsx)),
            ("a.js", Some(Lang::JavaScript)),
            ("a.jsx", Some(Lang::JavaScript)),
            ("a.mjs", Some(Lang::JavaScript)),
            ("a.cjs", Some(Lang::JavaScript)),
            ("a.py", Some(Lang::Python)),
            ("a.pyi", Some(Lang::Python)),
            ("a.go", Some(Lang::Go)),
            ("a_test.go", Some(Lang::Go)),
            ("A.RS", Some(Lang::Rust)),
            ("a.txt", None),
            ("Makefile", None),
            ("dir.rs/file", None),
        ];
        for (path, want) in cases {
            assert_eq!(Lang::for_path(path), want, "{path}");
        }
    }

    #[test]
    fn signatures_are_cut_at_two_hundred_characters_and_marked() {
        // Arrange, Act
        let short = clean_signature(&"a".repeat(199));
        let exact = clean_signature(&"a".repeat(200));
        let long = clean_signature(&"a".repeat(201));

        // Assert
        assert_eq!((short.0.len(), short.1), (199, false));
        assert_eq!((exact.0.len(), exact.1), (200, false));
        assert_eq!((long.0.len(), long.1), (200, true));
        assert_eq!(clean_signature("fn  a(\n  x: i32 )").0, "fn a( x: i32 )");
    }

    #[test]
    fn hash_pins_follows_the_tree_sitter_lines_only() {
        // Arrange
        let base = "[dependencies]\nserde = \"1\"\ntree-sitter-rust = \"=0.24.2\"\n";
        let bumped = "[dependencies]\nserde = \"1\"\ntree-sitter-rust = \"=0.24.3\"\n";
        let other_dep = "[dependencies]\nserde = \"2\"\ntree-sitter-rust = \"=0.24.2\"\n";

        // Assert
        assert_eq!(hash_pins(base), hash_pins(base));
        assert_ne!(hash_pins(base), hash_pins(bumped));
        assert_eq!(hash_pins(base), hash_pins(other_dep));
    }

    #[test]
    fn the_crate_manifest_has_pins_to_hash() {
        assert_ne!(hash_pins(CARGO_TOML), hash_pins(""));
    }

    #[test]
    fn grammar_id_keeps_tsx_apart_and_every_id_is_distinct() {
        // Arrange
        let ids: std::collections::HashSet<&str> = ALL.iter().map(|l| l.grammar_id()).collect();

        // Assert
        assert_eq!(ids.len(), ALL.len());
        assert_eq!(Lang::Tsx.grammar_id(), "tsx");
        assert_eq!(Lang::Tsx.name(), Lang::TypeScript.name());
        assert!(!ids.contains(TEXT_GRAMMAR));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_changed_extractor_source_changes_the_debug_revision() {
        assert_eq!(sources_hash(&["a", "b"]), sources_hash(&["a", "b"]));
        assert_ne!(sources_hash(&["a", "b"]), sources_hash(&["a", "c"]));
    }

    #[test]
    fn the_parser_key_names_version_revision_and_pins() {
        // Act
        let key = parser_key();

        // Assert
        assert!(key.starts_with(crate::host::tool_version()));
        assert_eq!(key.split('+').count(), 3);
        assert_eq!(key, parser_key());
    }
}
