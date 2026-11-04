// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The pieces the semantic commands (`definition`, `hover`, resolved `refs`,
//! `diagnostics`) share: reading a `path:line:column` target, converting its
//! column to the unit a server negotiated, and saying why a call fell back to
//! the index.

use crate::lsp::client::{ClientError, Encoding};
use crate::lsp::session::Unavailable;
use std::path::{Path, PathBuf};

/// A position the user named: a file inside the repo, a 1-based line and a
/// 1-based column counted in Unicode scalar values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The canonical absolute path.
    pub path: PathBuf,
    /// The path relative to the repo root, with `/` separators.
    pub relative: String,
    pub line: usize,
    pub column: usize,
}

/// Reads `<path>:<line>:<column>`. A relative path is taken from `cwd`. The
/// file must exist and, after links are resolved, sit inside `root`.
pub fn parse_target(spec: &str, cwd: &Path, root: &Path) -> Result<Target, String> {
    let shape = || format!("expected <path>:<line>:<column>, got {spec:?}");
    let mut parts = spec.rsplitn(3, ':');
    let column = parts.next().ok_or_else(shape)?;
    let line = parts.next().ok_or_else(shape)?;
    let path = parts.next().ok_or_else(shape)?;
    let column: usize = column.parse().map_err(|_| shape())?;
    let line: usize = line.parse().map_err(|_| shape())?;
    if line == 0 {
        return Err("line must be 1 or more".to_string());
    }
    if column == 0 {
        return Err("column must be 1 or more".to_string());
    }
    let joined = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        cwd.join(path)
    };
    let canonical = joined
        .canonicalize()
        .map_err(|_| format!("no such file: {path}"))?;
    let root = root
        .canonicalize()
        .map_err(|e| format!("cannot resolve the repo root: {e}"))?;
    let relative = canonical
        .strip_prefix(&root)
        .map_err(|_| format!("{path} is outside repo"))?;
    let relative = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    Ok(Target {
        path: canonical,
        relative,
        line,
        column,
    })
}

/// A zero-based LSP position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// The server position for `target` in `text`, with the column converted to
/// `encoding`. A column one past the last character of the line is the end of
/// the line and is allowed.
pub fn lsp_position(text: &str, target: &Target, encoding: Encoding) -> Result<Position, String> {
    let line_text = text.lines().nth(target.line - 1).ok_or_else(|| {
        format!(
            "line {} is past the end of {}",
            target.line, target.relative
        )
    })?;
    let before = target.column - 1;
    let total = line_text.chars().count();
    if before > total {
        return Err(format!(
            "column {} is past the end of line {} ({total} characters)",
            target.column, target.line
        ));
    }
    let character: usize = line_text
        .chars()
        .take(before)
        .map(|c| match encoding {
            Encoding::Utf8 => c.len_utf8(),
            Encoding::Utf16 => c.len_utf16(),
        })
        .sum();
    Ok(Position {
        line: u32::try_from(target.line - 1).map_err(|_| "line number is too large".to_string())?,
        character: u32::try_from(character).map_err(|_| "column is too large".to_string())?,
    })
}

/// The identifier under the cursor, or the one that ends just before it.
pub fn identifier_at(text: &str, line: usize, column: usize) -> Option<String> {
    let chars: Vec<char> = text.lines().nth(line.checked_sub(1)?)?.chars().collect();
    let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut at = column.checked_sub(1)?;
    if !chars.get(at).copied().is_some_and(is_word) {
        at = at.checked_sub(1)?;
        if !chars.get(at).copied().is_some_and(is_word) {
            return None;
        }
    }
    let start = (0..=at).rev().take_while(|&i| is_word(chars[i])).last()?;
    let end = (at..chars.len())
        .take_while(|&i| is_word(chars[i]))
        .last()?
        + 1;
    Some(chars[start..end].iter().collect())
}

/// Why a semantic command answered from the index instead of a server. Each
/// value is a `lsp.state` the JSON output reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    Untrusted,
    Busy,
    ServerMissing,
    CapabilityMissing,
    Timeout,
    ServerCrashed,
    /// The server returned an error or a result that did not parse.
    Error,
}

