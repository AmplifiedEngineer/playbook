// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Asking a language server about one position: `definition` and `hover`.
//! These functions never fall back themselves; they report why a server could
//! not answer and the command layer answers from the index.

use crate::lsp::client::{ClientError, Encoding};
use crate::lsp::session::{Launcher, Options, Session, Unavailable};
use crate::lsp::uri;
use crate::semantic::{lsp_position, Fallback, Target};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Hover text longer than this many bytes is cut.
pub const HOVER_LIMIT: usize = 8 * 1024;

/// What a query needs besides the target.
pub struct Ctx<'a> {
    pub root: &'a Path,
    pub options: &'a Options<'a>,
    pub launcher: &'a dyn Launcher,
}

pub enum Outcome<T> {
    Answered(Answered<T>),
    Fallback {
        why: Fallback,
        reason: String,
    },
    /// A signal arrived. The server is already stopped; the caller exits.
    Interrupted,
}

pub struct Answered<T> {
    pub value: T,
    pub server: &'static str,
    pub warnings: Vec<String>,
}

/// A place in a file, 1-based, with columns in Unicode scalar values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub path: String,
    pub line: usize,
    pub column: usize,
    pub end_line: usize,
    pub end_column: usize,
}

#[derive(Debug, Default)]
pub struct Locations {
    pub items: Vec<Location>,
    /// Results left out because they were not files inside the repo.
    pub dropped: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Hover {
    pub text: String,
    pub truncated: bool,
}

pub fn definition(ctx: &Ctx<'_>, target: &Target) -> Result<Outcome<Locations>, String> {
    ask(
        ctx,
        target,
        "definitionProvider",
        "textDocument/definition",
        |value, encoding, root| {
            let found = locations(value, root, encoding);
            let mut warnings = Vec::new();
            if found.dropped > 0 {
                warnings.push(format!(
                    "dropped {} location(s) that are not files inside the repo",
                    found.dropped
                ));
            }
            (found, warnings)
        },
    )
}

pub fn hover(ctx: &Ctx<'_>, target: &Target) -> Result<Outcome<Hover>, String> {
    ask(
        ctx,
        target,
        "hoverProvider",
        "textDocument/hover",
        |value, _, _| (hover_from(value), Vec::new()),
    )
}

/// The shared steps: validate the target, start a server, check the
/// capability, send one position request and parse the answer. Only an
/// argument error returns `Err`.
fn ask<T>(
    ctx: &Ctx<'_>,
    target: &Target,
    capability: &str,
    method: &str,
    parse: impl FnOnce(&Value, Encoding, &Path) -> (T, Vec<String>),
) -> Result<Outcome<T>, String> {
    let text = std::fs::read_to_string(&target.path)
        .map_err(|e| format!("could not read {}: {e}", target.relative))?;
    lsp_position(&text, target, Encoding::Utf16)?;
    let fallback = |why: Fallback, reason: String| Ok(Outcome::Fallback { why, reason });
    let mut session = match Session::for_file(ctx.root, &target.path, ctx.options, ctx.launcher) {
        Ok(session) => session,
        Err(Unavailable::Failed(ClientError::Interrupted)) => return Ok(Outcome::Interrupted),
        Err(why) => return fallback(Fallback::from_unavailable(&why), why.to_string()),
    };
    let opened = match session.open(&target.path) {
        Ok(opened) => opened,
        Err(why) => return fallback(Fallback::from_unavailable(&why), why.to_string()),
    };
    let server = session.spec().id;
    if !has_capability(session.client().capabilities(), capability) {
        return fallback(
            Fallback::CapabilityMissing,
            format!("{server} does not offer {capability}"),
        );
    }
    let encoding = session.client().encoding();
    let position = lsp_position(&opened.text, target, encoding)?;
    let params = json!({
        "textDocument": {"uri": opened.uri},
        "position": {"line": position.line, "character": position.character},
    });
    match session.client().request(method, params) {
        Ok(value) => {
            let (value, warnings) = parse(&value, encoding, ctx.root);
            session.finish();
            Ok(Outcome::Answered(Answered {
                value,
                server,
                warnings,
            }))
        }
        Err(ClientError::Interrupted) => Ok(Outcome::Interrupted),
        Err(e) => fallback(Fallback::from_client_error(&e), e.to_string()),
    }
}

fn has_capability(capabilities: &Value, name: &str) -> bool {
    !matches!(
        capabilities.get(name),
        None | Some(Value::Null | Value::Bool(false))
    )
}

/// Reads a `Location`, a `LocationLink` or an array of either, keeping the
/// ones that are files inside `root`.
fn locations(value: &Value, root: &Path, encoding: Encoding) -> Locations {
    let rows: Vec<&Value> = match value {
        Value::Null => Vec::new(),
        Value::Array(rows) => rows.iter().collect(),
        row => vec![row],
    };
    let mut found = Locations::default();
    let mut texts: HashMap<PathBuf, Option<String>> = HashMap::new();
    for row in rows {
        let (uri_text, range) = match row.get("targetUri") {
            Some(target) => (target, row.get("targetSelectionRange")),
            None => (&row["uri"], row.get("range")),
        };
        let inside = uri_text
            .as_str()
            .and_then(uri::to_path)
            .and_then(|path| path.canonicalize().ok())
            .filter(|path| path.is_file() && path.starts_with(root));
        let (Some(path), Some(range)) = (inside, range) else {
            found.dropped += 1;
            continue;
        };
        let text = texts
            .entry(path.clone())
            .or_insert_with(|| std::fs::read_to_string(&path).ok())
            .as_deref()
            .unwrap_or("");
        let (line, column) = to_line_column(text, &range["start"], encoding);
        let (end_line, end_column) = to_line_column(text, &range["end"], encoding);
        found.items.push(Location {
            path: relative(&path, root),
            line,
            column,
            end_line,
            end_column,
        });
    }
    found
}

fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// A server position as a 1-based line and a 1-based column in scalar values.
fn to_line_column(text: &str, position: &Value, encoding: Encoding) -> (usize, usize) {
    let line = position["line"].as_u64().unwrap_or(0) as usize;
    let wanted = position["character"].as_u64().unwrap_or(0) as usize;
    let line_text = text.lines().nth(line).unwrap_or("");
    let mut units = 0;
    let mut scalars = 0;
    for c in line_text.chars() {
        if units >= wanted {
            break;
        }
        units += match encoding {
            Encoding::Utf8 => c.len_utf8(),
            Encoding::Utf16 => c.len_utf16(),
        };
        scalars += 1;
    }
    (line + 1, scalars + 1)
}

/// The text of a hover answer, cut to [`HOVER_LIMIT`] bytes.
fn hover_from(value: &Value) -> Hover {
    let mut text = markup_text(&value["contents"]);
    let truncated = text.len() > HOVER_LIMIT;
    if truncated {
        let mut end = HOVER_LIMIT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    Hover { text, truncated }
}

/// `MarkupContent`, a `MarkedString` or an array of them, as plain text.
fn markup_text(contents: &Value) -> String {
    match contents {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(markup_text)
            .collect::<Vec<_>>()
            .join("\n\n"),
        Value::Object(_) => contents["value"].as_str().unwrap_or("").to_string(),
        _ => String::new(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::lsp::fake::{Fake, Step};
    use crate::lsp::testkit::{initialize_script, options, setup, trusted, FakeLauncher, Setup};
    use crate::lsp::transport::pipe_pair;
    use crate::semantic::parse_target;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    fn target(setup: &Setup, line: usize, column: usize) -> Target {
        parse_target(&format!("a.ts:{line}:{column}"), &setup.root, &setup.root).unwrap()
    }

    fn clean_exit() -> Vec<Step> {
        vec![
            Step::Reply {
                method: "shutdown",
                result: Value::Null,
            },
            Step::Expect("exit"),
        ]
    }

    /// Script: initialize with `capabilities`, read `didOpen`, then `rest`.
    fn script(capabilities: Value, rest: Vec<Step>) -> Vec<Step> {
        let mut steps = initialize_script(capabilities);
        steps.push(Step::Expect("textDocument/didOpen"));
        steps.extend(rest);
        steps
    }

    fn file_uri(path: &Path) -> String {
        uri::from_path(path)
    }

    #[test]
    fn an_inside_location_is_returned_and_an_outside_one_is_dropped_and_counted() {
        // Arrange
        let s = setup("q-def");
        let outside = std::env::temp_dir().join("elsewhere.ts");
        let answer = json!([
            {"uri": file_uri(&s.file), "range": {"start": {"line": 0, "character": 13}, "end": {"line": 0, "character": 14}}},
            {"uri": file_uri(&outside), "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}},
        ]);
        let (client_side, server) = pipe_pair().unwrap();
        let mut steps = script(
            json!({"definitionProvider": true}),
            vec![Step::Reply {
                method: "textDocument/definition",
                result: answer,
            }],
        );
        steps.extend(clean_exit());
        let fake = Fake::spawn(server, steps);
        let launcher = FakeLauncher::new(Some(client_side));
        let settings = trusted();
        let dirs = [s.bin.clone()];
        let opts = options(&s, &settings, &dirs);
        let ctx = Ctx {
            root: &s.root,
            options: &opts,
            launcher: &launcher,
        };

        // Act
        let got = definition(&ctx, &target(&s, 1, 14)).unwrap();
        fake.finish();

        // Assert
        let Outcome::Answered(answered) = got else {
            panic!("expected an answer");
        };
        assert_eq!(answered.server, "typescript-language-server");
        assert_eq!(answered.value.dropped, 1);
        assert_eq!(
            answered.value.items,
            [Location {
                path: "a.ts".into(),
                line: 1,
                column: 14,
                end_line: 1,
                end_column: 15,
            }]
        );
        assert!(
            answered.warnings[0].contains("dropped"),
            "{:?}",
            answered.warnings
        );
    }

    #[test]
    fn untitled_https_and_escaping_file_locations_are_all_dropped() {
        // Arrange
        let s = setup("q-drop");
        let range =
            json!({"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}});
        let rows = json!([
            {"uri": "untitled:Untitled-1", "range": range},
            {"uri": "https://example.com/a.ts", "range": range},
            {"uri": format!("file://{}/../outside.ts", s.root.display()), "range": range},
        ]);

        // Act
        let got = locations(&rows, &s.root, Encoding::Utf16);

        // Assert
        assert!(got.items.is_empty());
        assert_eq!(got.dropped, 3);
    }

    #[test]
    fn a_server_without_the_capability_falls_back_with_capability_missing() {
        // Arrange
        let s = setup("q-nocap");
        let (client_side, server) = pipe_pair().unwrap();
        let fake = Fake::spawn(server, script(json!({}), vec![]));
        let launcher = FakeLauncher::new(Some(client_side));
        let settings = trusted();
        let dirs = [s.bin.clone()];
        let opts = options(&s, &settings, &dirs);
        let ctx = Ctx {
            root: &s.root,
            options: &opts,
            launcher: &launcher,
        };

        // Act
        let got = definition(&ctx, &target(&s, 1, 1)).unwrap();
        fake.finish();

        // Assert
        assert!(matches!(
            got,
            Outcome::Fallback {
                why: Fallback::CapabilityMissing,
                ..
            }
        ));
    }

    /// Runs `definition` against a scripted server with a 100 ms request timeout.
    fn definition_fallback(
        tag: &str,
        steps: Vec<Step>,
        installed: bool,
    ) -> (Fallback, String, Fake) {
        let s = setup(&format!("q-fb-{tag}"));
        let (client_side, server) = pipe_pair().unwrap();
        let fake = Fake::spawn(server, steps);
        let launcher = FakeLauncher::new(Some(client_side));
        let settings = trusted();
        let dirs = if installed {
            vec![s.bin.clone()]
        } else {
            vec![]
        };
        let mut opts = options(&s, &settings, &dirs);
        opts.timeout = Some(Duration::from_millis(100));
        let ctx = Ctx {
            root: &s.root,
            options: &opts,
            launcher: &launcher,
        };
        match definition(&ctx, &target(&s, 1, 1)).unwrap() {
            Outcome::Fallback { why, reason } => (why, reason, fake),
            _ => panic!("{tag}: expected a fallback"),
        }
    }

    #[test]
    fn a_server_that_closes_mid_request_falls_back_with_server_crashed() {
        // Arrange
        let steps = script(
            json!({"definitionProvider": true}),
            vec![Step::Expect("textDocument/definition"), Step::Close],
        );

        // Act
        let (why, reason, fake) = definition_fallback("crash", steps, true);
        fake.finish();

        // Assert
        assert_eq!(why, Fallback::ServerCrashed);
        assert!(!reason.is_empty());
    }

    #[test]
    fn a_reply_held_past_the_timeout_falls_back_with_timeout() {
        // Arrange
        let (release, held) = std::sync::mpsc::channel();
        let steps = script(
            json!({"definitionProvider": true}),
            vec![Step::Hold {
                method: "textDocument/definition",
                result: Value::Null,
                release: held,
            }],
        );

        // Act
        let (why, _, fake) = definition_fallback("timeout", steps, true);
        release.send(()).unwrap();
        fake.finish();

        // Assert
        assert_eq!(why, Fallback::Timeout);
    }

    #[test]
    fn no_installed_server_falls_back_with_server_missing() {
        // Act
        let (why, reason, fake) = definition_fallback("missing", vec![], false);
        fake.finish();

        // Assert
        assert_eq!(why, Fallback::ServerMissing);
        assert!(
            reason.contains("code.lsp.typescript-language-server.path"),
            "{reason}"
        );
    }

    #[test]
    fn a_column_past_the_end_is_an_argument_error_and_no_server_starts() {
        // Arrange
        let s = setup("q-arg");
        let launcher = FakeLauncher::new(None);
        let settings = trusted();
        let dirs = [s.bin.clone()];
        let opts = options(&s, &settings, &dirs);
        let ctx = Ctx {
            root: &s.root,
            options: &opts,
            launcher: &launcher,
        };

        // Act
        let got = hover(&ctx, &target(&s, 1, 99));

        // Assert
        assert!(got.err().unwrap().contains("column"));
        assert_eq!(launcher.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn hover_text_is_cut_only_above_the_limit() {
        for (len, cut) in [
            (HOVER_LIMIT - 1, false),
            (HOVER_LIMIT, false),
            (HOVER_LIMIT + 1, true),
        ] {
            // Arrange
            let answer = json!({"contents": {"kind": "markdown", "value": "x".repeat(len)}});

            // Act
            let got = hover_from(&answer);

            // Assert
            assert_eq!(got.truncated, cut, "{len}");
            assert_eq!(got.text.len(), len.min(HOVER_LIMIT), "{len}");
        }
    }

    #[test]
    fn hover_contents_in_every_shape_become_plain_text() {
        assert_eq!(hover_from(&json!({"contents": "plain"})).text, "plain");
        assert_eq!(
            hover_from(&json!({"contents": ["a", {"value": "b"}]})).text,
            "a\n\nb"
        );
        assert_eq!(hover_from(&Value::Null).text, "");
    }

    #[test]
    fn a_cut_never_splits_a_character() {
        let answer = json!({"contents": format!("{}世", "x".repeat(HOVER_LIMIT - 1))});
        let got = hover_from(&answer);
        assert!(got.truncated);
        assert_eq!(got.text.len(), HOVER_LIMIT - 1);
    }

    #[test]
    fn location_links_and_utf8_columns_are_converted_to_scalar_columns() {
        // Arrange
        let s = setup("q-link");
        std::fs::write(&s.file, "😀世x\n").unwrap();
        let rows = json!({
            "targetUri": file_uri(&s.file),
            "targetSelectionRange": {"start": {"line": 0, "character": 7}, "end": {"line": 0, "character": 8}},
        });

        // Act
        let got = locations(&rows, &s.root, Encoding::Utf8);

        // Assert
        assert_eq!((got.items[0].line, got.items[0].column), (1, 3));
        assert_eq!(got.items[0].end_column, 4);
    }
}
