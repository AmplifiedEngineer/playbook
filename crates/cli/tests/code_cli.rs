// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Binary tests for `playbook code`. Each test gets a scratch git repo with a
//! fake `origin`, a scratch `HOME` and a scratch `PATH` that holds only git.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    repo: PathBuf,
    home: PathBuf,
    bin: PathBuf,
}

fn which_git() -> PathBuf {
    for dir in ["/usr/bin", "/opt/homebrew/bin", "/usr/local/bin", "/bin"] {
        let candidate = Path::new(dir).join("git");
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from("git")
}

impl Fixture {
    fn new(tag: &str, with_origin: bool) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!(
            "playbook-code-cli-{tag}-{}-{n}",
            playbook::testing::run_id()
        ));
        let repo = base.join("repo");
        let home = base.join("home");
        let bin = base.join("bin");
        for dir in [&repo, &home, &bin] {
            fs::create_dir_all(dir).expect("scratch dir");
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(which_git(), bin.join("git")).expect("git link");
        let fx = Self {
            repo: repo.canonicalize().expect("repo"),
            home,
            bin,
        };
        fx.git(&["init", "--quiet"]);
        if with_origin {
            fx.git(&[
                "remote",
                "add",
                "origin",
                "https://github.com/test-owner/test-repo.git",
            ]);
        }
        fx
    }

    fn git(&self, args: &[&str]) {
        let status = Command::new(which_git())
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env_clear()
            .env("HOME", &self.home)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?}");
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.repo.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        fs::write(path, text).expect("write");
    }

    /// A second worktree of this repo, in its own folder and with the same
    /// home. A repo needs one commit before it can have a second worktree.
    fn add_worktree(&self, name: &str) -> Self {
        self.git(&[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "start",
        ]);
        let path = self.repo.parent().expect("base").join(name);
        self.git(&[
            "worktree",
            "add",
            "--quiet",
            "-b",
            name,
            path.to_str().expect("utf8"),
        ]);
        Self {
            repo: path.canonicalize().expect("worktree"),
            home: self.home.clone(),
            bin: self.bin.clone(),
        }
    }

    fn playbook(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_playbook"))
            .args(args)
            .current_dir(&self.repo)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", &self.bin)
            .output()
            .expect("playbook runs")
    }
}

fn json_of(out: &Output) -> serde_json::Value {
    let text = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("not one JSON document ({e}): {text}"))
}

#[test]
fn index_prints_the_envelope_and_a_second_run_is_all_unchanged() {
    // Arrange
    let fx = Fixture::new("envelope", true);
    fx.write("a.rs", "fn a() {}\n");
    fx.write("b.ts", "export const b = 1;\n");
    fx.write("c.py", "def c():\n    pass\n");

    // Act
    let first = fx.playbook(&["code", "index", "--json"]);
    let second = fx.playbook(&["code", "index", "--json"]);
    let rebuilt = fx.playbook(&["code", "index", "--json", "--rebuild"]);

    // Assert
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first = json_of(&first);
    assert_eq!(first["version"], 1);
    assert_eq!(first["command"], "index");
    assert_eq!(first["results"]["indexed"], 3);
    assert_eq!(first["results"]["changed"], 3);
    assert_eq!(first["results"]["reused"], 0);
    let second = json_of(&second);
    assert_eq!(second["results"]["unchanged"], 3);
    assert_eq!(second["results"]["changed"], 0);
    let rebuilt = json_of(&rebuilt);
    assert_eq!(rebuilt["results"]["indexed"], 3);
    assert_eq!(rebuilt["results"]["changed"], 3);
}

#[test]
fn index_in_text_mode_prints_a_one_line_summary() {
    // Arrange
    let fx = Fixture::new("text", true);
    fx.write("a.rs", "fn a() {}\n");

    // Act
    let out = fx.playbook(&["code", "index"]);

    // Assert
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(text.starts_with("indexed 1 files: 1 changed"), "{text}");
}

