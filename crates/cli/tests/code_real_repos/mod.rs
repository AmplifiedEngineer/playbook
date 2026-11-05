// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Real-repository mode of the code benchmark. It runs the Tree-sitter index
//! and any number of language servers on shallow clones of open-source repos,
//! over 20 tasks per repo picked by `pick_tasks.py` from the repo's own
//! symbols. Switch: `BENCH_CODE_REAL_MANIFEST=<manifest.json>` on the ignored
//! `bench_code` test (see `manifest.example.json`). Nothing is installed or
//! run from the repos; servers come from the paths the manifest names.
//! Bytes stand in for tokens, as in the other arms.

#![allow(
    dead_code,
    unreachable_pub,
    reason = "each test crate uses a different part of this shared module"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::code_lsp_arm::{file_uri, places, render, Client, Place};

const WARM_PASSES: usize = 2;
const SEARCH_TOP: usize = 10;
const LINE_SLACK: usize = 2;

#[derive(Debug, Clone)]
pub struct Task {
    pub kind: String,
    pub query: String,
    pub name: String,
    pub expected: Vec<(String, usize)>,
    pub decl: Option<(String, usize)>,
    pub site: Option<(String, usize)>,
    pub names: Vec<String>,
    pub common: bool,
    /// Expected lines where the name only sits in a comment or a string.
    pub noncode: Vec<(String, usize)>,
}

fn pair(v: &Value) -> Option<(String, usize)> {
    Some((v[0].as_str()?.to_string(), v[1].as_u64()? as usize))
}

pub fn load_tasks(path: &Path) -> Vec<Task> {
    let doc: Value = serde_json::from_str(&fs::read_to_string(path).expect("tasks file"))
        .expect("tasks file is JSON");
    doc["tasks"]
        .as_array()
        .expect("tasks")
        .iter()
        .map(|t| Task {
            kind: t["kind"].as_str().unwrap_or_default().to_string(),
            query: t["query"].as_str().unwrap_or_default().to_string(),
            name: t["name"]
                .as_str()
                .or_else(|| t["query"].as_str())
                .unwrap_or_default()
                .to_string(),
            expected: t["expected"]
                .as_array()
                .map_or(vec![], |l| l.iter().filter_map(pair).collect()),
            decl: pair(&t["decl"]),
            site: pair(&t["site"]),
            names: t["names"].as_array().map_or(vec![], |l| {
                l.iter()
                    .filter_map(|n| n.as_str().map(String::from))
                    .collect()
            }),
            common: t["common"].as_bool().unwrap_or(false),
            noncode: t["noncode"]
                .as_array()
                .map_or(vec![], |l| l.iter().filter_map(pair).collect()),
        })
        .collect()
}

/// A hit within `LINE_SLACK` lines of the expected line: servers report the
/// name line, which can differ from the line the picker counted by a decorator.
pub fn near(found: &Place, path: &str, line: usize) -> bool {
    found.path == path && found.line <= line + LINE_SLACK && found.line + LINE_SLACK >= line
}

pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    let index = ((sorted.len() as f64 * p).ceil() as usize).saturating_sub(1);
    sorted.get(index).copied().unwrap_or(0.0)
}

pub fn latency(times: &[f64]) -> Value {
    let mut t = times.to_vec();
    t.sort_by(f64::total_cmp);
    json!({
        "n": t.len(),
        "p50_ms": percentile(&t, 0.5),
        "p90_ms": percentile(&t, 0.9),
        "p95_ms": percentile(&t, 0.95),
        "p99_ms": percentile(&t, 0.99),
        "max_ms": t.last().copied().unwrap_or(0.0),
    })
}

fn median(values: &[f64]) -> f64 {
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

// ---------------------------------------------------------------- memory

/// Resident set size in KiB of `root` and all its descendants.
pub fn tree_rss_kb(root: u32) -> u64 {
    let Ok(out) = Command::new("ps")
        .args(["-axo", "pid=,ppid=,rss="])
        .output()
    else {
        return 0;
    };
    let rows: Vec<(u32, u32, u64)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut p = l.split_whitespace();
            Some((
                p.next()?.parse().ok()?,
                p.next()?.parse().ok()?,
                p.next()?.parse().ok()?,
            ))
        })
        .collect();
    sum_tree(&rows, root)
}

