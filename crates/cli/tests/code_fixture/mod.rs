// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A deterministic synthetic repo with ground truth, a runner for the built
//! binary, and the pure `verdict` that turns measured numbers into kill,
//! partial or pass. Shared by the correctness test and the timing harness.

#![allow(
    dead_code,
    unreachable_pub,
    reason = "each test crate uses a different part of this shared module"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

/// What a task asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Declaration,
    References,
    Phrase,
}

/// One question with its known answer: `(path, line)` pairs.
#[derive(Debug, Clone)]
pub struct Task {
    pub kind: Kind,
    pub query: String,
    pub expected: Vec<(String, usize)>,
}

pub struct Repo {
    pub root: PathBuf,
    pub tasks: Vec<Task>,
    /// `(path, declared name)` of every file's first declaration.
    pub declarations: Vec<(String, String)>,
    pub source_bytes: u64,
}

const LANGS: [&str; 4] = ["rs", "ts", "js", "py"];
const DECLS_PER_FILE: usize = 3;
const TASKS_PER_KIND: usize = 10;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn path_of(file: usize) -> String {
    let ext = LANGS[file % LANGS.len()];
    format!("src/m{}/f{file}.{ext}", file % 20)
}

fn decl_name(file: usize, k: usize) -> String {
    format!("{}_fn_{file}_{k}", LANGS[file % LANGS.len()])
}

/// The lines of function `name` calling `callee`; the call is on line 2.
fn function(ext: &str, name: &str, callee: &str) -> [String; 3] {
    match ext {
        "rs" => [
            format!("pub fn {name}() -> u32 {{"),
            format!("    {callee}() + 1"),
            "}".to_string(),
        ],
        "ts" => [
            format!("export function {name}(): number {{"),
            format!("  return {callee}() + 1;"),
            "}".to_string(),
        ],
        "js" => [
            format!("export function {name}() {{"),
            format!("  return {callee}() + 1;"),
            "}".to_string(),
        ],
        _ => [
            format!("def {name}():"),
            format!("    return {callee}() + 1"),
            String::new(),
        ],
    }
}

/// Number of filler helpers per file in realistic mode (about 5 KB a file).
const FILLER_FNS: usize = 14;

/// Identifiers that appear in every realistic file, so `rg` returns many hits.
pub const COMMON_CALL: &str = "parse_config";
pub const COMMON_PHRASE: &str = "validate the input before use";

/// Filler helpers that call common identifiers, in the file's language.
fn filler(ext: &str, file: usize, i: usize) -> Vec<String> {
    let name = format!("helper_{file}_{i}");
    let note = format!("{COMMON_PHRASE} and log the result of step {i}");
    let (head, tail, comment) = match ext {
        "rs" => (
            format!("fn {name}(input: &str) -> Result<usize, Error> {{"),
            "}",
            "//",
        ),
        "ts" => (
            format!("function {name}(input: string): number {{"),
            "}",
            "//",
        ),
        "js" => (format!("function {name}(input) {{"), "}", "//"),
        _ => (format!("def {name}(input):"), "", "#"),
    };
    let semi = if ext == "py" { "" } else { ";" };
    let ind = "    ";
    let kw = match ext {
        "rs" => "let",
        "ts" | "js" => "const",
        _ => "",
    };
    let bind = |var: &str, expr: &str| {
        if kw.is_empty() {
            format!("{ind}{var} = {expr}{semi}")
        } else {
            format!("{ind}{kw} {var} = {expr}{semi}")
        }
    };
    let mut out = vec![head, format!("{ind}{comment} {note}")];
    out.push(bind("config", &format!("{COMMON_CALL}(input)")));
    out.push(bind("request", "handle_request(config, input)"));
    out.push(bind("checked", "validate_input(request)"));
    out.push(bind("result", "render_view(checked, config.value)"));
    out.push(format!(
        "{ind}{comment} fall back to the default value when the result is empty"
    ));
    out.push(bind("total", &format!("result.length + {i}")));
    out.push(format!("{ind}return total{semi}"));
    if !tail.is_empty() {
        out.push(tail.to_string());
    }
    out.push(String::new());
    out
}