#[test]
fn a_repo_without_origin_exits_one_and_names_the_missing_remote() {
    // Arrange
    let fx = Fixture::new("no-origin", false);
    fx.write("a.rs", "fn a() {}\n");

    // Act
    let out = fx.playbook(&["code", "index"]);

    // Assert
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(err.contains("origin"), "{err}");
}

#[test]
fn the_index_lands_under_the_home_state_dir_and_not_in_the_repo() {
    // Arrange
    let fx = Fixture::new("location", true);
    fx.write("a.rs", "fn a() {}\n");

    // Act
    let out = fx.playbook(&["code", "index", "--json"]);

    // Assert
    let doc = json_of(&out);
    let path = doc["index"]["path"].as_str().expect("path").to_string();
    assert!(
        path.starts_with(fx.home.to_str().expect("utf8 home")),
        "{path}"
    );
    assert!(path.contains("test-owner/test-repo"), "{path}");
    assert!(path.contains("/data/code/"), "{path}");
    assert!(path.ends_with("/store-v4.sqlite3"), "{path}");
    assert!(Path::new(&path).is_file());
    let listing = fs::read_dir(&fx.repo)
        .expect("repo")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(!listing.iter().any(|n| n == ".playbook"), "{listing:?}");
}

#[test]
fn status_without_an_index_reports_missing_and_exits_zero() {
    // Arrange
    let fx = Fixture::new("status-missing", true);
    fx.write("a.rs", "fn a() {}\n");

    // Act
    let out = fx.playbook(&["code", "status", "--json"]);

    // Assert
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc = json_of(&out);
    assert_eq!(doc["command"], "status");
    assert_eq!(doc["index"]["state"], "missing");
}

#[test]
fn status_is_fresh_after_indexing_and_stale_after_an_added_file() {
    // Arrange
    let fx = Fixture::new("status-stale", true);
    fx.write("a.rs", "fn a() {}\n");
    assert!(fx.playbook(&["code", "index"]).status.success());

    // Act
    let fresh = json_of(&fx.playbook(&["code", "status", "--json"]));
    fx.write("new.rs", "fn n() {}\n");
    let stale = json_of(&fx.playbook(&["code", "status", "--json"]));

    // Assert
    assert_eq!(fresh["index"]["state"], "fresh");
    assert_eq!(fresh["results"]["interrupted"], false);
    assert_eq!(stale["index"]["state"], "stale");
    assert_eq!(stale["results"]["added"]["paths"][0], "new.rs");
    assert!(!stale["warnings"].as_array().expect("warnings").is_empty());
}

#[test]
fn search_after_an_added_file_warns_on_stderr_in_text_mode() {
    // Arrange
    let fx = Fixture::new("search-text-warn", true);
    fx.write("a.txt", "alpha\nbeta\nneedleword here\n");
    assert!(fx.playbook(&["code", "index"]).status.success());
    fx.write("new.txt", "other\n");

    // Act
    let out = fx.playbook(&["code", "search", "--text", "needleword"]);

    // Assert
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(stdout.starts_with("a.txt:1-3"), "{stdout}");
    assert!(stdout.contains("needleword here"), "{stdout}");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(stderr.contains("stale"), "{stderr}");
}

#[test]
fn search_after_an_added_file_puts_the_warning_in_the_json_document() {
    // Arrange
    let fx = Fixture::new("search-json-warn", true);
    fx.write("a.txt", "needleword here\n");
    assert!(fx.playbook(&["code", "index"]).status.success());
    fx.write("new.txt", "other\n");

    // Act
    let out = fx.playbook(&["code", "search", "--text", "needleword", "--json"]);

    // Assert
    let doc = json_of(&out);
    assert_eq!(doc["index"]["state"], "stale");
    assert_eq!(doc["results"][0]["path"], "a.txt");
    assert_eq!(doc["results"][0]["start_line"], 1);
    assert!(!doc["warnings"].as_array().expect("warnings").is_empty());
}

