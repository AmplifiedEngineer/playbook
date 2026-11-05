// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Timing and size benchmark for `playbook code`, run by `just bench-code`.
//! It compares the index against `rg` with whole-file reads (arm a) and `rg`
//! with 30 lines of context (arm b), and writes a JSON report with a verdict
//! per fixture size. Bytes stand in for tokens. CI never runs the ignored test.

mod code_fixture;
mod code_lsp_arm;

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use code_fixture::{
    correctness, generate_with, stale_without_flag, verdict, which, Harness, Kind, Numbers, Repo,
    Task,
};
use serde_json::{json, Value};

const RUNS: usize = 20;
const READ_LINE_CAP: usize = 2_000;
const DEFAULT_SIZES: &str = "100,5000,50000";

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values.get(values.len() / 2).copied().unwrap_or(0.0)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let index = ((sorted.len() as f64 * p).ceil() as usize).saturating_sub(1);
    sorted.get(index).copied().unwrap_or(0.0)
}

/// Milliseconds for each of `RUNS` runs of the command, sorted.
fn timings(h: &Harness, args: &[&str]) -> Vec<f64> {
    let mut times: Vec<f64> = (0..RUNS)
        .map(|_| {
            let start = Instant::now();
            h.playbook(args);
            start.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    times.sort_by(f64::total_cmp);
    times
}

fn latency_json(times: &[f64]) -> Value {
    json!({"p50_ms": percentile(times, 0.5), "p95_ms": percentile(times, 0.95)})
}

fn rg(h: &Harness, args: &[&str]) -> String {
    let out = Command::new(h.bin.join("rg"))
        .args(args)
        .current_dir(&h.repo)
        .output()
        .expect("rg runs");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Arm a: the `rg` hits plus every matched file read up to the line cap.
fn arm_a_bytes(h: &Harness, task: &Task) -> f64 {
    let flag = if task.kind == Kind::References {
        "-w"
    } else {
        "-F"
    };
    let hits = rg(h, &["-n", flag, &task.query]);
    let mut files: Vec<&str> = hits.lines().filter_map(|l| l.split(':').next()).collect();
    files.dedup();
    let read: usize = files
        .iter()
        .filter_map(|f| fs::read_to_string(h.repo.join(f)).ok())
        .map(|text| {
            text.lines()
                .take(READ_LINE_CAP)
                .map(|l| l.len() + 1)
                .sum::<usize>()
        })
        .sum();
    (hits.len() + read) as f64
}

fn arm_b_bytes(h: &Harness, task: &Task) -> f64 {
    let flag = if task.kind == Kind::References {
        "-w"
    } else {
        "-F"
    };
    rg(h, &["-n", flag, "-C", "30", &task.query]).len() as f64
}

/// Arm d, the plan's agent baseline: the first 200 `rg` hit lines, then 100
/// lines around each of the first 5 matches.
fn arm_d_bytes(h: &Harness, task: &Task) -> f64 {
    let flag = if task.kind == Kind::References {
        "-w"
    } else {
        "-F"
    };
    let hits = rg(h, &["-n", flag, &task.query]);
    let listing: usize = hits.lines().take(200).map(|l| l.len() + 1).sum();
    let windows: usize = hits
        .lines()
        .take(5)
        .filter_map(|l| {
            let mut parts = l.splitn(3, ':');
            let (file, line) = (parts.next()?, parts.next()?.parse::<usize>().ok()?);
            let text = fs::read_to_string(h.repo.join(file)).ok()?;
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

fn arm_c_args(task: &Task) -> Vec<&str> {
    match task.kind {
        Kind::Declaration => vec!["code", "symbol", &task.query, "--json"],
        Kind::References => vec!["code", "refs", &task.query, "--limit", "200", "--json"],
        Kind::Phrase => vec!["code", "search", &task.query, "--text", "--json"],
    }
}

fn arm_c_bytes(h: &Harness, task: &Task) -> f64 {
    h.playbook(&arm_c_args(task)).stdout.len() as f64
}

fn savings(tool: &mut [f64], baseline: &mut [f64]) -> f64 {
    100.0 * (1.0 - median(tool) / median(baseline).max(1.0))
}

fn db_bytes(h: &Harness) -> u64 {
    let doc = h.json(&["code", "status", "--json"]);
    let path = doc["index"]["path"].as_str().map(PathBuf::from);
    path.and_then(|p| fs::metadata(p).ok())
        .map_or(0, |m| m.len())
}

struct Bars {
    latency: bool,
    db_size: bool,
    large: bool,
}

/// `p50s` holds the warm medians of `symbol`, `search --text` and `context`.
fn bars(files: usize, p50s: [f64; 3], index_s: (f64, f64), sizes: (f64, f64)) -> Bars {
    let [symbol, search, context] = p50s;
    let (cold_s, warm_s) = index_s;
    let (db, source) = sizes;
    Bars {
        latency: symbol <= 50.0
            && search <= 100.0
            && context <= 500.0
            && (files >= 50_000 || (cold_s <= 15.0 && warm_s <= 2.0)),
        db_size: db <= 1.5 * source,
        large: files < 50_000 || (cold_s < 180.0 && warm_s < 20.0),
    }
}

fn measure(files: usize) -> Value {
    let h = Harness::new(&format!("bench-{files}"), true);
    let realistic = std::env::var("BENCH_CODE_REALISTIC").is_ok_and(|v| v == "1");
    let repo = generate_with(&h.repo, files, 7, realistic);
    h.init_and_commit();

    let start = Instant::now();
    h.json(&["code", "index", "--json"]);
    let cold_s = start.elapsed().as_secs_f64();
    let warm = timings(&h, &["code", "index", "--json"]);
    let warm_s = percentile(&warm, 0.5) / 1000.0;
    let db = db_bytes(&h) as f64;

    let (mut a, mut b, mut c, mut d): (Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>) =
        (vec![], vec![], vec![], vec![]);
    for task in &repo.tasks {
        a.push(arm_a_bytes(&h, task));
        b.push(arm_b_bytes(&h, task));
        c.push(arm_c_bytes(&h, task));
        d.push(arm_d_bytes(&h, task));
    }
    let r_a = savings(&mut c.clone(), &mut a.clone());
    let r_b = savings(&mut c.clone(), &mut b.clone());
    let r_d = savings(&mut c.clone(), &mut d.clone());
    // The two common-identifier tasks come last in realistic mode.
    let common = if realistic {
        let n = repo.tasks.len() - 2;
        json!({
            "tasks": repo.tasks[n..].iter().map(|t| &t.query).collect::<Vec<_>>(),
            "rg_reads_bytes": &a[n..],
            "rg_context_bytes": &b[n..],
            "rg_window_bytes": &d[n..],
            "index_bytes": &c[n..],
        })
    } else {
        Value::Null
    };

    let lsp = std::env::var("BENCH_CODE_LSP").is_ok_and(|v| v == "1");
    let lsp_entry = lsp.then(|| measure_lsp(&h, &repo, files, [&a, &b, &c, &d]));

    let symbol = timings(&h, &["code", "symbol", &repo.declarations[0].1, "--json"]);
    let search = timings(
        &h,
        &["code", "search", &repo.tasks[2].query, "--text", "--json"],
    );
    let task_words = format!("add support for {}", repo.declarations[1].1);
    let context = timings(&h, &["code", "context", &task_words, "--json"]);

    let (top3, recall) = correctness(&h, &repo);
    let unflagged = stale_without_flag(&h, &repo);
    let ok = bars(
        files,
        [&symbol, &search, &context].map(|t| percentile(t, 0.5)),
        (cold_s, warm_s),
        (db, repo.source_bytes as f64),
    );
    let numbers = Numbers {
        files,
        r_a,
        r_b,
        symbol_top3: top3,
        refs_recall: recall,
        stale_without_flag: unflagged,
        latency_bars_met: ok.latency,
        db_size_bar_met: ok.db_size,
        large_bars_met: ok.large,
    };
    let mut entry = report_entry(
        &numbers,
        &repo,
        &[
            ("symbol", &symbol),
            ("search_text", &search),
            ("context", &context),
        ],
        cold_s,
        warm_s,
        db,
    );
    entry["realistic"] = json!(realistic);
    entry["savings_vs_rg_window_pct"] = json!(r_d);
    entry["common_identifier_tasks"] = common;
    if let Some(lsp_entry) = lsp_entry {
        entry["lsp"] = lsp_entry;
    }
    entry
}

/// Arm e: the same declaration and references tasks answered by real language
/// servers started once per language. Phrase and common-identifier tasks have
/// no server request and are left out; the baselines are compared on the same
/// tasks. Needs the servers on `PATH`; installs nothing.
fn measure_lsp(h: &Harness, repo: &Repo, files: usize, bytes: [&Vec<f64>; 4]) -> Value {
    use code_lsp_arm::{
        declaration, discover, lang_of, prepare_workspace, recall, references, render, start,
    };
    let [a, b, c, d] = bytes;
    prepare_workspace(&h.repo, files);
    let limit = Duration::from_secs(
        std::env::var("BENCH_CODE_LSP_START_S")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(600),
    );
    let supported: Vec<usize> = (0..repo.tasks.len())
        .filter(|&i| repo.tasks[i].kind != Kind::Phrase && lang_of(&repo.tasks[i].query).is_some())
        .collect();
    let (mut bytes_lsp, mut times, mut idx) = (vec![], vec![], vec![]);
    let (mut decl_ok, mut decl_n, mut ref_ok, mut ref_n, mut def_ok, mut def_n) =
        (0, 0, 0, 0, 0, 0);
    let (mut same_ok, mut same_n) = (0, 0);
    let mut servers = vec![];
    for spec in discover() {
        let mine: Vec<usize> = supported
            .iter()
            .copied()
            .filter(|&i| lang_of(&repo.tasks[i].query).is_some_and(|l| spec.exts.contains(&l)))
            .collect();
        // Declarations of this server's languages, for the probe and the
        // spot checks (the fixture's own tasks may all fall on one language).
        let own: Vec<&(String, String)> = repo
            .declarations
            .iter()
            .filter(|(path, _)| {
                std::path::Path::new(path)
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| spec.exts.contains(&e))
            })
            .collect();
        // The probe is the last declaration: a server that has loaded only the
        // opened file would still answer for the first one.
        let (Some((first_file, _)), Some((_, probe_decl))) = (own.first(), own.last()) else {
            continue;
        };
        let (first_file, probe_decl) = (first_file.clone(), probe_decl.clone());
        let Some(mut ready) = start(&spec, &h.repo, &first_file, &probe_decl, limit) else {
            servers
                .push(json!({"server": spec.name, "started": false, "limit_s": limit.as_secs()}));
            continue;
        };
        let mut entry = json!({"server": spec.name, "started": true, "cold_start_s": ready.cold_s, "tasks": mine.len()});
        for i in mine {
            let task = &repo.tasks[i];
            let answer = if task.kind == Kind::Declaration {
                declaration(&mut ready, &h.repo, &task.query)
            } else {
                references(&mut ready, &h.repo, &task.query, 200)
            };
            bytes_lsp.push(render(&answer.places).len() as f64);
            times.push(answer.ms);
            idx.push(i);
            let (hit, total) = recall(&task.expected, &answer.places);
            if task.kind == Kind::Declaration {
                decl_n += 1;
                decl_ok += usize::from(hit == total);
            } else {
                ref_n += total;
                ref_ok += hit;
                // Callers in other languages cannot resolve to this symbol.
                let lang = lang_of(&task.query).unwrap_or_default();
                let same: Vec<(String, usize)> = task
                    .expected
                    .iter()
                    .filter(|(p, _)| p.ends_with(&format!(".{lang}")))
                    .cloned()
                    .collect();
                same_n += same.len();
                same_ok += recall(&same, &answer.places).0;
                if let Some(ok) = answer.definition_ok {
                    def_n += 1;
                    def_ok += usize::from(ok);
                }
            }
        }
        // Five spot checks per server on its own languages, outside the 32 tasks.
        let step = (own.len() / 5).max(1);
        let spots: Vec<_> = own.iter().step_by(step).take(5).collect();
        let (mut ok, mut ms) = (0, vec![]);
        for (path, name) in &spots {
            let answer = declaration(&mut ready, &h.repo, name);
            ok += usize::from(recall(&[(path.clone(), 3)], &answer.places).0 == 1);
            ms.push(answer.ms);
        }
        ms.sort_by(f64::total_cmp);
        entry["spot_checks"] = json!({"n": spots.len(), "hits": ok, "latency": latency_json(&ms)});
        servers.push(entry);
        ready.client.stop();
    }
    let pick = |v: &Vec<f64>| -> Vec<f64> { idx.iter().map(|&i| v[i]).collect() };
    let (mut lsp_b, sa, sb, sc, sd) = (bytes_lsp.clone(), pick(a), pick(b), pick(c), pick(d));
    let pct = |ok: usize, n: usize| 100.0 * ok as f64 / n.max(1) as f64;
    let mut sorted = times.clone();
    sorted.sort_by(f64::total_cmp);
    let colds: Vec<f64> = servers
        .iter()
        .filter_map(|s| s["cold_start_s"].as_f64())
        .collect();
    json!({
        "servers": servers,
        "tasks_answered": idx.len(),
        "tasks_without_server_request": repo.tasks.len() - supported.len(),
        "task_set": "declaration and references with a language prefix",
        "lsp_bytes_median": median(&mut lsp_b),
        "index_bytes_median_same_tasks": median(&mut sc.clone()),
        "savings_vs_rg_and_reads_pct": savings(&mut bytes_lsp.clone(), &mut sa.clone()),
        "savings_vs_rg_context_pct": savings(&mut bytes_lsp.clone(), &mut sb.clone()),
        "savings_vs_rg_window_pct": savings(&mut bytes_lsp.clone(), &mut sd.clone()),
        "index_savings_same_tasks_vs_reads_pct": savings(&mut sc.clone(), &mut sa.clone()),
        "index_savings_same_tasks_vs_window_pct": savings(&mut sc.clone(), &mut sd.clone()),
        "latency": latency_json(&sorted),
        "slowest_cold_start_s": colds.iter().copied().fold(0.0, f64::max),
        "symbol_hit_pct": pct(decl_ok, decl_n),
        "refs_recall_pct": pct(ref_ok, ref_n),
        "refs_expected_in_same_language_pct": pct(same_n, ref_n),
        "refs_recall_same_language_pct": pct(same_ok, same_n),
        "definition_round_trip_pct": pct(def_ok, def_n),
        "bars_informational": {
            "savings_90": savings(&mut bytes_lsp.clone(), &mut sd.clone()) >= 90.0,
            "savings_25_vs_context": savings(&mut bytes_lsp.clone(), &mut sb.clone()) >= 25.0,
            "cold_start_5s": colds.iter().copied().fold(0.0, f64::max) <= 5.0,
        },
    })
}

fn report_entry(
    n: &Numbers,
    repo: &Repo,
    latencies: &[(&str, &Vec<f64>)],
    cold_s: f64,
    warm_s: f64,
    db: f64,
) -> Value {
    let latency: serde_json::Map<String, Value> = latencies
        .iter()
        .map(|(name, times)| ((*name).to_string(), latency_json(times)))
        .collect();
    json!({
        "files": n.files,
        "tasks": repo.tasks.len(),
        "source_bytes": repo.source_bytes,
        "db_bytes": db,
        "cold_index_s": cold_s,
        "warm_index_s": warm_s,
        "latency": latency,
        "savings_vs_rg_and_reads_pct": n.r_a,
        "savings_vs_rg_context_pct": n.r_b,
        "symbol_top3_pct": n.symbol_top3,
        "refs_recall_pct": n.refs_recall,
        "stale_without_flag": n.stale_without_flag,
        "bars": {
            "latency": n.latency_bars_met,
            "db_size": n.db_size_bar_met,
            "large_repo": n.large_bars_met,
        },
        "verdict": verdict(n).as_str(),
    })
}

#[test]
#[ignore = "timing benchmark, run with `just bench-code`"]
fn bench_code() {
    assert!(which("rg").is_some(), "the benchmark needs rg installed");
    let sizes = std::env::var("BENCH_CODE_SIZES").unwrap_or_else(|_| DEFAULT_SIZES.to_string());
    let runs: Vec<Value> = sizes
        .split(',')
        .map(|s| {
            measure(
                s.trim()
                    .parse()
                    .expect("BENCH_CODE_SIZES holds file counts"),
            )
        })
        .collect();
    let report = json!({"runs": runs, "limits": "bytes stand in for tokens; the server arm runs only with BENCH_CODE_LSP=1"});
    let path = std::env::var("BENCH_CODE_REPORT").map_or_else(
        |_| std::env::temp_dir().join("playbook-code-bench.json"),
        PathBuf::from,
    );
    fs::write(&path, serde_json::to_string_pretty(&report).expect("json")).expect("report");
    println!("{}", serde_json::to_string_pretty(&report).expect("json"));
    println!("report written to {}", path.display());
}

#[test]
fn a_report_entry_carries_every_bar_and_a_verdict() {
    // Arrange
    let h = Harness::new("schema", false);
    let repo = generate_with(&h.repo, 20, 1, false);
    let n = Numbers {
        files: 20,
        r_a: 10.0,
        r_b: 5.0,
        symbol_top3: 100.0,
        refs_recall: 100.0,
        stale_without_flag: 0,
        latency_bars_met: true,
        db_size_bar_met: true,
        large_bars_met: true,
    };
    let times = vec![1.0, 2.0, 3.0];

    // Act
    let entry = report_entry(&n, &repo, &[("symbol", &times)], 1.5, 0.1, 1000.0);

    // Assert
    for key in [
        "files",
        "tasks",
        "source_bytes",
        "db_bytes",
        "cold_index_s",
        "warm_index_s",
        "latency",
        "savings_vs_rg_and_reads_pct",
        "savings_vs_rg_context_pct",
        "symbol_top3_pct",
        "refs_recall_pct",
        "stale_without_flag",
        "bars",
        "verdict",
    ] {
        assert!(entry.get(key).is_some(), "{key} is missing");
    }
    assert_eq!(entry["verdict"], "partial");
    assert_eq!(entry["latency"]["symbol"]["p50_ms"], 2.0);
}
