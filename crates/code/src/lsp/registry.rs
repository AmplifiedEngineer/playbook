// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The language servers playbook knows about: what each handles, how it is
//! launched and what it needs on the machine.

use std::time::Duration;

/// Something a server needs on the machine besides its own binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prerequisite {
    Node,
    Cargo,
    /// The Rust standard library sources, needed by `rust-analyzer`.
    RustSrc,
    Go,
}

#[derive(Debug)]
pub struct ServerSpec {
    pub id: &'static str,
    pub license: &'static str,
    /// Names of the languages the server answers for, as `lang` names them.
    pub languages: &'static [&'static str],
    /// File extensions the server handles, without the dot.
    pub extensions: &'static [&'static str],
    /// The program looked up on `PATH`.
    pub executable: &'static str,
    pub args: &'static [&'static str],
    pub prerequisites: &'static [Prerequisite],
    pub request_timeout: Duration,
}

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

/// `rust-analyzer` indexes the whole workspace before it can answer.
const RUST_ANALYZER_TIMEOUT: Duration = Duration::from_secs(60);

pub const SERVERS: [ServerSpec; 4] = [
    ServerSpec {
        id: "typescript-language-server",
        license: "Apache-2.0",
        languages: &["typescript", "javascript"],
        extensions: &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"],
        executable: "typescript-language-server",
        args: &["--stdio"],
        prerequisites: &[Prerequisite::Node],
        request_timeout: DEFAULT_TIMEOUT,
    },
    ServerSpec {
        id: "rust-analyzer",
        license: "MIT OR Apache-2.0",
        languages: &["rust"],
        extensions: &["rs"],
        executable: "rust-analyzer",
        args: &[],
        prerequisites: &[Prerequisite::Cargo, Prerequisite::RustSrc],
        request_timeout: RUST_ANALYZER_TIMEOUT,
    },
    ServerSpec {
        id: "pyright",
        license: "MIT",
        languages: &["python"],
        extensions: &["py", "pyi"],
        executable: "pyright-langserver",
        args: &["--stdio"],
        prerequisites: &[Prerequisite::Node],
        request_timeout: DEFAULT_TIMEOUT,
    },
    ServerSpec {
        id: "gopls",
        license: "BSD-3-Clause",
        languages: &["go"],
        extensions: &["go"],
        executable: "gopls",
        args: &[],
        prerequisites: &[Prerequisite::Go],
        request_timeout: DEFAULT_TIMEOUT,
    },
];

pub fn by_id(id: &str) -> Option<&'static ServerSpec> {
    SERVERS.iter().find(|s| s.id == id)
}

/// The server that handles files with `extension` (no dot, any case).
pub fn for_extension(extension: &str) -> Option<&'static ServerSpec> {
    SERVERS.iter().find(|s| {
        s.extensions
            .iter()
            .any(|e| e.eq_ignore_ascii_case(extension))
    })
}

/// The LSP `languageId` for a file extension.
pub fn language_id(extension: &str) -> &'static str {
    match extension.to_ascii_lowercase().as_str() {
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "rs" => "rust",
        "py" | "pyi" => "python",
        "go" => "go",
        _ => "plaintext",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_extension_maps_to_exactly_one_server() {
        // Arrange
        let all: Vec<&str> = SERVERS.iter().flat_map(|s| s.extensions).copied().collect();

        for extension in &all {
            // Act
            let owners = SERVERS
                .iter()
                .filter(|s| s.extensions.contains(extension))
                .count();

            // Assert
            assert_eq!(owners, 1, ".{extension}");
        }
    }

    #[test]
    fn extension_and_id_lookups_find_the_server() {
        assert_eq!(
            for_extension("TSX").map(|s| s.id),
            Some("typescript-language-server")
        );
        assert_eq!(for_extension("rs").map(|s| s.id), Some("rust-analyzer"));
        assert_eq!(for_extension("txt").map(|s| s.id), None);
        assert_eq!(by_id("gopls").map(|s| s.executable), Some("gopls"));
        assert!(by_id("nosuch").is_none());
    }

    #[test]
    fn every_registered_extension_has_a_language_id() {
        for spec in &SERVERS {
            for extension in spec.extensions {
                assert_ne!(language_id(extension), "plaintext", ".{extension}");
            }
        }
    }

    #[test]
    fn only_rust_analyzer_gets_the_long_request_timeout() {
        for spec in &SERVERS {
            let long = spec.request_timeout > DEFAULT_TIMEOUT;
            assert_eq!(long, spec.id == "rust-analyzer", "{}", spec.id);
        }
    }
}