#[test]
fn search_without_an_index_exits_one_and_says_to_index_first() {
    // Arrange
    let fx = Fixture::new("search-missing", true);

    // Act
    let out = fx.playbook(&["code", "search", "--text", "x"]);

    // Assert
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(err.contains("no index yet"), "{err}");
}

#[test]
fn symbol_finds_a_declaration_and_prints_a_stable_json_document() {
    // Arrange
    let fx = Fixture::new("symbol-json", true);
    fx.write(
        "lib/pay.py",
        "class Pay:\n    def run(self):\n        pass\n",
    );
    assert!(fx.playbook(&["code", "index"]).status.success());

    // Act
    let first = fx.playbook(&["code", "symbol", "Pay", "--json"]);
    let second = fx.playbook(&["code", "symbol", "Pay", "--json"]);

    // Assert
    assert!(first.status.success());
    assert_eq!(first.stdout, second.stdout);
    let doc = json_of(&first);
    assert_eq!(doc["command"], "symbol");
    assert_eq!(doc["results"][0]["qualified_name"], "Pay");
    assert_eq!(doc["results"][0]["kind"], "class");
    assert_eq!(doc["results"][0]["lang"], "python");
    assert_eq!(doc["results"][0]["tier"], 1);
}

#[test]
fn symbol_text_output_and_filters_work_end_to_end() {
    // Arrange
    let fx = Fixture::new("symbol-text", true);
    fx.write("a.py", "def target():\n    pass\n");
    fx.write("b.ts", "function target() {}\n");
    assert!(fx.playbook(&["code", "index"]).status.success());

    // Act
    let out = fx.playbook(&["code", "symbol", "target", "--lang", "python"]);

    // Assert
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(stdout.starts_with("a.py:1-2"), "{stdout}");
    assert!(!stdout.contains("b.ts"), "{stdout}");
}

#[test]
fn ranked_search_without_the_text_flag_finds_symbols() {
    // Arrange
    let fx = Fixture::new("ranked", true);
    fx.write("a.rs", "fn needle_fn() {}\n");
    assert!(fx.playbook(&["code", "index"]).status.success());

    // Act
    let out = fx.playbook(&["code", "search", "needle_fn", "--json"]);

    // Assert
    let doc = json_of(&out);
    assert_eq!(doc["results"][0]["match_type"], "symbol");
    assert_eq!(doc["results"][0]["path"], "a.rs");
}

#[test]
fn limits_of_zero_and_two_hundred_and_one_are_argument_errors() {
    // Arrange
    let fx = Fixture::new("limits", true);
    fx.write("a.rs", "fn a() {}\n");
    assert!(fx.playbook(&["code", "index"]).status.success());

    // Act, Assert
    for bad in ["0", "201"] {
        let out = fx.playbook(&["code", "symbol", "a", "--limit", bad]);
        assert!(!out.status.success(), "limit {bad}");
    }
    for good in ["1", "200"] {
        let out = fx.playbook(&["code", "symbol", "a", "--limit", good]);
        assert!(out.status.success(), "limit {good}");
    }
}

#[test]
fn symbol_without_an_index_exits_one_and_says_to_index_first() {
    // Arrange
    let fx = Fixture::new("symbol-missing", true);

    // Act
    let out = fx.playbook(&["code", "symbol", "x"]);

    // Assert
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(err.contains("no index yet"), "{err}");
}

#[test]
fn outline_works_without_an_index_and_a_text_file_exits_one() {
    // Arrange
    let fx = Fixture::new("outline", true);
    fx.write("a.py", "class A:\n    def run(self):\n        pass\n");
    fx.write("b.txt", "hello\n");

    // Act
    let ok = fx.playbook(&["code", "outline", "a.py"]);
    let bad = fx.playbook(&["code", "outline", "b.txt"]);

    // Assert
    let stdout = String::from_utf8_lossy(&ok.stdout).to_string();
    assert!(ok.status.success());
    assert!(stdout.contains("class:1-3"), "{stdout}");
    assert!(stdout.contains("  method:2-3"), "{stdout}");
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("supported:"));
}