pub fn sum_tree(rows: &[(u32, u32, u64)], root: u32) -> u64 {
    let mut members = vec![root];
    let mut i = 0;
    while i < members.len() {
        let parent = members[i];
        members.extend(rows.iter().filter(|r| r.1 == parent).map(|r| r.0));
        i += 1;
    }
    rows.iter()
        .filter(|r| members.contains(&r.0))
        .map(|r| r.2)
        .sum()
}

/// Polls the server's process tree and keeps the peak.
pub struct Sampler {
    stop: Arc<AtomicBool>,
    peak: Arc<AtomicU64>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Sampler {
    pub fn start(pid: u32) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(AtomicU64::new(0));
        let (s, p) = (stop.clone(), peak.clone());
        let handle = thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                p.fetch_max(tree_rss_kb(pid), Ordering::Relaxed);
                thread::sleep(Duration::from_millis(200));
            }
        });
        Self {
            stop,
            peak,
            handle: Some(handle),
        }
    }

    pub fn finish(mut self) -> f64 {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.peak.load(Ordering::Relaxed) as f64 / 1024.0
    }
}

// ---------------------------------------------------------------- baselines

fn rg(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("rg")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("rg runs");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The plan's agent baseline: the first 200 `rg` hit lines, then 100 lines
/// around each of the first 5 matches. A file-context task reads the file.
pub fn baseline_bytes(repo: &Path, task: &Task) -> f64 {
    if task.kind == "file_context" {
        let text = fs::read_to_string(repo.join(&task.query)).unwrap_or_default();
        return text.lines().take(2000).map(|l| l.len() + 1).sum::<usize>() as f64;
    }
    let flag = if task.kind == "references" || task.kind == "definition" {
        "-w"
    } else {
        "-F"
    };
    let hits = rg(repo, &["-n", flag, "--", &task.query]);
    let listing: usize = hits.lines().take(200).map(|l| l.len() + 1).sum();
    let windows: usize = hits
        .lines()
        .take(5)
        .filter_map(|l| {
            let mut parts = l.splitn(3, ':');
            let (file, line) = (parts.next()?, parts.next()?.parse::<usize>().ok()?);
            let text = fs::read_to_string(repo.join(file)).ok()?;
            Some(
                text.lines()
                    .skip(line.saturating_sub(50))
                    .take(100)
                    .map(|l| l.len() + 1)
                    .sum::<usize>(),
            )
        })
        .sum();
    (listing + windows) as f64
}

// ---------------------------------------------------------------- scoring

/// What one task produced, before it is folded into the per-kind numbers.
#[derive(Debug, Default, Clone)]
pub struct Outcome {
    pub ok: bool,
    pub hit: usize,
    pub total: usize,
    pub same_hit: usize,
    pub same_total: usize,
    /// The same four counts over code references only (no comment or string).
    pub code_hit: usize,
    pub code_total: usize,
    pub code_same_hit: usize,
    pub code_same_total: usize,
    pub bytes: usize,
    pub returned: usize,
    pub missed: Vec<String>,
}

fn score_refs(task: &Task, found: &[Place], bytes: usize) -> Outcome {
    let decl_path = task.decl.as_ref().map(|d| d.0.as_str()).unwrap_or_default();
    let have = |p: &str, l: usize| found.iter().any(|f| f.path == p && f.line == l);
    let mut o = Outcome {
        bytes,
        returned: found.len(),
        total: task.expected.len(),
        ..Outcome::default()
    };
    for (p, l) in &task.expected {
        let h = have(p, *l);
        let code = !task.noncode.iter().any(|(np, nl)| np == p && nl == l);
        let same = p == decl_path;
        o.hit += usize::from(h);
        o.same_total += usize::from(same);
        o.same_hit += usize::from(same && h);
        o.code_total += usize::from(code);
        o.code_hit += usize::from(code && h);
        o.code_same_total += usize::from(code && same);
        o.code_same_hit += usize::from(code && same && h);
        if !h && o.missed.len() < 5 {
            o.missed.push(format!(
                "{p}:{l}{}",
                if code { "" } else { " (comment or string)" }
            ));
        }
    }
    o.ok = o.hit == o.total;
    o
}

fn score_decl(task: &Task, found: &[Place], bytes: usize) -> Outcome {
    let (p, l) = &task.expected[0];
    let ok = found.iter().any(|f| near(f, p, *l));
    Outcome {
        ok,
        hit: usize::from(ok),
        total: 1,
        bytes,
        returned: found.len(),
        ..Outcome::default()
    }
}

fn score_names(task: &Task, names: &[String], bytes: usize) -> Outcome {
    let hit = task.names.iter().filter(|n| names.contains(n)).count();
    Outcome {
        ok: hit == task.names.len(),
        hit,
        total: task.names.len(),
        bytes,
        returned: names.len(),
        ..Outcome::default()
    }
}

// ---------------------------------------------------------------- the index

pub struct IndexEnv {
    pub bin: PathBuf,
    pub home: PathBuf,
}

impl IndexEnv {
    fn run(&self, repo: &Path, args: &[&str]) -> (Value, usize, f64) {
        let began = Instant::now();
        let out = Command::new(&self.bin)
            .args(args)
            .current_dir(repo)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", "/usr/bin:/bin:/opt/homebrew/bin")
            .output()
            .expect("playbook runs");
        let ms = began.elapsed().as_secs_f64() * 1000.0;
        let doc = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
        (doc, out.stdout.len(), ms)
    }
}

fn index_places(doc: &Value) -> Vec<Place> {
    doc["results"].as_array().map_or(vec![], |l| {
        l.iter()
            .filter_map(|r| {
                Some(Place {
                    path: r["path"].as_str()?.to_string(),
                    line: r["start_line"].as_u64().or_else(|| r["line"].as_u64())? as usize,
                })
            })
            .collect()
    })
}

fn index_task(env: &IndexEnv, repo: &Path, task: &Task) -> (Outcome, f64) {
    let (doc, bytes, ms) = match task.kind.as_str() {
        "references" => env.run(
            repo,
            &["code", "refs", &task.name, "--limit", "200", "--json"],
        ),
        "symbol_search" => env.run(
            repo,
            &["code", "search", &task.query, "--limit", "10", "--json"],
        ),
        "file_context" => env.run(repo, &["code", "outline", &task.query, "--json"]),
        _ => env.run(repo, &["code", "symbol", &task.name, "--json"]),
    };
    let outcome = match task.kind.as_str() {
        "references" => score_refs(task, &index_places(&doc), bytes),
        "file_context" => {
            let names: Vec<String> = doc["results"]["entries"].as_array().map_or(vec![], |l| {
                l.iter()
                    .filter_map(|e| e["name"].as_str().map(String::from))
                    .collect()
            });
            score_names(task, &names, bytes)
        }
        _ => {
            let top: Vec<Place> = index_places(&doc).into_iter().take(SEARCH_TOP).collect();
            score_decl(task, &top, bytes)
        }
    };
    (outcome, ms)
}

// ---------------------------------------------------------------- servers

pub struct ServerSpec {
    pub name: String,
    pub version: String,
    pub lang: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub options: Value,
    pub env: Vec<(String, String)>,
}

fn language_id(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "ts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "rs" => "rust",
        _ => "python",
    }
}