/// Writes `files` source files under `root` and returns the ground truth.
pub fn generate(root: &Path, files: usize, seed: u64) -> Repo {
    generate_with(root, files, seed, false)
}

/// Like `generate`; with `realistic` every file also holds filler helpers
/// that repeat common identifiers, and two extra tasks query those.
pub fn generate_with(root: &Path, files: usize, seed: u64, realistic: bool) -> Repo {
    let mut rng = Rng(seed | 1);
    let total = files * DECLS_PER_FILE;
    let comment = |ext: &str, text: &str| match ext {
        "py" => format!("# {text}"),
        _ => format!("// {text}"),
    };
    let mut calls: Vec<Vec<(String, usize)>> = vec![Vec::new(); total];
    let mut declared: Vec<(String, usize)> = Vec::new();
    let mut phrases: Vec<(String, String)> = Vec::new();
    let mut bytes = 0;
    for file in 0..files {
        let ext = LANGS[file % LANGS.len()];
        let phrase = format!("zq phrase {file} {}", rng.next() % 100_000);
        let mut lines = vec![comment(ext, &phrase), String::new()];
        for k in 0..DECLS_PER_FILE {
            let me = file * DECLS_PER_FILE + k;
            // The first function calls its neighbour, so every task has a caller.
            let callee_index = if k == 0 {
                (me + 1) % total
            } else {
                (me * 7 + 1 + (rng.next() as usize % total)) % total
            };
            let callee = decl_name(callee_index / DECLS_PER_FILE, callee_index % DECLS_PER_FILE);
            let first = lines.len() + 1;
            lines.extend(function(ext, &decl_name(file, k), &callee));
            lines.push(String::new());
            if k == 0 {
                declared.push((path_of(file), first));
            }
            calls[callee_index].push((path_of(file), first + 1));
        }
        if realistic {
            for i in 0..FILLER_FNS {
                lines.extend(filler(ext, file, i));
            }
        }
        phrases.push((path_of(file), phrase));
        let path = root.join(path_of(file));
        fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        let text = lines.join("\n") + "\n";
        bytes += text.len() as u64;
        fs::write(path, text).expect("write source");
    }
    let step = (files / TASKS_PER_KIND).max(1);
    let pick = |n: usize| (n * step) % files;
    let mut tasks = Vec::new();
    for n in 0..TASKS_PER_KIND {
        let file = pick(n);
        tasks.push(Task {
            kind: Kind::Declaration,
            query: decl_name(file, 0),
            expected: vec![declared[file].clone()],
        });
        let callee_index = pick(n) * DECLS_PER_FILE + 1;
        tasks.push(Task {
            kind: Kind::References,
            query: decl_name(callee_index / DECLS_PER_FILE, callee_index % DECLS_PER_FILE),
            expected: calls[callee_index].clone(),
        });
        tasks.push(Task {
            kind: Kind::Phrase,
            query: phrases[file].1.clone(),
            expected: vec![(phrases[file].0.clone(), 1)],
        });
    }
    if realistic {
        tasks.push(Task {
            kind: Kind::References,
            query: COMMON_CALL.to_string(),
            expected: Vec::new(),
        });
        tasks.push(Task {
            kind: Kind::Phrase,
            query: COMMON_PHRASE.to_string(),
            expected: Vec::new(),
        });
    }
    let declarations = (0..files)
        .map(|file| (path_of(file), decl_name(file, 0)))
        .collect();
    Repo {
        root: root.to_path_buf(),
        tasks,
        declarations,
        source_bytes: bytes,
    }
}

/// Runs the built binary and git in a scratch home with a minimal `PATH`.
pub struct Harness {
    pub repo: PathBuf,
    pub home: PathBuf,
    pub bin: PathBuf,
}