#[test]
fn read_prints_numbered_lines_and_rejects_bad_ranges_and_outside_paths() {
    // Arrange
    let fx = Fixture::new("read", true);
    fx.write("a.txt", "one\ntwo\nthree\n");

    // Act
    let ok = fx.playbook(&["code", "read", "a.txt", "2:3"]);
    let json = fx.playbook(&["code", "read", "a.txt", "1:1", "--json"]);
    let reversed = fx.playbook(&["code", "read", "a.txt", "3:2"]);
    let outside = fx.playbook(&["code", "read", "../outside", "1:1"]);

    // Assert
    let stdout = String::from_utf8_lossy(&ok.stdout).to_string();
    assert!(stdout.contains("     2  two"), "{stdout}");
    assert!(stdout.contains("     3  three"), "{stdout}");
    let doc = json_of(&json);
    assert_eq!(doc["results"]["text"], "one");
    assert_eq!(doc["index"]["state"], "not_used");
    assert!(!reversed.status.success());
    assert_eq!(outside.status.code(), Some(1));
}

#[test]
fn refs_lists_labelled_candidates_in_json_and_text() {
    // Arrange
    let fx = Fixture::new("refs", true);
    fx.write("a.py", "def target():\n    pass\ntarget()\n# target\n");
    assert!(fx.playbook(&["code", "index"]).status.success());

    // Act
    let json = fx.playbook(&["code", "refs", "target", "--json"]);
    let text = fx.playbook(&["code", "refs", "target"]);

    // Assert
    let doc = json_of(&json);
    assert_eq!(doc["command"], "refs");
    assert_eq!(doc["results"][0]["role"], "declaration");
    assert_eq!(doc["results"][1]["line"], 3);
    assert_eq!(doc["results"][1]["confidence"], "candidate");
    assert_eq!(doc["stats"]["files_scanned"], 1);
    let stdout = String::from_utf8_lossy(&text.stdout).to_string();
    assert!(
        stdout.starts_with("a.py:1:5  declaration (candidate)"),
        "{stdout}"
    );
}

#[test]
fn context_prints_snippets_and_a_stable_json_document() {
    // Arrange
    let fx = Fixture::new("context", true);
    fx.write(
        "pay.ts",
        "export function processPayment() {\n  return 1;\n}\n",
    );
    assert!(fx.playbook(&["code", "index"]).status.success());

    // Act
    let first = fx.playbook(&["code", "context", "fix the payment", "--json"]);
    let second = fx.playbook(&["code", "context", "fix the payment", "--json"]);
    let text = fx.playbook(&["code", "context", "fix the payment"]);
    let small = fx.playbook(&["code", "context", "payment", "--max-tokens", "199"]);

    // Assert
    assert_eq!(first.stdout, second.stdout);
    let doc = json_of(&first);
    assert_eq!(doc["command"], "context");
    assert_eq!(doc["results"]["snippets"][0]["path"], "pay.ts");
    assert!(String::from_utf8_lossy(&text.stdout).contains("== pay.ts:1-3"));
    assert!(!small.status.success());
}

const BUDGET_FILES: usize = 60;

fn budget_fixture(name: &str) -> Fixture {
    let fx = Fixture::new(name, true);
    for i in 0..BUDGET_FILES {
        fx.write(
            &format!("src/m{i}.ts"),
            &format!(
                "export function processPayment{i}() {{\n{}}}\n",
                "  charge();\n".repeat(30)
            ),
        );
    }
    assert!(fx.playbook(&["code", "index"]).status.success());
    fx
}

#[test]
fn context_text_output_fits_a_small_token_budget_as_a_whole() {
    // Arrange
    let fx = budget_fixture("ctx-small-budget");

    // Act
    let out = fx.playbook(&["code", "context", "payment", "--max-tokens", "200"]);

    // Assert
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(text.contains("== "), "{text}");
    assert!(out.stdout.len() <= 800, "{}", out.stdout.len());
}

