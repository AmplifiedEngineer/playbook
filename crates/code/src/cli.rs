// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The thin command line layer: argument shapes, and rendering of the results
//! the library functions in `code` return.

use super::host::{self, LspSettings};
use super::lsp;
use super::output::{Envelope, IndexState, LspInfo};
use super::search::SearchOptions;
use super::semantic;
use super::semantic_render;
use super::status;
use clap::Subcommand;
use serde_json::json;
use std::path::PathBuf;

/// `playbook code` subcommands.
#[derive(Subcommand, Debug)]
pub enum CodeCommand {
    /// Build or refresh the code index for this repo
    ///
    /// Hashes every file git lists, and reads only the ones that changed.
    /// Only one run at a time is allowed.
    ///
    /// Example: `playbook code index --json`
    Index {
        /// Print one JSON document
        #[arg(long)]
        json: bool,
        /// Delete the cache first and index everything again
        #[arg(long)]
        rebuild: bool,
    },
    /// Report whether the index matches the files on disk
    ///
    /// Hashes every file git lists, so it is slower than the quick check that
    /// search commands run first.
    ///
    /// Example: `playbook code status --json`
    Status {
        /// Print one JSON document
        #[arg(long)]
        json: bool,
    },
    /// Search symbols, imports and text of this repo
    ///
    /// Without the text flag, results are ranked in tiers: exact qualified
    /// name, exact name, name ignoring case, name prefix, import, text, then a
    /// plain substring scan. With it, only indexed text is searched. Words in
    /// the query are matched as plain text, so operators have no special
    /// meaning. Results are marked stale when a file changed after indexing.
    ///
    /// Example: `playbook code search retry --lang python`
    Search {
        /// Words to look for
        query: String,
        /// Search indexed text only
        #[arg(long)]
        text: bool,
        /// Only files under this path prefix, for example `lib/payment/`
        #[arg(long)]
        path: Option<String>,
        /// Only files of this language: rust, typescript, javascript or python
        #[arg(long, value_parser = ["rust", "typescript", "javascript", "python"])]
        lang: Option<String>,
        /// Most results to show, from 1 to 200
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=200))]
        limit: u32,
        /// Print one JSON document
        #[arg(long)]
        json: bool,
    },
    /// Find where a name is declared
    ///
    /// Matches the exact qualified name first (a top level symbol's qualified
    /// name is its plain name), then the name, then the name ignoring case,
    /// then the name as a prefix.
    ///
    /// Example: `playbook code symbol PaymentService --kind class`
    Symbol {
        /// Name to look up
        name: String,
        /// Only this kind, for example `function`, `class` or `method`
        #[arg(long)]
        kind: Option<String>,
        /// Only files of this language: rust, typescript, javascript or python
        #[arg(long, value_parser = ["rust", "typescript", "javascript", "python"])]
        lang: Option<String>,
        /// Only files under this path prefix, for example `lib/payment/`
        #[arg(long)]
        path: Option<String>,
        /// Most results to show, from 1 to 200
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=200))]
        limit: u32,
        /// Print one JSON document
        #[arg(long)]
        json: bool,
    },
    /// List the declarations in one file
    ///
    /// Parses the file as it is on disk, so it works without an index and never
    /// shows old text. Prints at most 500 declarations, indented by nesting.
    ///
    /// Example: `playbook code outline lib/payment/service.ts`
    Outline {
        /// File to outline
        path: String,
        /// Print one JSON document
        #[arg(long)]
        json: bool,
    },
    /// Print numbered lines of one file
    ///
    /// Reads the file as it is on disk. Prints at most 400 lines or 32 KiB. A
    /// range that ends past the file is cut at the end with a warning.
    ///
    /// Example: `playbook code read lib/payment/service.ts 10:40`
    Read {
        /// File to read
        path: String,
        /// First and last line, 1 based and inclusive, as START:END
        #[arg(value_parser = parse_range_arg)]
        lines: (usize, usize),
        /// Print one JSON document
        #[arg(long)]
        json: bool,
    },
    /// List syntax references to a name
    ///
    /// Finds identifiers in code that spell the name, including declarations
    /// and imports, and skips comments and strings. Every result is a
    /// candidate: nothing checks scope or types, so unrelated names that share
    /// a spelling also appear.
    ///
    /// Given `<path>:<line>:<column>` instead of a name, a language server
    /// resolves the symbol at that position and lists its real references,
    /// marked resolved. If no server can answer, the name candidates are
    /// listed and the reason is reported.
    ///
    /// Example: `playbook code refs processPayment`
    Refs {
        /// Name to look for, or a position as `<path>:<line>:<column>`. A dotted or scoped name uses its last part
        target: String,
        /// Most results to show, from 1 to 200
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=200))]
        limit: u32,
        /// Longest wait for the server, in milliseconds, for a position
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=600_000))]
        timeout: Option<u64>,
        /// Print one JSON document
        #[arg(long)]
        json: bool,
    },
    /// Gather the code a task needs
    ///
    /// Describe the task in words. Finds matching declarations and text,
    /// widens text hits to the declaration around them, adds imports of the
    /// top files and tests that name a top declaration, merges overlaps and
    /// fills a size budget. Prints what did not fit.
    ///
    /// Example: `playbook code context "add retry support to payment processing"`
    Context {
        /// Task, in words
        query: String,
        /// Budget in tokens for the whole text output, counted as four bytes each
        #[arg(long, default_value_t = 4000, value_parser = clap::value_parser!(u32).range(200..=65536))]
        max_tokens: u32,
        /// Print one JSON document
        #[arg(long)]
        json: bool,
    },
    /// Show compile and type errors from a language server
    ///
    /// Without a path, checks the files that changed since the last index, up
    /// to 20. With one, checks that file. A server reports as it indexes, so a
    /// quiet answer on a big project may mean it is not done. At most 200
    /// problems are shown. Without a server the list is empty and the reason
    /// is reported.
    ///
    /// Example: `playbook code diagnostics src/app.ts`
    Diagnostics {
        /// File to check
        path: Option<String>,
        /// Longest wait for the server, in milliseconds
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=600_000))]
        timeout: Option<u64>,
        /// Print one JSON document
        #[arg(long)]
        json: bool,
    },
    /// Show where a symbol is defined, using a language server
    ///
    /// Takes `<path>:<line>:<column>`, with the column counted in characters
    /// from 1. A language server answers when this repo is trusted to run one
    /// and one is installed; otherwise the answer comes from the index and
    /// says why. Output keeps to files inside the repo.
    ///
    /// Example: `playbook code definition src/app.ts:12:5`
    Definition {
        /// Position, as `<path>:<line>:<column>`
        target: String,
        /// Longest wait for the server, in milliseconds
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=600_000))]
        timeout: Option<u64>,
        /// Print one JSON document
        #[arg(long)]
        json: bool,
    },
    /// Show the type and documentation at a position, using a language server
    ///
    /// Takes `<path>:<line>:<column>`. Falls back to the index signature of
    /// the name under the cursor, and says why.
    ///
    /// Example: `playbook code hover src/app.ts:12:5`
    Hover {
        /// Position, as `<path>:<line>:<column>`
        target: String,
        /// Longest wait for the server, in milliseconds
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=600_000))]
        timeout: Option<u64>,
        /// Print one JSON document
        #[arg(long)]
        json: bool,
    },
    /// Manage and inspect language servers
    ///
    /// Language servers answer exact questions about code. They run workspace
    /// code, so they start only in a repo you trust.
    ///
    /// Example: `playbook code lsp status`
    Lsp {
        #[command(subcommand)]
        sub: LspCommand,
    },
}

