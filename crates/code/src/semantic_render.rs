// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `definition` and `hover` as commands: ask a language server, and when it
//! cannot answer, answer from the index and say why. Every path prints a
//! usable answer and exits 0.

use crate::host;
use crate::lsp::discover;
use crate::lsp::session::{Options, ProcessLauncher};
use crate::output::{Envelope, IndexState, LspInfo};
use crate::queries::{self, Answered, Ctx, Hover, Locations, Outcome};
use crate::search::SearchOptions;
use crate::semantic::{identifier_at, parse_target, Fallback, Target};
use crate::{location, status};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// How many index symbols a fallback shows.
const FALLBACK_LIMIT: usize = 5;

const SIGINT_EXIT: i32 = 130;
const SIGTERM_EXIT: i32 = 143;

pub struct Request<'a> {
    pub env: &'a crate::cli::Env,
    pub spec: &'a str,
    pub timeout_ms: Option<u64>,
    pub json: bool,
}

pub fn definition(request: &Request<'_>) -> Result<String, String> {
    let (root, target) = resolve_target(request)?;
    let outcome = with_production_ctx(request, &root, |ctx| queries::definition(ctx, &target))?;
    let (rows, info, warnings, index) = match outcome {
        Outcome::Answered(Answered {
            value,
            server,
            warnings,
        }) => (
            Rows::Locations(value),
            ok_info(server),
            warnings,
            Index::default(),
        ),
        Outcome::Fallback { why, reason } => {
            let (rows, warnings, index) = symbol_rows(request, &target);
            (
                Rows::Symbols(rows),
                fallback_info(why, &reason),
                warnings,
                index.with(reason),
            )
        }
        Outcome::Interrupted => return Err("interrupted".to_string()),
    };
    Ok(render_definition(request, rows, info, warnings, index))
}

pub fn hover(request: &Request<'_>) -> Result<String, String> {
    let (root, target) = resolve_target(request)?;
    let outcome = with_production_ctx(request, &root, |ctx| queries::hover(ctx, &target))?;
    let (hover, info, warnings, index) = match outcome {
        Outcome::Answered(Answered {
            value,
            server,
            warnings,
        }) => (value, ok_info(server), warnings, Index::default()),
        Outcome::Fallback { why, reason } => {
            let (rows, warnings, index) = symbol_rows(request, &target);
            let text = rows.first().map(SymbolRow::summary).unwrap_or_default();
            (
                Hover {
                    text,
                    truncated: false,
                },
                fallback_info(why, &reason),
                warnings,
                index.with(reason),
            )
        }
        Outcome::Interrupted => return Err("interrupted".to_string()),
    };
    Ok(render_hover(request, &hover, info, warnings, index))
}

fn resolve_target(request: &Request<'_>) -> Result<(PathBuf, Target), String> {
    let root = location::repo_root(&request.env.cwd)?;
    let target = parse_target(request.spec, &request.env.cwd, &root)?;
    Ok((root, target))
}