#[test]
fn context_text_output_fits_the_default_budget_and_lists_what_was_left_out() {
    // Arrange
    let fx = budget_fixture("ctx-default-budget");

    // Act
    let out = fx.playbook(&["code", "context", "payment"]);

    // Assert
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.stdout.len() <= 16_000, "{}", out.stdout.len());
    assert!(text.contains("more candidates did not fit"), "{text}");
}

#[test]
fn context_json_bytes_count_snippet_text_only() {
    // Arrange
    let fx = budget_fixture("ctx-json-bytes");

    // Act
    let doc = json_of(&fx.playbook(&["code", "context", "payment", "--json"]));

    // Assert
    let snippets = doc["results"]["snippets"].as_array().expect("snippets");
    let text_bytes: usize = snippets
        .iter()
        .map(|s| s["text"].as_str().expect("text").len())
        .sum();
    assert_eq!(doc["results"]["bytes"].as_u64(), Some(text_bytes as u64));
}

fn store_dir(doc: &serde_json::Value) -> PathBuf {
    PathBuf::from(doc["index"]["path"].as_str().expect("path"))
        .parent()
        .expect("store folder")
        .to_path_buf()
}

fn db_files(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .expect("store folder")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".db"))
        .collect()
}

fn run_lock_of_the_worktree(dir: &Path) -> PathBuf {
    fs::read_dir(dir)
        .expect("store folder")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.extension().is_some_and(|x| x == "lock")
                && p.file_stem().is_some_and(|n| n != "store-v4")
        })
        .expect("a run lock")
}

#[test]
fn two_worktrees_share_one_store_and_see_only_their_own_files() {
    // Arrange
    let a = Fixture::new("share", true);
    a.write("only_a.rs", "fn only_a() {}\n");
    let b = a.add_worktree("share-b");
    b.write("only_b.rs", "fn only_b() {}\n");

    // Act
    let in_a = json_of(&a.playbook(&["code", "index", "--json"]));
    let in_b = json_of(&b.playbook(&["code", "index", "--json"]));
    let miss = b.playbook(&["code", "symbol", "only_a", "--json"]);
    let hit = b.playbook(&["code", "symbol", "only_b", "--json"]);

    // Assert
    assert_eq!(in_a["index"]["path"], in_b["index"]["path"]);
    assert!(db_files(&store_dir(&in_a)).is_empty());
    assert_eq!(json_of(&miss)["results"].as_array().map(Vec::len), Some(0));
    assert_eq!(json_of(&hit)["results"].as_array().map(Vec::len), Some(1));
}

#[test]
fn a_new_worktree_of_an_indexed_repo_has_no_index_yet() {
    // Arrange
    let a = Fixture::new("fresh-worktree", true);
    a.write("a.rs", "fn a() {}\n");
    let b = a.add_worktree("fresh-worktree-b");
    json_of(&a.playbook(&["code", "index", "--json"]));

    // Act
    let status = b.playbook(&["code", "status", "--json"]);
    let symbol = b.playbook(&["code", "symbol", "a"]);
    let search = b.playbook(&["code", "search", "--text", "a"]);
    let context = b.playbook(&["code", "context", "a"]);

    // Assert
    assert!(status.status.success());
    assert_eq!(json_of(&status)["index"]["state"], "missing");
    for out in [symbol, search, context] {
        assert_eq!(out.status.code(), Some(1));
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(err.contains("code index"), "{err}");
    }
}

#[test]
fn the_run_lock_is_per_worktree() {
    // Arrange
    let a = Fixture::new("lock-per-worktree", true);
    a.write("a.rs", "fn a() {}\n");
    let b = a.add_worktree("lock-per-worktree-b");
    b.write("b.rs", "fn b() {}\n");
    let first = json_of(&a.playbook(&["code", "index", "--json"]));
    let lock = fs::OpenOptions::new()
        .write(true)
        .open(run_lock_of_the_worktree(&store_dir(&first)))
        .expect("lock file");
    lock.lock().expect("hold a's lock");

    // Act
    let blocked = a.playbook(&["code", "index"]);
    let sibling = b.playbook(&["code", "index"]);

    // Assert
    assert_eq!(blocked.status.code(), Some(1));
    let err = String::from_utf8_lossy(&blocked.stderr).into_owned();
    assert!(err.contains("another index run is in progress"), "{err}");
    assert!(
        sibling.status.success(),
        "{}",
        String::from_utf8_lossy(&sibling.stderr)
    );
}