/// `playbook code lsp` subcommands.
#[derive(Subcommand, Debug)]
pub enum LspCommand {
    /// List the language servers playbook can use
    ///
    /// Shows each server with the languages it handles, its license and where
    /// playbook looks for it.
    ///
    /// Example: `playbook code lsp list`
    List {
        /// Print one JSON document
        #[arg(long)]
        json: bool,
    },
    /// Show which language servers are installed and what they still need
    ///
    /// Reports the server found for each language, where it came from, its
    /// version, any missing prerequisite and whether this repo is trusted to
    /// run servers. It starts no server and needs no trust.
    ///
    /// Example: `playbook code lsp status --json`
    Status {
        /// Print one JSON document
        #[arg(long)]
        json: bool,
    },
}

fn parse_range_arg(spec: &str) -> Result<(usize, usize), String> {
    super::source::parse_range(spec)
}

/// What a command needs from its surroundings.
pub struct Env {
    pub cwd: PathBuf,
    pub lsp: LspSettings,
}

impl Env {
    /// The current directory with the safe default settings: no server is trusted.
    pub fn real() -> Self {
        Self {
            cwd: std::env::current_dir().unwrap_or_default(),
            lsp: LspSettings::default(),
        }
    }
}

/// Runs one subcommand and returns what to print.
pub fn run(sub: CodeCommand, env: &Env) -> Result<String, String> {
    match sub {
        CodeCommand::Index { json, rebuild } => render_index(env, json, rebuild),
        CodeCommand::Status { json } => render_status(env, json),
        CodeCommand::Search {
            query,
            text,
            path,
            lang,
            limit,
            json,
        } => {
            let mut opts = SearchOptions::new(&query);
            opts.path = path.map(|p| normalize_prefix(&p));
            opts.lang = lang;
            opts.limit = limit as usize;
            let mode = if text { Mode::Text } else { Mode::Ranked };
            render_search(env, &opts, mode, json)
        }
        CodeCommand::Outline { path, json } => render_outline(env, &path, json),
        CodeCommand::Read { path, lines, json } => render_read(env, &path, lines, json),
        CodeCommand::Context {
            query,
            max_tokens,
            json,
        } => render_context(env, &query, max_tokens as usize, json),
        CodeCommand::Refs {
            target,
            limit,
            timeout,
            json,
        } if semantic::looks_like_position(&target) => semantic_render::refs(
            &semantic_render::Request {
                env,
                spec: &target,
                timeout_ms: timeout,
                json,
            },
            limit as usize,
        ),
        CodeCommand::Refs {
            target,
            limit,
            json,
            ..
        } => render_refs(env, &target, limit as usize, json, None),
        CodeCommand::Diagnostics {
            path,
            timeout,
            json,
        } => semantic_render::diagnostics(env, path.as_deref(), timeout, json),
        CodeCommand::Definition {
            target,
            timeout,
            json,
        } => semantic_render::definition(&semantic_render::Request {
            env,
            spec: &target,
            timeout_ms: timeout,
            json,
        }),
        CodeCommand::Hover {
            target,
            timeout,
            json,
        } => semantic_render::hover(&semantic_render::Request {
            env,
            spec: &target,
            timeout_ms: timeout,
            json,
        }),
        CodeCommand::Lsp { sub } => Ok(render_lsp(sub, env)),
        CodeCommand::Symbol {
            name,
            kind,
            lang,
            path,
            limit,
            json,
        } => {
            let mut opts = SearchOptions::new(&name);
            opts.kind = kind;
            opts.path = path.map(|p| normalize_prefix(&p));
            opts.lang = lang;
            opts.limit = limit as usize;
            render_search(env, &opts, Mode::Symbol, json)
        }
    }
}