pub fn which(program: &str) -> Option<PathBuf> {
    let dirs = ["/usr/bin", "/opt/homebrew/bin", "/usr/local/bin", "/bin"];
    dirs.iter()
        .map(|d| Path::new(d).join(program))
        .find(|p| p.is_file())
}

impl Harness {
    /// A scratch area with an empty repo, its own `HOME`, and `git` (and, if
    /// asked, `rg`) linked into a `PATH` directory.
    pub fn new(tag: &str, with_rg: bool) -> Self {
        let base = std::env::temp_dir().join(format!(
            "playbook-code-fixture-{tag}-{}",
            playbook::testing::run_id()
        ));
        let _ = fs::remove_dir_all(&base);
        let (repo, home, bin) = (base.join("repo"), base.join("home"), base.join("bin"));
        for dir in [&repo, &home, &bin] {
            fs::create_dir_all(dir).expect("scratch dir");
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(which("git").expect("git"), bin.join("git")).expect("link");
            if with_rg {
                std::os::unix::fs::symlink(which("rg").expect("rg on PATH"), bin.join("rg"))
                    .expect("link");
            }
        }
        Self {
            repo: repo.canonicalize().expect("repo"),
            home,
            bin,
        }
    }

    pub fn git(&self, args: &[&str]) {
        let status = Command::new(which("git").expect("git"))
            .arg("-C")
            .arg(&self.repo)
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
            ])
            .args(args)
            .env_clear()
            .env("HOME", &self.home)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?}");
    }

    /// `git init`, a fake `origin`, and everything committed.
    pub fn init_and_commit(&self) {
        self.git(&["init", "--quiet", "-b", "main"]);
        self.git(&[
            "remote",
            "add",
            "origin",
            "https://github.com/test-owner/test-repo.git",
        ]);
        self.git(&["add", "-A"]);
        self.git(&["commit", "--quiet", "-m", "fixture"]);
    }

    pub fn playbook(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(args)
            .current_dir(&self.repo)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", &self.bin)
            .output()
            .expect("playbook runs")
    }

    pub fn json(&self, args: &[&str]) -> serde_json::Value {
        let out = self.playbook(args);
        let text = String::from_utf8_lossy(&out.stdout);
        serde_json::from_str(text.trim()).unwrap_or_else(|e| {
            panic!(
                "{args:?} printed no JSON ({e}): {text} {}",
                String::from_utf8_lossy(&out.stderr)
            )
        })
    }
}

/// The measured numbers a verdict is computed from.
#[derive(Debug, Clone)]
pub struct Numbers {
    /// Fixture size the savings numbers were measured at.
    pub files: usize,
    /// Output bytes saved against `rg` plus whole-file reads, in percent.
    pub r_a: f64,
    /// Output bytes saved against `rg -C 30`, in percent.
    pub r_b: f64,
    pub symbol_top3: f64,
    pub refs_recall: f64,
    /// Results that pointed at changed or removed content with no stale flag.
    pub stale_without_flag: usize,
    pub latency_bars_met: bool,
    pub db_size_bar_met: bool,
    pub large_bars_met: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Kill,
    Partial,
    Pass,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Kill => "kill",
            Self::Partial => "partial",
            Self::Pass => "pass",
        }
    }
}

/// Kill when a correctness bar is missed, or when savings against `rg` plus
/// reads are under 50% on a 5,000 file repo. Pass when every bar is met.
/// Otherwise partial.
pub fn verdict(n: &Numbers) -> Verdict {
    let correctness_missed =
        n.stale_without_flag > 0 || n.symbol_top3 < 95.0 || n.refs_recall < 90.0;
    if correctness_missed || (n.files >= 5_000 && n.r_a < 50.0) {
        return Verdict::Kill;
    }
    let all_met = n.r_a >= 75.0
        && n.r_b >= 40.0
        && n.latency_bars_met
        && n.db_size_bar_met
        && n.large_bars_met;
    if all_met {
        Verdict::Pass
    } else {
        Verdict::Partial
    }
}