/// Builds the real session options and runs `query`. A signal stops the
/// server, then the process exits with the conventional status.
fn with_production_ctx<T>(
    request: &Request<'_>,
    root: &Path,
    query: impl FnOnce(&Ctx<'_>) -> Result<Outcome<T>, String>,
) -> Result<Outcome<T>, String> {
    let Ok(lock_dir) = host::state_dir() else {
        return Ok(Outcome::Fallback {
            why: Fallback::Error,
            reason: "could not resolve where to keep the server lock for this repo".to_string(),
        });
    };
    let signals = Signals::install();
    let path_dirs = discover::process_path();
    let options = Options {
        settings: &request.env.lsp,
        path_dirs: &path_dirs,
        lock_dir: &lock_dir,
        probe_timeout: discover::PROBE_TIMEOUT,
        timeout: request.timeout_ms.map(Duration::from_millis),
        interrupt: Some(Arc::clone(&signals.any)),
    };
    let ctx = Ctx {
        root,
        options: &options,
        launcher: &ProcessLauncher,
    };
    let outcome = query(&ctx)?;
    if matches!(outcome, Outcome::Interrupted) || signals.any.load(Ordering::SeqCst) {
        std::process::exit(signals.exit_code());
    }
    Ok(outcome)
}

/// Flags set by SIGINT and SIGTERM, so a wait can end and the server be
/// stopped before the process exits.
struct Signals {
    any: Arc<AtomicBool>,
    term: Arc<AtomicBool>,
}

impl Signals {
    fn install() -> Self {
        let signals = Self {
            any: Arc::new(AtomicBool::new(false)),
            term: Arc::new(AtomicBool::new(false)),
        };
        #[cfg(unix)]
        {
            use signal_hook::consts::{SIGINT, SIGTERM};
            let _ = signal_hook::flag::register(SIGINT, Arc::clone(&signals.any));
            let _ = signal_hook::flag::register(SIGTERM, Arc::clone(&signals.any));
            let _ = signal_hook::flag::register(SIGTERM, Arc::clone(&signals.term));
        }
        signals
    }

    fn exit_code(&self) -> i32 {
        if self.term.load(Ordering::SeqCst) {
            SIGTERM_EXIT
        } else {
            SIGINT_EXIT
        }
    }
}

fn ok_info(server: &str) -> LspInfo {
    LspInfo {
        state: "ok",
        server: Some(server.to_string()),
        source: "lsp",
        reason: None,
    }
}

fn fallback_info(why: Fallback, reason: &str) -> LspInfo {
    LspInfo {
        state: why.state(),
        server: None,
        source: "tree_sitter_fallback",
        reason: Some(reason.to_string()),
    }
}

/// What the index contributed to an answer.
#[derive(Default)]
struct Index {
    state: Option<IndexState>,
    path: String,
    /// Why a server did not answer, for the stderr warning.
    reason: Option<String>,
}

impl Index {
    fn with(mut self, reason: String) -> Self {
        self.reason = Some(reason);
        self
    }
}

struct SymbolRow {
    path: String,
    line: usize,
    kind: String,
    name: String,
    signature: Option<String>,
}

impl SymbolRow {
    fn summary(&self) -> String {
        format!(
            "{} {} {}",
            self.kind,
            self.name,
            self.signature.as_deref().unwrap_or("")
        )
        .trim()
        .to_string()
    }
}

/// The index symbols whose name is the identifier under the cursor.
fn symbol_rows(request: &Request<'_>, target: &Target) -> (Vec<SymbolRow>, Vec<String>, Index) {
    let mut warnings = Vec::new();
    let name = std::fs::read_to_string(&target.path)
        .ok()
        .and_then(|text| identifier_at(&text, target.line, target.column));
    let Some(name) = name else {
        warnings.push("no identifier at that position".to_string());
        return (Vec::new(), warnings, Index::default());
    };
    let mut opts = SearchOptions::new(&name);
    opts.limit = FALLBACK_LIMIT;
    match crate::symbol_lookup(&request.env.cwd, &opts) {
        Ok(out) => {
            let state = status::state_of(&out.drift);
            if state == IndexState::Stale {
                warnings.push(status::stale_warning(&out.drift));
            }
            let rows = out
                .result
                .hits
                .iter()
                .filter_map(|hit| {
                    let symbol = hit.symbol.as_ref()?;
                    Some(SymbolRow {
                        path: hit.path.clone(),
                        line: hit.line,
                        kind: symbol.kind.clone(),
                        name: symbol.qualified_name.clone(),
                        signature: symbol.signature.clone(),
                    })
                })
                .collect();
            let index = Index {
                state: Some(state),
                path: out.index_path.display().to_string(),
                reason: None,
            };
            (rows, warnings, index)
        }
        Err(why) => {
            warnings.push(why);
            (Vec::new(), warnings, Index::default())
        }
    }
}

enum Rows {
    Locations(Locations),
    Symbols(Vec<SymbolRow>),
}

fn render_definition(
    request: &Request<'_>,
    rows: Rows,
    info: LspInfo,
    mut warnings: Vec<String>,
    index: Index,
) -> String {
    if let Some(reason) = &index.reason {
        warnings.insert(0, reason.clone());
    }
    let (results, lines): (Vec<Value>, Vec<String>) = match &rows {
        Rows::Locations(found) => found
            .items
            .iter()
            .map(|l| {
                (
                    json!({
                        "path": l.path,
                        "line": l.line,
                        "column": l.column,
                        "end_line": l.end_line,
                        "end_column": l.end_column,
                    }),
                    format!("{}:{}:{}", l.path, l.line, l.column),
                )
            })
            .unzip(),
        Rows::Symbols(symbols) => symbols
            .iter()
            .map(|s| {
                (
                    json!({
                        "path": s.path,
                        "line": s.line,
                        "kind": s.kind,
                        "name": s.name,
                        "signature": s.signature,
                    }),
                    format!("{}:{}  {}", s.path, s.line, s.summary()),
                )
            })
            .unzip(),
    };
    if request.json {
        let mut envelope = envelope("definition", Value::Array(results), info, warnings, &index);
        if let Rows::Locations(found) = &rows {
            envelope.stats = Some(json!({"dropped": found.dropped}));
        }
        return envelope.to_json();
    }
    print_warnings(&warnings);
    if lines.is_empty() {
        return "no definition found\n".to_string();
    }
    lines.join("\n") + "\n"
}

fn render_hover(
    request: &Request<'_>,
    hover: &Hover,
    info: LspInfo,
    mut warnings: Vec<String>,
    index: Index,
) -> String {
    if let Some(reason) = &index.reason {
        warnings.insert(0, reason.clone());
    }
    if request.json {
        let mut envelope = envelope("hover", json!({"text": hover.text}), info, warnings, &index);
        envelope.truncated = hover.truncated;
        return envelope.to_json();
    }
    print_warnings(&warnings);
    if hover.text.is_empty() {
        return "no hover information\n".to_string();
    }
    let note = if hover.truncated { "\n(cut)" } else { "" };
    format!("{}{note}\n", hover.text)
}

fn envelope(
    command: &'static str,
    results: Value,
    info: LspInfo,
    warnings: Vec<String>,
    index: &Index,
) -> Envelope {
    let mut envelope = Envelope::new(
        command,
        index.state.unwrap_or(IndexState::NotUsed),
        index.path.clone(),
        results,
    );
    envelope.warnings = warnings;
    envelope.lsp = Some(info);
    envelope
}

fn print_warnings(warnings: &[String]) {
    for warning in warnings {
        eprintln!("code: {warning}");
    }
}