/// A path prefix as the index stores paths: forward slashes, no leading `./`.
fn normalize_prefix(prefix: &str) -> String {
    let slashed = prefix.replace('\\', "/");
    let mut rest = slashed.as_str();
    while let Some(next) = rest.strip_prefix("./") {
        rest = next;
    }
    rest.to_string()
}

/// Which lookup a search-shaped command runs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Text,
    Ranked,
    Symbol,
}

fn render_search(
    env: &Env,
    opts: &SearchOptions,
    mode: Mode,
    json: bool,
) -> Result<String, String> {
    let command = match mode {
        Mode::Symbol => "symbol",
        Mode::Text | Mode::Ranked => "search",
    };
    let out = match mode {
        Mode::Text => super::search_text(&env.cwd, opts)?,
        Mode::Ranked => super::search_ranked(&env.cwd, opts)?,
        Mode::Symbol => super::symbol_lookup(&env.cwd, opts)?,
    };
    let state = status::state_of(&out.drift);
    let mut warnings = Vec::new();
    if state == IndexState::Stale {
        warnings.push(status::stale_warning(&out.drift));
    }
    if out.result.hits.iter().any(|h| h.stale) {
        warnings.push("some results changed after they were indexed".to_string());
    }
    if out.result.scan_capped {
        warnings.push(format!(
            "searched only the first {} files, so results may be incomplete",
            out.result.files_scanned.unwrap_or(0)
        ));
    }
    if json {
        let results: Vec<serde_json::Value> = out
            .result
            .hits
            .iter()
            .map(|h| {
                let mut row = json!({
                    "path": h.path,
                    "start_line": h.start_line,
                    "end_line": h.end_line,
                    "line": h.line,
                    "match_type": h.match_type,
                    "tier": h.tier,
                    "excerpt": h.excerpt,
                    "stale": h.stale,
                });
                if let (Some(sym), Some(map)) = (&h.symbol, row.as_object_mut()) {
                    map.insert("name".into(), json!(sym.name));
                    map.insert("qualified_name".into(), json!(sym.qualified_name));
                    map.insert("kind".into(), json!(sym.kind));
                    map.insert("lang".into(), json!(sym.lang));
                    map.insert("signature".into(), json!(sym.signature));
                    map.insert("exported".into(), json!(sym.exported));
                }
                row
            })
            .collect();
        let mut envelope = Envelope::new(
            command,
            state,
            out.index_path.display().to_string(),
            serde_json::Value::Array(results),
        );
        envelope.truncated = out.result.truncated;
        envelope.omitted = out.result.omitted;
        envelope.warnings = warnings;
        if let Some(n) = out.result.files_scanned {
            envelope.stats = Some(json!({"files_scanned": n}));
        }
        return Ok(envelope.to_json());
    }
    for warning in &warnings {
        eprintln!("code: {warning}");
    }
    let mut text = String::new();
    for h in &out.result.hits {
        let stale = if h.stale { " (stale)" } else { "" };
        let shown = match &h.symbol {
            Some(sym) => format!(
                "{} {}  {}",
                sym.kind,
                sym.qualified_name,
                sym.signature.as_deref().unwrap_or("")
            ),
            None => h
                .excerpt
                .lines()
                .nth(h.line.saturating_sub(h.excerpt_start))
                .unwrap_or("")
                .to_string(),
        };
        text.push_str(&format!(
            "{}:{}-{}{stale}  {}\n",
            h.path,
            h.start_line,
            h.end_line,
            shown.trim()
        ));
    }
    if out.result.omitted > 0 {
        text.push_str(&format!("{} more results not shown\n", out.result.omitted));
    }
    if out.result.scan_capped {
        let n = out.result.files_scanned.unwrap_or(0);
        text.push_str(&format!(
            "searched only the first {n} files, so results may be incomplete\n"
        ));
    } else if out.result.hits.is_empty() {
        text.push_str("no matches\n");
    }
    Ok(text)
}