fn column_of(root: &Path, place: &(String, usize), name: &str) -> usize {
    fs::read_to_string(root.join(&place.0))
        .ok()
        .and_then(|t| t.lines().nth(place.1 - 1).and_then(|l| l.find(name)))
        .unwrap_or(0)
}

/// Collects names from `SymbolInformation[]` or nested `DocumentSymbol[]`.
fn symbol_names(answer: &Value, out: &mut Vec<String>) {
    if let Some(list) = answer.as_array() {
        for s in list {
            if let Some(n) = s["name"].as_str() {
                out.push(n.to_string());
            }
            symbol_names(&s["children"], out);
        }
    }
}

struct Session<'a> {
    client: Client,
    root: &'a Path,
    wait: Duration,
    opened: Vec<String>,
}

impl Session<'_> {
    fn open(&mut self, rel: &str) {
        if !self.opened.iter().any(|o| o == rel) {
            self.client.did_open(&self.root.join(rel), language_id(rel));
            self.opened.push(rel.to_string());
        }
    }

    fn symbols(&mut self, query: &str) -> Vec<(String, Value)> {
        let answer = self
            .client
            .request("workspace/symbol", json!({"query": query}), self.wait)
            .unwrap_or(Value::Null);
        answer.as_array().map_or(vec![], |l| {
            l.iter()
                .filter_map(|s| Some((s["name"].as_str()?.to_string(), s.clone())))
                .collect()
        })
    }

    fn exact(&mut self, name: &str) -> Vec<Place> {
        let hits: Vec<Value> = self
            .symbols(name)
            .into_iter()
            .filter(|(n, _)| n == name)
            .map(|(_, v)| v)
            .collect();
        places(&Value::Array(hits), self.root)
    }

    fn at(&self, rel: &str, line: usize, col: usize) -> Value {
        json!({
            "textDocument": {"uri": file_uri(&self.root.join(rel))},
            "position": {"line": line - 1, "character": col},
            "context": {"includeDeclaration": false},
        })
    }

    /// `workspace/symbol`, then the position request at the declaration.
    fn declared_then(&mut self, task: &Task, method: &str) -> (Vec<Place>, usize) {
        let decl = self.exact(&task.name);
        let Some(first) = decl
            .iter()
            .find(|p| task.decl.as_ref().is_some_and(|d| near(p, &d.0, d.1)))
            .or_else(|| decl.first())
            .cloned()
        else {
            return (vec![], 0);
        };
        let col = column_of(self.root, &(first.path.clone(), first.line), &task.name);
        self.open(&first.path);
        let params = self.at(&first.path, first.line, col);
        let found = self
            .client
            .request(method, params, self.wait)
            .unwrap_or(Value::Null);
        let mut got = places(&found, self.root);
        got.dedup();
        (got, decl.len())
    }

    fn run(&mut self, task: &Task) -> Outcome {
        match task.kind.as_str() {
            "declaration" => {
                let found = self.exact(&task.name);
                score_decl(task, &found, render(&found).len())
            }
            "references" => {
                let (found, _) = self.declared_then(task, "textDocument/references");
                score_refs(task, &found, render(&found).len())
            }
            "symbol_search" => {
                let answer = self.symbols(&task.query);
                let top: Vec<Value> = answer
                    .into_iter()
                    .take(SEARCH_TOP)
                    .map(|(_, v)| v)
                    .collect();
                let found = places(&Value::Array(top), self.root);
                score_decl(task, &found, render(&found).len())
            }
            "definition" => {
                let (path, line) = task.site.clone().unwrap_or_default();
                let col = column_of(self.root, &(path.clone(), line), &task.name);
                self.open(&path);
                let params = self.at(&path, line, col);
                let found = self
                    .client
                    .request("textDocument/definition", params, self.wait)
                    .unwrap_or(Value::Null);
                let got = places(&found, self.root);
                score_decl(task, &got, render(&got).len())
            }
            _ => {
                self.open(&task.query);
                let answer = self
                    .client
                    .request(
                        "textDocument/documentSymbol",
                        json!({"textDocument": {"uri": file_uri(&self.root.join(&task.query))}}),
                        self.wait,
                    )
                    .unwrap_or(Value::Null);
                let mut names = vec![];
                symbol_names(&answer, &mut names);
                let bytes = names.iter().map(|n| n.len() + 1).sum();
                score_names(task, &names, bytes)
            }
        }
    }
}