impl Fallback {
    pub fn state(self) -> &'static str {
        match self {
            Self::Untrusted => "untrusted",
            Self::Busy => "busy",
            Self::ServerMissing => "server_missing",
            Self::CapabilityMissing => "capability_missing",
            Self::Timeout => "timeout",
            Self::ServerCrashed => "server_crashed",
            Self::Error => "error",
        }
    }

    pub fn from_unavailable(why: &Unavailable) -> Self {
        match why {
            Unavailable::Untrusted => Self::Untrusted,
            Unavailable::Busy => Self::Busy,
            Unavailable::NoServerFor(_)
            | Unavailable::NotInstalled(_)
            | Unavailable::BadConfiguredPath(_) => Self::ServerMissing,
            Unavailable::Unreadable(_) => Self::Error,
            Unavailable::Failed(e) => Self::from_client_error(e),
        }
    }

    pub fn from_client_error(error: &ClientError) -> Self {
        match error {
            ClientError::Timeout { .. } | ClientError::WriteTimeout => Self::Timeout,
            ClientError::Crashed { .. } => Self::ServerCrashed,
            ClientError::Spawn(_) => Self::ServerMissing,
            ClientError::Write(_)
            | ClientError::Server { .. }
            | ClientError::Retryable { .. }
            | ClientError::ResponseLimit { .. }
            | ClientError::Protocol(_)
            | ClientError::Interrupted => Self::Error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::discover::BadPath;

    fn scratch(tag: &str) -> PathBuf {
        let dir = playbook_common::test_support::scratch_dir(tag);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn target(line: usize, column: usize) -> Target {
        Target {
            path: PathBuf::from("/r/a.ts"),
            relative: "a.ts".into(),
            line,
            column,
        }
    }

    #[test]
    fn a_column_after_an_emoji_and_a_cjk_character_converts_to_each_encoding() {
        // Arrange: two characters before the column, 4 and 3 bytes, 2 and 1 UTF-16 units.
        let text = "😀世x\n";

        // Act
        let utf16 = lsp_position(text, &target(1, 3), Encoding::Utf16).unwrap();
        let utf8 = lsp_position(text, &target(1, 3), Encoding::Utf8).unwrap();

        // Assert
        assert_eq!((utf16.line, utf16.character), (0, 3));
        assert_eq!((utf8.line, utf8.character), (0, 7));
    }

    #[test]
    fn the_end_of_the_line_is_allowed_and_one_past_it_is_a_column_error() {
        // Arrange
        let text = "abc\ndef\n";

        // Act
        let end = lsp_position(text, &target(2, 4), Encoding::Utf16).unwrap();
        let past = lsp_position(text, &target(2, 5), Encoding::Utf16).unwrap_err();
        let no_line = lsp_position(text, &target(3, 1), Encoding::Utf16).unwrap_err();

        // Assert
        assert_eq!((end.line, end.character), (1, 3));
        assert!(past.contains("column"), "{past}");
        assert!(no_line.contains("line 3"), "{no_line}");
    }

    #[test]
    fn column_zero_and_a_malformed_target_are_argument_errors() {
        // Arrange
        let root = scratch("sem-args");

        // Act
        let zero = parse_target("a.ts:1:0", &root, &root).unwrap_err();
        let no_line = parse_target("a.ts:0:1", &root, &root).unwrap_err();
        let shape = parse_target("a.ts", &root, &root).unwrap_err();
        let words = parse_target("a.ts:x:y", &root, &root).unwrap_err();

        // Assert
        assert!(zero.contains("column"), "{zero}");
        assert!(no_line.contains("line"), "{no_line}");
        assert!(shape.contains("<path>:<line>:<column>"), "{shape}");
        assert!(words.contains("<path>:<line>:<column>"), "{words}");
    }

    #[test]
    fn a_path_inside_the_repo_resolves_relative_to_the_working_directory() {
        // Arrange
        let root = scratch("sem-inside");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.ts"), "x\n").unwrap();

        // Act
        let from_root = parse_target("src/a.ts:1:1", &root, &root).unwrap();
        let from_src = parse_target("a.ts:1:1", &root.join("src"), &root).unwrap();

        // Assert
        assert_eq!(from_root.relative, "src/a.ts");
        assert_eq!(from_src, from_root);
    }

    #[test]
    fn a_parent_path_and_an_escaping_symlink_are_outside_the_repo() {
        // Arrange
        let base = scratch("sem-outside");
        let root = base.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(base.join("secret.ts"), "x\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(base.join("secret.ts"), root.join("link.ts")).unwrap();

        // Act
        let parent = parse_target("../secret.ts:1:1", &root, &root).unwrap_err();
        #[cfg(unix)]
        let link = parse_target("link.ts:1:1", &root, &root).unwrap_err();

        // Assert
        assert!(parent.contains("outside repo"), "{parent}");
        #[cfg(unix)]
        assert!(link.contains("outside repo"), "{link}");
    }

    #[test]
    fn a_file_that_does_not_exist_is_reported_as_missing() {
        let root = scratch("sem-missing");
        let err = parse_target("nope.ts:1:1", &root, &root).unwrap_err();
        assert!(err.contains("no such file"), "{err}");
    }

    #[test]
    fn identifier_at_finds_the_word_under_or_just_before_the_cursor() {
        let text = "const fooBar = $baz_1(x);\n";
        assert_eq!(identifier_at(text, 1, 8).as_deref(), Some("fooBar"));
        assert_eq!(identifier_at(text, 1, 13).as_deref(), Some("fooBar"));
        assert_eq!(identifier_at(text, 1, 16).as_deref(), Some("$baz_1"));
        assert_eq!(identifier_at(text, 1, 14), None);
        assert_eq!(identifier_at(text, 2, 1), None);
        assert_eq!(identifier_at(text, 1, 0), None);
    }

    #[test]
    fn each_reason_maps_to_its_state() {
        let cases = [
            (
                Fallback::from_unavailable(&Unavailable::Untrusted),
                "untrusted",
            ),
            (Fallback::from_unavailable(&Unavailable::Busy), "busy"),
            (
                Fallback::from_unavailable(&Unavailable::NotInstalled("gopls")),
                "server_missing",
            ),
            (
                Fallback::from_unavailable(&Unavailable::NoServerFor("txt".into())),
                "server_missing",
            ),
            (
                Fallback::from_unavailable(&Unavailable::BadConfiguredPath(BadPath::NotAbsolute(
                    "x".into(),
                ))),
                "server_missing",
            ),
            (
                Fallback::from_client_error(&ClientError::Timeout { method: "m".into() }),
                "timeout",
            ),
            (
                Fallback::from_client_error(&ClientError::Crashed { detail: "x".into() }),
                "server_crashed",
            ),
            (
                Fallback::from_client_error(&ClientError::Server {
                    code: -1,
                    message: "m".into(),
                }),
                "error",
            ),
            (Fallback::CapabilityMissing, "capability_missing"),
        ];
        for (fallback, state) in cases {
            assert_eq!(fallback.state(), state);
        }
    }
}