/// Control characters other than tab as visible escapes, so file text cannot
/// drive the terminal.
fn visible(line: &str) -> String {
    line.chars()
        .flat_map(|c| {
            if c.is_control() && c != '\t' {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

fn render_context(env: &Env, query: &str, max_tokens: usize, json: bool) -> Result<String, String> {
    let mut opts = super::context::ContextOptions::new(query);
    opts.max_tokens = max_tokens;
    let out = super::build_context(&env.cwd, &opts)?;
    let state = status::state_of(&out.drift);
    let mut warnings = Vec::new();
    if state == IndexState::Stale {
        warnings.push(status::stale_warning(&out.drift));
    }
    if out.result.snippets.iter().any(|s| s.stale) {
        warnings.push("some snippets changed after they were indexed".to_string());
    }
    if out.result.unreadable > 0 {
        warnings.push(format!(
            "{} matches were skipped because their files can no longer be read",
            out.result.unreadable
        ));
    }
    if json {
        let snippets: Vec<serde_json::Value> = out
            .result
            .snippets
            .iter()
            .map(|s| {
                json!({
                    "path": s.path,
                    "start_line": s.start_line,
                    "end_line": s.end_line,
                    "reason": s.reason,
                    "tier": s.tier,
                    "stale": s.stale,
                    "text": s.text,
                })
            })
            .collect();
        let omitted: Vec<serde_json::Value> = out
            .result
            .omitted
            .iter()
            .map(|o| {
                json!({
                    "path": o.path,
                    "start_line": o.start_line,
                    "end_line": o.end_line,
                    "reason": o.reason,
                })
            })
            .collect();
        let mut envelope = Envelope::new(
            "context",
            state,
            out.index_path.display().to_string(),
            json!({
                "terms": out.result.terms,
                "snippets": snippets,
                "omitted": omitted,
                "bytes": out.result.bytes,
            }),
        );
        envelope.truncated = out.result.truncated;
        envelope.omitted = out.result.omitted_count;
        envelope.warnings = warnings;
        return Ok(envelope.to_json());
    }
    for warning in &warnings {
        eprintln!("code: {warning}");
    }
    Ok(super::context::render_text(
        &out.result,
        opts.byte_ceiling(),
        visible_block,
    ))
}

/// Like `visible`, for several lines.
fn visible_block(text: &str) -> String {
    text.split('\n').map(visible).collect::<Vec<_>>().join("\n")
}

/// Name candidates for `target`. `lsp` says why they stand in for resolved
/// references, when they do.
pub(crate) fn render_refs(
    env: &Env,
    target: &str,
    limit: usize,
    json: bool,
    lsp: Option<(LspInfo, String)>,
) -> Result<String, String> {
    let mut opts = super::refs::RefsOptions::new(target);
    opts.limit = limit;
    let out = super::find_refs(&env.cwd, &opts)?;
    let state = status::state_of(&out.drift);
    let mut warnings = Vec::new();
    if let Some((_, reason)) = &lsp {
        warnings.push(reason.clone());
    }
    if state == IndexState::Stale {
        warnings.push(status::stale_warning(&out.drift));
    }
    if out.result.files_capped {
        warnings.push(format!(
            "read only the first {} matching files, so results may be incomplete",
            out.result.files_scanned
        ));
    }
    if json {
        let rows: Vec<serde_json::Value> = out
            .result
            .refs
            .iter()
            .map(|r| {
                json!({
                    "path": r.path,
                    "line": r.line,
                    "column": r.column,
                    "role": r.role,
                    "text": r.text,
                    "confidence": "candidate",
                })
            })
            .collect();
        let mut envelope = Envelope::new(
            "refs",
            state,
            out.index_path.display().to_string(),
            serde_json::Value::Array(rows),
        );
        envelope.truncated = out.result.truncated;
        envelope.omitted = out.result.omitted;
        envelope.warnings = warnings;
        envelope.lsp = lsp.map(|(info, _)| info);
        envelope.stats = Some(json!({"files_scanned": out.result.files_scanned}));
        return Ok(envelope.to_json());
    }
    for warning in &warnings {
        eprintln!("code: {warning}");
    }
    let mut text = String::new();
    for r in &out.result.refs {
        text.push_str(&format!(
            "{}:{}:{}  {} (candidate)  {}\n",
            r.path,
            r.line,
            r.column,
            r.role,
            visible(&r.text)
        ));
    }
    if out.result.omitted > 0 {
        text.push_str(&format!("{} more results not shown\n", out.result.omitted));
    }
    if out.result.refs.is_empty() && !out.result.files_capped {
        text.push_str("no references\n");
    }
    Ok(text)
}

fn render_outline(env: &Env, input: &str, json: bool) -> Result<String, String> {
    let root = super::location::repo_root(&env.cwd)?;
    let out = super::source::outline(&root, &env.cwd, input, super::source::OUTLINE_CAP)?;
    let mut warnings = Vec::new();
    if out.partial {
        warnings
            .push("the file has syntax errors, so some declarations may be missing".to_string());
    }
    if json {
        let rows: Vec<serde_json::Value> = out
            .entries
            .iter()
            .map(|e| {
                json!({
                    "depth": e.depth,
                    "kind": e.kind,
                    "name": e.name,
                    "qualified_name": e.qualified_name,
                    "signature": e.signature,
                    "start_line": e.start_line,
                    "end_line": e.end_line,
                    "exported": e.exported,
                })
            })
            .collect();
        let mut envelope = Envelope::new(
            "outline",
            IndexState::NotUsed,
            String::new(),
            json!({"path": out.path, "lang": out.lang, "entries": rows}),
        );
        envelope.truncated = out.truncated;
        envelope.omitted = out.omitted;
        envelope.warnings = warnings;
        return Ok(envelope.to_json());
    }
    for warning in &warnings {
        eprintln!("code: {warning}");
    }
    let mut text = String::new();
    for e in &out.entries {
        text.push_str(&format!(
            "{}{}:{}-{}  {}  {}\n",
            "  ".repeat(e.depth),
            e.kind,
            e.start_line,
            e.end_line,
            e.qualified_name,
            e.signature.as_deref().unwrap_or("")
        ));
    }
    if out.truncated {
        text.push_str(&format!("{} more declarations not shown\n", out.omitted));
    }
    if out.entries.is_empty() {
        text.push_str("no declarations\n");
    }
    Ok(text)
}

fn render_read(
    env: &Env,
    input: &str,
    range: (usize, usize),
    json: bool,
) -> Result<String, String> {
    let root = super::location::repo_root(&env.cwd)?;
    let out = super::source::read_lines(
        &root,
        &env.cwd,
        input,
        range,
        super::source::Caps::default(),
    )?;
    let mut warnings = Vec::new();
    if out.clamped {
        warnings.push(format!(
            "the range ends past the file; showing up to line {} of {}",
            out.end, out.total_lines
        ));
    }
    if json {
        let mut envelope = Envelope::new(
            "read",
            IndexState::NotUsed,
            String::new(),
            json!({
                "path": out.path,
                "start_line": out.start,
                "end_line": out.end,
                "total_lines": out.total_lines,
                "text": out.text,
                "last_line_partial": out.last_line_partial,
            }),
        );
        envelope.truncated = out.truncated;
        envelope.warnings = warnings;
        return Ok(envelope.to_json());
    }
    for warning in &warnings {
        eprintln!("code: {warning}");
    }
    let mut text = String::new();
    if out.end >= out.start {
        for (offset, line) in out.text.split('\n').enumerate() {
            text.push_str(&format!("{:>6}  {}\n", out.start + offset, visible(line)));
        }
    }
    if out.truncated {
        text.push_str("output truncated\n");
    }
    Ok(text)
}

fn render_lsp(sub: LspCommand, env: &Env) -> String {
    let (command, json, text, results) = match sub {
        LspCommand::List { json } => (
            "lsp list",
            json,
            lsp::status::list_text(),
            lsp::status::list_json(),
        ),
        LspCommand::Status { json } => {
            let statuses = lsp::status::collect(&env.lsp, &lsp::status::Machine::real());
            (
                "lsp status",
                json,
                lsp::status::status_text(&env.lsp, &statuses),
                lsp::status::status_json(&env.lsp, &statuses),
            )
        }
    };
    if json {
        Envelope::new(command, IndexState::NotUsed, String::new(), results).to_json()
    } else {
        text
    }
}

fn render_status(env: &Env, json: bool) -> Result<String, String> {
    match super::status(&env.cwd)? {
        super::StatusOutcome::Missing { index_path } => {
            if json {
                let mut envelope = Envelope::new(
                    "status",
                    IndexState::Missing,
                    index_path.display().to_string(),
                    json!({}),
                );
                envelope.warnings = vec![format!("no index yet; run `{}`", host::command("index"))];
                return Ok(envelope.to_json());
            }
            Ok(format!(
                "index: missing; run `{}`\n",
                host::command("index")
            ))
        }
        super::StatusOutcome::Found { index_path, status } => {
            if json {
                let mut envelope = Envelope::new(
                    "status",
                    status.state,
                    index_path.display().to_string(),
                    status.to_json(),
                );
                if status.state == IndexState::Stale {
                    envelope.warnings = vec![super::status::stale_warning(&status.drift)];
                }
                return Ok(envelope.to_json());
            }
            Ok(status.to_text())
        }
    }
}

fn render_index(env: &Env, json: bool, rebuild: bool) -> Result<String, String> {
    let outcome = super::index(&env.cwd, rebuild)?;
    let report = &outcome.report;
    let mut warnings = Vec::new();
    if report.failed > 0 {
        warnings.push(format!(
            "{} files could not be read and keep their previous index rows",
            report.failed
        ));
    }
    if json {
        let mut envelope = Envelope::new(
            "index",
            IndexState::Fresh,
            outcome.index_path.display().to_string(),
            json!({
                "indexed": report.indexed,
                "changed": report.changed,
                "unchanged": report.unchanged,
                "parsed": report.parsed,
                "removed": report.removed,
                "skipped": report.skipped,
                "failed": report.failed,
            }),
        );
        envelope.warnings = warnings;
        return Ok(envelope.to_json());
    }
    for warning in &warnings {
        eprintln!("code: {warning}");
    }
    Ok(format!(
        "indexed {} files: {} changed, {} unchanged, {} removed, {} skipped, {} failed\n",
        report.indexed,
        report.changed,
        report.unchanged,
        report.removed,
        report.skipped,
        report.failed
    ))
}

#[cfg(test)]
mod tests {
    use super::{normalize_prefix, visible};

    #[test]
    fn control_characters_print_as_escapes_and_tabs_stay() {
        // Arrange, Act, Assert
        assert_eq!(visible("a\u{1b}[31mb\rc\td"), "a\\u{1b}[31mb\\rc\td");
    }

    #[test]
    fn a_prefix_loses_leading_dot_slashes_and_backslashes() {
        // Arrange, Act, Assert
        assert_eq!(normalize_prefix("./lib/"), "lib/");
        assert_eq!(normalize_prefix("././lib"), "lib");
        assert_eq!(normalize_prefix("lib\\pay"), "lib/pay");
        assert_eq!(normalize_prefix("lib/"), "lib/");
    }
}