// ---------------------------------------------------------------- one run

fn fold(rows: &[(&Task, Outcome)]) -> Value {
    let by_kind = |kind: &str| -> Vec<&(&Task, Outcome)> {
        rows.iter().filter(|(t, _)| t.kind == kind).collect()
    };
    let rate = |hit: usize, total: usize| {
        if total == 0 {
            Value::Null
        } else {
            json!(100.0 * hit as f64 / total as f64)
        }
    };
    let tasks_ok = |kind: &str| {
        let v = by_kind(kind);
        rate(v.iter().filter(|(_, o)| o.ok).count(), v.len())
    };
    let refs: Vec<_> = by_kind("references");
    let sum = |f: &dyn Fn(&Outcome) -> usize, skip_common: bool| -> usize {
        refs.iter()
            .filter(|(t, _)| !(skip_common && t.common))
            .map(|(_, o)| f(o))
            .sum()
    };
    let ctx = by_kind("file_context");
    json!({
        "declaration_recall_pct": tasks_ok("declaration"),
        "symbol_search_recall_pct": tasks_ok("symbol_search"),
        "definition_recall_pct": tasks_ok("definition"),
        "file_context_name_recall_pct": rate(
            ctx.iter().map(|(_, o)| o.hit).sum(), ctx.iter().map(|(_, o)| o.total).sum()),
        "references_recall_all_pct": rate(sum(&|o| o.hit, false), sum(&|o| o.total, false)),
        "references_recall_incl_noncode_pct": rate(sum(&|o| o.hit, true), sum(&|o| o.total, true)),
        "references_recall_code_pct": rate(sum(&|o| o.code_hit, true), sum(&|o| o.code_total, true)),
        "references_recall_code_same_file_pct": rate(
            sum(&|o| o.code_same_hit, true), sum(&|o| o.code_same_total, true)),
        "references_recall_code_cross_file_pct": rate(
            sum(&|o| o.code_hit - o.code_same_hit, true),
            sum(&|o| o.code_total - o.code_same_total, true)),
        "references_recall_common_identifier_pct": rate(
            refs.iter().filter(|(t, _)| t.common).map(|(_, o)| o.code_hit).sum(),
            refs.iter().filter(|(t, _)| t.common).map(|(_, o)| o.code_total).sum()),
    })
}

