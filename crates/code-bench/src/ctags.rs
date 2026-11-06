// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The universal-ctags adapter. It lists the symbols of a pinned checkout
//! through an injected process runner. A missing tool or a different version
//! returns a skip with a reason. Raw output is cached by repo, commit and the
//! pinned tool version, and symbols come back sorted.

use crate::corpus::Repo;
use crate::process::{ProcessRunner, RunError};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

const PROGRAM: &str = "ctags";
const BANNER_PREFIX: &str = "Universal Ctags ";
const EXCLUDED_DIRS: [&str; 6] = [".git", "node_modules", "vendor", "target", "dist", "build"];
const TAG_TYPE: &str = "tag";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Interface,
    Enum,
    Trait,
    Type,
    Constant,
}

impl SymbolKind {
    pub const ALL: [SymbolKind; 9] = [
        SymbolKind::Function,
        SymbolKind::Method,
        SymbolKind::Class,
        SymbolKind::Struct,
        SymbolKind::Interface,
        SymbolKind::Enum,
        SymbolKind::Trait,
        SymbolKind::Type,
        SymbolKind::Constant,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
            SymbolKind::Class => "class",
            SymbolKind::Struct => "struct",
            SymbolKind::Interface => "interface",
            SymbolKind::Enum => "enum",
            SymbolKind::Trait => "trait",
            SymbolKind::Type => "type",
            SymbolKind::Constant => "constant",
        }
    }

    /// Maps a ctags kind name; kinds outside the tiers return `None`.
    fn from_ctags(kind: &str) -> Option<Self> {
        Some(match kind {
            "function" | "func" => SymbolKind::Function,
            "method" => SymbolKind::Method,
            "class" => SymbolKind::Class,
            "struct" => SymbolKind::Struct,
            "interface" => SymbolKind::Interface,
            "enum" => SymbolKind::Enum,
            "trait" => SymbolKind::Trait,
            "typedef" | "alias" | "type" => SymbolKind::Type,
            "constant" | "const" => SymbolKind::Constant,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Symbol {
    pub path: String,
    pub line: u32,
    pub name: String,
    pub kind: SymbolKind,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Symbols(Vec<Symbol>),
    Skipped { reason: String },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CtagsError {
    #[error("ctags failed: {0}")]
    Run(String),
    #[error("ctags output line is not valid: {0}")]
    Parse(String),
    #[error("language {0} has no ctags mapping")]
    Language(String),
    #[error("cache io error: {0}")]
    Io(String),
}

#[derive(Deserialize)]
struct Tag {
    #[serde(rename = "_type")]
    kind_of_row: String,
    name: Option<String>,
    path: Option<String>,
    line: Option<u32>,
    kind: Option<String>,
}

fn ctags_language(language: &str) -> Option<&'static str> {
    Some(match language {
        "rust" => "Rust",
        "go" => "Go",
        "javascript" => "JavaScript",
        "typescript" => "TypeScript",
        "python" => "Python",
        _ => return None,
    })
}

fn banner_version(banner: &str) -> Option<&str> {
    let rest = banner.strip_prefix(BANNER_PREFIX)?;
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

fn skip(reason: String) -> Result<Outcome, CtagsError> {
    Ok(Outcome::Skipped { reason })
}

/// Lists the symbols of `checkout`. `pinned` is the manifest's ctags version.
pub fn list_symbols(
    runner: &dyn ProcessRunner,
    pinned: &str,
    repo: &Repo,
    checkout: &Path,
    cache: &Path,
) -> Result<Outcome, CtagsError> {
    let language = ctags_language(&repo.language)
        .ok_or_else(|| CtagsError::Language(repo.language.clone()))?;
    let cached = cache.join(format!(
        "ctags-{}-{}-{pinned}.jsonl",
        repo.name, repo.commit
    ));
    if let Ok(raw) = fs::read_to_string(&cached) {
        return parse(&raw).map(Outcome::Symbols);
    }
    let banner = match runner.run(PROGRAM, &["--version".to_string()], checkout) {
        Ok(text) => text,
        Err(RunError::NotFound) => return skip(format!("{PROGRAM} is not installed")),
        Err(RunError::Failed(e)) => return Err(CtagsError::Run(e)),
    };
    match banner.lines().next().and_then(banner_version) {
        Some(found) if found == pinned => {}
        Some(found) => return skip(format!("ctags {found} is installed, {pinned} is pinned")),
        None => return skip("the installed ctags is not universal-ctags".to_string()),
    }
    let mut args: Vec<String> = ["--output-format=json", "--fields=+n", "-R", "-f", "-"]
        .map(String::from)
        .into();
    args.push(format!("--languages={language}"));
    args.extend(EXCLUDED_DIRS.iter().map(|d| format!("--exclude={d}")));
    args.push(".".to_string());
    let raw = runner
        .run(PROGRAM, &args, checkout)
        .map_err(|e| CtagsError::Run(format!("{e:?}")))?;
    let symbols = parse(&raw)?;
    fs::create_dir_all(cache).map_err(|e| CtagsError::Io(e.to_string()))?;
    fs::write(&cached, raw).map_err(|e| CtagsError::Io(e.to_string()))?;
    Ok(Outcome::Symbols(symbols))
}

fn parse(raw: &str) -> Result<Vec<Symbol>, CtagsError> {
    let mut symbols = Vec::new();
    for line in raw.lines().filter(|l| !l.trim().is_empty()) {
        let tag: Tag =
            serde_json::from_str(line).map_err(|_| CtagsError::Parse(line.to_string()))?;
        if tag.kind_of_row != TAG_TYPE {
            continue;
        }
        let (Some(name), Some(path), Some(line_no), Some(kind)) =
            (tag.name, tag.path, tag.line, tag.kind)
        else {
            return Err(CtagsError::Parse(line.to_string()));
        };
        if let Some(kind) = SymbolKind::from_ctags(&kind) {
            let path = path.strip_prefix("./").unwrap_or(&path).to_string();
            symbols.push(Symbol {
                path,
                line: line_no,
                name,
                kind,
            });
        }
    }
    symbols.sort();
    symbols.dedup();
    Ok(symbols)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::Manifest;
    use playbook_common::test_support::scratch_dir;
    use std::cell::RefCell;
    use std::path::PathBuf;

    const PINNED: &str = "6.2.0";
    const BANNER: &str = "Universal Ctags 6.2.0(abc), Copyright (C) 2015-2024\n";
    const ROWS: &str = concat!(
        "{\"_type\":\"ptag\",\"name\":\"TAG_PROGRAM_NAME\"}\n",
        "{\"_type\":\"tag\",\"name\":\"zed\",\"path\":\"./b.go\",\"line\":9,\"kind\":\"function\"}\n",
        "{\"_type\":\"tag\",\"name\":\"alpha\",\"path\":\"a.go\",\"line\":3,\"kind\":\"struct\"}\n",
        "{\"_type\":\"tag\",\"name\":\"x\",\"path\":\"a.go\",\"line\":5,\"kind\":\"variable\"}\n",
    );

    struct Stub {
        banner: Result<String, RunError>,
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl Stub {
        fn new(banner: Result<String, RunError>) -> Self {
            Self {
                banner,
                calls: RefCell::default(),
            }
        }
    }

    impl ProcessRunner for Stub {
        fn run(&self, _: &str, args: &[String], _: &Path) -> Result<String, RunError> {
            self.calls.borrow_mut().push(args.to_vec());
            if args == ["--version"] {
                return self.banner.clone();
            }
            Ok(ROWS.to_string())
        }
    }

    fn repo(language: &str) -> Repo {
        let mut repo = Manifest::bundled().unwrap().repos.remove(0);
        repo.language = language.to_string();
        repo
    }

    fn run(stub: &Stub, cache: &Path) -> Result<Outcome, CtagsError> {
        list_symbols(stub, PINNED, &repo("go"), &PathBuf::from("."), cache)
    }

    #[test]
    fn symbols_are_filtered_and_sorted() {
        let cache = scratch_dir("ctags-sorted");
        let stub = Stub::new(Ok(BANNER.to_string()));
        let Outcome::Symbols(symbols) = run(&stub, &cache).unwrap() else {
            panic!("expected symbols");
        };
        let names: Vec<_> = symbols
            .iter()
            .map(|s| (s.path.as_str(), s.name.as_str()))
            .collect();
        assert_eq!(names, [("a.go", "alpha"), ("b.go", "zed")]);
        assert_eq!(symbols[0].kind, SymbolKind::Struct);
    }

    #[test]
    fn missing_tool_skips_with_a_reason() {
        let cache = scratch_dir("ctags-missing");
        let stub = Stub::new(Err(RunError::NotFound));
        let outcome = run(&stub, &cache).unwrap();
        assert_eq!(
            outcome,
            Outcome::Skipped {
                reason: "ctags is not installed".into()
            }
        );
    }

    #[test]
    fn other_version_and_other_tool_skip() {
        let cache = scratch_dir("ctags-version");
        let old = Stub::new(Ok("Universal Ctags 6.1.0(x)\n".into()));
        assert!(
            matches!(run(&old, &cache).unwrap(), Outcome::Skipped { reason } if reason.contains("6.1.0"))
        );
        let exuberant = Stub::new(Ok("Exuberant Ctags 5.8\n".into()));
        assert!(matches!(
            run(&exuberant, &cache).unwrap(),
            Outcome::Skipped { .. }
        ));
        assert_eq!(old.calls.borrow().len(), 1);
    }

    #[test]
    fn cache_hit_runs_no_tool_and_other_version_misses() {
        let cache = scratch_dir("ctags-cache");
        let first = Stub::new(Ok(BANNER.to_string()));
        run(&first, &cache).unwrap();
        let second = Stub::new(Err(RunError::NotFound));
        assert!(matches!(run(&second, &cache).unwrap(), Outcome::Symbols(_)));
        assert!(second.calls.borrow().is_empty());
        let newer = list_symbols(&second, "6.3.0", &repo("go"), &PathBuf::from("."), &cache);
        assert!(matches!(newer.unwrap(), Outcome::Skipped { .. }));
    }

    #[test]
    fn bad_rows_and_unknown_languages_fail() {
        assert!(matches!(parse("not json"), Err(CtagsError::Parse(_))));
        assert!(matches!(
            parse("{\"_type\":\"tag\",\"name\":\"a\"}"),
            Err(CtagsError::Parse(_))
        ));
        let cache = scratch_dir("ctags-lang");
        let stub = Stub::new(Ok(BANNER.to_string()));
        let res = list_symbols(&stub, PINNED, &repo("cobol"), &PathBuf::from("."), &cache);
        assert_eq!(res, Err(CtagsError::Language("cobol".into())));
    }
}