#[test]
fn a_rebuild_of_a_healthy_store_says_worktree_only() {
    // Arrange
    let a = Fixture::new("rebuild-scope", true);
    a.write("a.rs", "fn a() {}\n");
    let b = a.add_worktree("rebuild-scope-b");
    b.write("b.rs", "fn b() {}\n");
    a.playbook(&["code", "index"]);
    b.playbook(&["code", "index"]);
    let before = b.playbook(&["code", "symbol", "b", "--json"]).stdout;

    // Act
    let json = json_of(&a.playbook(&["code", "index", "--rebuild", "--json"]));
    let text = a.playbook(&["code", "index", "--rebuild"]);

    // Assert
    assert_eq!(json["results"]["scope"], "worktree");
    assert!(String::from_utf8_lossy(&text.stdout).contains("rebuilt: worktree only"));
    assert_eq!(
        b.playbook(&["code", "symbol", "b", "--json"]).stdout,
        before
    );
}

#[test]
fn an_index_kept_by_an_earlier_version_is_ignored() {
    // Arrange
    let fx = Fixture::new("old-index", true);
    fx.write("a.rs", "fn a() {}\n");
    let probe = json_of(&fx.playbook(&["code", "index", "--json"]));
    let store = PathBuf::from(probe["index"]["path"].as_str().expect("path"));
    let old = store_dir(&probe).join("old-worktree.db");
    fs::write(&old, vec![b'x'; 4096]).expect("junk");

    // Act
    let out = fx.playbook(&["code", "index", "--json"]);

    // Assert
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(store.is_file());
    assert_eq!(fs::read(&old).expect("old file stays"), vec![b'x'; 4096]);
}

#[test]
fn status_reports_the_same_store_size_in_every_worktree() {
    // Arrange
    let a = Fixture::new("db-bytes", true);
    a.write("a.rs", "fn a() {}\n");
    let b = a.add_worktree("db-bytes-b");
    b.write("b.rs", "fn b() {}\n");
    a.playbook(&["code", "index"]);
    b.playbook(&["code", "index"]);

    // Act
    let in_a = json_of(&a.playbook(&["code", "status", "--json"]));
    let in_b = json_of(&b.playbook(&["code", "status", "--json"]));

    // Assert
    assert_eq!(in_a["results"]["db_bytes"], in_b["results"]["db_bytes"]);
    assert!(in_a["results"]["db_bytes"].as_u64().is_some_and(|n| n > 0));
}

#[test]
fn an_empty_repo_indexes_zero_files_into_a_store() {
    // Arrange
    let fx = Fixture::new("empty-repo", true);

    // Act
    let out = fx.playbook(&["code", "index", "--json"]);

    // Assert
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(json_of(&out)["results"]["indexed"], 0);
}

#[test]
fn a_second_worktree_with_the_same_files_reuses_them() {
    // Arrange
    let a = Fixture::new("reuse", true);
    let b = a.add_worktree("reuse-b");
    for fx in [&a, &b] {
        fx.write("a.rs", "fn a() {}\n");
        fx.write("b.ts", "export const b = 1;\n");
        fx.write("c.py", "def c():\n    pass\n");
    }
    a.playbook(&["code", "index"]);

    // Act
    let json = json_of(&b.playbook(&["code", "index", "--json"]));
    let text = Fixture::playbook(&b, &["code", "index", "--rebuild"]);

    // Assert
    assert_eq!(json["results"]["reused"], 3);
    assert_eq!(json["results"]["parsed"], 0);
    assert!(String::from_utf8_lossy(&text.stdout).contains("reused: 0"));
}