fn passes(
    repo: &Path,
    tasks: &[Task],
    mut run: impl FnMut(&Task) -> (Outcome, f64),
) -> (Vec<(Outcome, f64)>, Vec<f64>) {
    let _ = repo;
    let first: Vec<(Outcome, f64)> = tasks.iter().map(&mut run).collect();
    let mut warm = vec![];
    for _ in 0..WARM_PASSES {
        warm.extend(tasks.iter().map(|t| run(t).1));
    }
    (first, warm)
}

fn summarize(tasks: &[Task], first: &[(Outcome, f64)], warm: &[f64], base: &[f64]) -> Value {
    let rows: Vec<(&Task, Outcome)> = tasks
        .iter()
        .zip(first.iter().map(|(o, _)| o.clone()))
        .collect();
    let bytes: Vec<f64> = first.iter().map(|(o, _)| o.bytes as f64).collect();
    let mut summary = fold(&rows);
    summary["output_bytes_median"] = json!(median(&bytes));
    summary["plan_baseline_bytes_median"] = json!(median(base));
    summary["bytes_saved_vs_plan_baseline_pct"] =
        json!(100.0 * (1.0 - median(&bytes) / median(base).max(1.0)));
    summary["warm_latency"] = latency(warm);
    summary["first_run_latency"] = latency(&first.iter().map(|(_, ms)| *ms).collect::<Vec<_>>());
    summary["per_task"] = json!(rows
        .iter()
        .zip(first)
        .map(|((t, o), (_, ms))| json!({
            "kind": t.kind, "query": t.query, "ok": o.ok, "hit": o.hit, "total": o.total,
            "bytes": o.bytes, "returned": o.returned, "first_ms": ms, "common": t.common,
            "missed_sample": o.missed,
        }))
        .collect::<Vec<_>>());
    summary
}