fn results(doc: &Value) -> &[Value] {
    doc["results"].as_array().map_or(&[], Vec::as_slice)
}

fn hit(result: &Value, path: &str, line: usize) -> bool {
    let first = result["start_line"]
        .as_u64()
        .or_else(|| result["line"].as_u64());
    result["path"] == path && first == Some(line as u64)
}

/// Runs the index, then answers every task. Returns the share of declaration
/// tasks with the right declaration in the top 3 results and the share of
/// call sites that `refs` found, both in percent.
pub fn correctness(h: &Harness, repo: &Repo) -> (f64, f64) {
    h.json(&["code", "index", "--json"]);
    let (mut decl_total, mut decl_ok, mut ref_total, mut ref_ok) = (0, 0, 0, 0);
    for task in &repo.tasks {
        match task.kind {
            Kind::Declaration => {
                let doc = h.json(&["code", "symbol", &task.query, "--json"]);
                let (path, line) = &task.expected[0];
                decl_total += 1;
                decl_ok += usize::from(results(&doc).iter().take(3).any(|r| hit(r, path, *line)));
            }
            Kind::References => {
                let doc = h.json(&["code", "refs", &task.query, "--limit", "200", "--json"]);
                for (path, line) in &task.expected {
                    ref_total += 1;
                    ref_ok += usize::from(results(&doc).iter().any(|r| hit(r, path, *line)));
                }
            }
            Kind::Phrase => {}
        }
    }
    let pct = |ok: usize, total: usize| 100.0 * ok as f64 / total.max(1) as f64;
    (pct(decl_ok, decl_total), pct(ref_ok, ref_total))
}

/// Applies edit, rename, delete and branch switch one at a time, with no
/// reindex in between, and counts the answers whose `index.state` is not
/// `stale` even though the files changed.
pub fn stale_without_flag(h: &Harness, repo: &Repo) -> usize {
    h.json(&["code", "index", "--json"]);
    let [(edit, edit_name), (moved, moved_name), (gone, gone_name), (branch, branch_name)] =
        [3, 7, 11, 15].map(|n| repo.declarations[n % repo.declarations.len()].clone());
    type Step<'a> = (&'a str, Box<dyn Fn() + 'a>, &'a str);
    let steps: [Step; 4] = [
        ("edit", Box::new(|| append(h, &edit)), &edit_name),
        (
            "rename",
            Box::new(|| {
                let target = format!("{moved}.moved");
                h.git(&["mv", &moved, &target]);
            }),
            &moved_name,
        ),
        (
            "delete",
            Box::new(|| fs::remove_file(h.repo.join(&gone)).expect("delete")),
            &gone_name,
        ),
        (
            "branch switch",
            Box::new(|| {
                h.git(&["checkout", "--quiet", "-b", "other"]);
                append(h, &branch);
                h.git(&["commit", "--quiet", "-am", "other"]);
                h.git(&["checkout", "--quiet", "main"]);
                h.git(&["checkout", "--quiet", "other"]);
            }),
            &branch_name,
        ),
    ];
    let mut unflagged = 0;
    for (step, apply, name) in steps {
        apply();
        for args in [
            ["code", "symbol", name, "--json"],
            ["code", "refs", name, "--json"],
            ["code", "search", name, "--json"],
        ] {
            let state = h.json(&args)["index"]["state"].as_str().map(str::to_owned);
            if state.as_deref() != Some("stale") {
                eprintln!("{step}: {args:?} answered with index state {state:?}");
                unflagged += 1;
            }
        }
    }
    unflagged
}

fn append(h: &Harness, path: &str) {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(h.repo.join(path))
        .expect("open");
    writeln!(file, "// edited").expect("append");
}