fn run_server(spec: &ServerSpec, repo: &Path, tasks: &[Task], limit: Duration) -> Value {
    let mut out = json!({"server": spec.name, "version": spec.version, "program": spec.program});
    let began = Instant::now();
    let args: Vec<&str> = spec.args.iter().map(String::as_str).collect();
    let Ok(client) = Client::spawn_env(&spec.program, &args, repo, &spec.env) else {
        out["failure"] = json!("spawn failed");
        return out;
    };
    let sampler = Sampler::start(client.pid());
    let mut session = Session {
        client,
        root: repo,
        wait: limit,
        opened: vec![],
    };
    if !session.client.initialize(repo, spec.options.clone()) {
        out["failure"] = json!("initialize timed out or failed");
        out["alive"] = json!(session.client.is_alive());
        session.client.stop();
        out["peak_rss_mb"] = json!(sampler.finish());
        return out;
    }
    let decls: Vec<&Task> = tasks.iter().filter(|t| t.kind == "declaration").collect();
    let (Some(open_task), Some(probe_task)) = (decls.first(), decls.last()) else {
        out["failure"] = json!("no declaration task");
        session.client.stop();
        return out;
    };
    session.open(&open_task.expected[0].0);
    // First answer: workspace/symbol finds the declaration in the opened file.
    // Workspace ready: it also finds the last declaration task's name, which
    // can sit in a project the server has not loaded.
    for (target, key) in [
        (&open_task.name, "first_answer_s"),
        (&probe_task.name, "workspace_ready_s"),
    ] {
        let mut found = false;
        while began.elapsed() < limit {
            let left = limit.saturating_sub(began.elapsed());
            session.wait = left.min(Duration::from_secs(30));
            if !session.exact(target).is_empty() {
                found = true;
                break;
            }
            if !session.client.is_alive() {
                out["failure"] = json!("server exited before it answered");
                break;
            }
            thread::sleep(Duration::from_millis(250));
        }
        if !found {
            if out.get("failure").is_none() {
                out["failure"] = json!(format!("{key} not reached within {} s", limit.as_secs()));
            }
            break;
        }
        out[key] = json!(began.elapsed().as_secs_f64());
    }
    out["cold_start_s"] = out.get("first_answer_s").cloned().unwrap_or(Value::Null);
    // A server that never answered still gets the tasks, with a short wait.
    let ready = out.get("first_answer_s").is_some();
    session.wait = if ready {
        limit
    } else {
        Duration::from_secs(10)
    };
    // Upper bound: open every file the tasks touch, as an editor with all of
    // them open would, so each server loads every project involved.
    let preopen = std::env::var("BENCH_CODE_REAL_PREOPEN").is_ok_and(|v| v == "1");
    out["preopened_task_files"] = json!(preopen);
    if preopen {
        for t in tasks {
            let mut files: Vec<String> = t.expected.iter().take(1).map(|e| e.0.clone()).collect();
            files.extend(t.decl.iter().chain(t.site.iter()).map(|e| e.0.clone()));
            if t.kind == "file_context" {
                files.push(t.query.clone());
            }
            for f in files {
                session.open(&f);
            }
        }
        thread::sleep(Duration::from_secs(5));
    }
    let mut timeouts = 0usize;
    let (first, warm) = passes(repo, tasks, |t| {
        let began = Instant::now();
        let o = session.run(t);
        let ms = began.elapsed().as_secs_f64() * 1000.0;
        if ms >= session.wait.as_secs_f64() * 1000.0 - 50.0 {
            timeouts += 1;
        }
        (o, ms)
    });
    let base: Vec<f64> = tasks.iter().map(|t| baseline_bytes(repo, t)).collect();
    out["alive_at_end"] = json!(session.client.is_alive());
    out["steady_rss_mb"] = json!(tree_rss_kb(session.client.pid()) as f64 / 1024.0);
    out["timeouts"] = json!(timeouts);
    out["results"] = summarize(tasks, &first, &warm, &base);
    session.client.stop();
    out["peak_rss_mb"] = json!(sampler.finish());
    out
}

/// Runs everything in the manifest and returns the report.
pub fn run_manifest(manifest: &Path, bin: &Path) -> Value {
    let doc: Value = serde_json::from_str(&fs::read_to_string(manifest).expect("manifest"))
        .expect("manifest is JSON");
    let dir = manifest.parent().unwrap_or(Path::new("."));
    let limit = Duration::from_secs(doc["limit_s"].as_u64().unwrap_or(120));
    let home = PathBuf::from(doc["home"].as_str().unwrap_or("/tmp/pb-lsp-real-home"));
    fs::create_dir_all(&home).expect("home");
    let env = IndexEnv {
        bin: bin.to_path_buf(),
        home: home.clone(),
    };
    let servers: Vec<ServerSpec> = doc["servers"].as_array().map_or(vec![], |l| {
        l.iter()
            .map(|s| ServerSpec {
                name: s["name"].as_str().unwrap_or_default().into(),
                version: s["version"].as_str().unwrap_or_default().into(),
                lang: s["lang"].as_str().unwrap_or_default().into(),
                program: PathBuf::from(s["program"].as_str().unwrap_or_default()),
                args: s["args"].as_array().map_or(vec![], |a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                }),
                options: s["options"].clone(),
                env: vec![
                    ("HOME".into(), home.display().to_string()),
                    (
                        "XDG_CACHE_HOME".into(),
                        home.join(".cache").display().to_string(),
                    ),
                ],
            })
            .collect()
    });
    let only = std::env::var("BENCH_CODE_REAL_ONLY").ok();
    let mut repos = vec![];
    for r in doc["repos"].as_array().expect("repos") {
        let name = r["name"].as_str().unwrap_or_default();
        if only
            .as_deref()
            .is_some_and(|o| !o.split(',').any(|x| x == name))
        {
            continue;
        }
        let path = PathBuf::from(r["path"].as_str().expect("repo path"))
            .canonicalize()
            .expect("repo exists");
        let lang = r["lang"].as_str().unwrap_or_default();
        let tasks = load_tasks(&dir.join(r["tasks"].as_str().expect("tasks path")));
        let began = Instant::now();
        let built = env.run(&path, &["code", "index", "--json"]);
        let cold_index_s = began.elapsed().as_secs_f64();
        let base: Vec<f64> = tasks.iter().map(|t| baseline_bytes(&path, t)).collect();
        let (first, warm) = passes(&path, &tasks, |t| index_task(&env, &path, t));
        let mut entry = json!({
            "name": name, "lang": lang, "sha": r["sha"], "source_files": r["source_files"],
            "tasks": tasks.len(),
            "index": {
                "cold_index_s": cold_index_s,
                "indexed": built.0["results"]["indexed"],
                "results": summarize(&tasks, &first, &warm, &base),
            },
        });
        let mut runs = vec![];
        for spec in servers.iter().filter(|s| s.lang == lang) {
            if std::env::var("BENCH_CODE_REAL_SERVERS")
                .ok()
                .is_some_and(|only| !only.split(',').any(|x| x == spec.name))
            {
                continue;
            }
            eprintln!("{name}: {}", spec.name);
            runs.push(run_server(spec, &path, &tasks, limit));
        }
        entry["servers"] = Value::Array(runs);
        repos.push(entry);
    }
    json!({"repos": repos, "limit_s": limit.as_secs(), "warm_passes": WARM_PASSES})
}

#[test]
fn rss_of_a_process_tree_sums_the_root_and_its_descendants() {
    // Arrange: 1 spawns 2 and 3, 3 spawns 4; 9 is unrelated.
    let rows = [(1, 0, 10), (2, 1, 20), (3, 1, 30), (4, 3, 40), (9, 0, 500)];

    // Act
    let total = sum_tree(&rows, 1);

    // Assert
    assert_eq!(total, 100);
}

#[test]
fn a_hit_may_sit_two_lines_after_a_decorator() {
    // Arrange
    let at = |line| Place {
        path: "a.ts".into(),
        line,
    };

    // Act and assert
    assert!(near(&at(12), "a.ts", 10));
    assert!(near(&at(9), "a.ts", 10));
    assert!(!near(&at(13), "a.ts", 10));
    assert!(!near(&at(10), "b.ts", 10));
}

#[test]
fn references_recall_splits_same_file_from_cross_file() {
    // Arrange
    let task = Task {
        kind: "references".into(),
        query: "f".into(),
        name: "f".into(),
        expected: vec![("a.py".into(), 3), ("b.py".into(), 5), ("c.py".into(), 7)],
        decl: Some(("a.py".into(), 1)),
        site: None,
        names: vec![],
        common: false,
        noncode: vec![("b.py".into(), 5)],
    };
    let found = [
        Place {
            path: "a.py".into(),
            line: 3,
        },
        Place {
            path: "c.py".into(),
            line: 7,
        },
    ];

    // Act
    let o = score_refs(&task, &found, 10);

    // Assert
    assert_eq!((o.hit, o.total, o.same_hit, o.same_total), (2, 3, 1, 1));
    assert_eq!((o.code_hit, o.code_total), (2, 2));
    assert_eq!(o.missed, vec!["b.py:5 (comment or string)".to_string()]);
    assert!(!o.ok);
}

#[test]
fn latency_reports_the_tail_not_only_the_median() {
    // Arrange
    let times: Vec<f64> = (1..=100).map(f64::from).collect();

    // Act
    let l = latency(&times);

    // Assert
    assert_eq!(l["p50_ms"], 50.0);
    assert_eq!(l["p99_ms"], 99.0);
    assert_eq!(l["max_ms"], 100.0);
}
