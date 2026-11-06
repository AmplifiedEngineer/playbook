// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook code` on every language fixture. One test per command and
//! language pair, each in its own temp repo, temp home and temp store. Exit
//! code and stdout must equal the committed file in
//! `tests/fixtures/code_acceptance/<language>/<command>.txt`. Set
//! `UPDATE_ACCEPTANCE=1` to rewrite those files.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const UPDATE_ENV: &str = "UPDATE_ACCEPTANCE";
const ORIGIN: &str = "https://github.com/test-owner/test-repo.git";
const READ_RANGE: &str = "1:8";
const INVALID_USAGE_EXIT: i32 = 2;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// What one language fixture asks of each command.
struct Scenario {
    lang: &'static str,
    /// A name that is declared, used and searched in the fixture.
    name: &'static str,
    /// A file that declares symbols.
    file: &'static str,
}

fn git_binary() -> PathBuf {
    ["/usr/bin", "/opt/homebrew/bin", "/usr/local/bin", "/bin"]
        .iter()
        .map(|dir| Path::new(dir).join("git"))
        .find(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("git"))
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("dir");
    for entry in fs::read_dir(from).expect("read dir") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy");
        }
    }
}

/// A temp copy of the fixture repo with an `origin`, a home and a PATH that
/// holds only git.
struct Sandbox {
    repo: PathBuf,
    home: PathBuf,
    bin: PathBuf,
}

impl Sandbox {
    fn new(lang: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!(
            "playbook-acceptance-{lang}-{}-{n}",
            playbook::testing::run_id()
        ));
        let (repo, home, bin) = (base.join("repo"), base.join("home"), base.join("bin"));
        fs::create_dir_all(&home).expect("home");
        fs::create_dir_all(&bin).expect("bin");
        #[cfg(unix)]
        std::os::unix::fs::symlink(git_binary(), bin.join("git")).expect("git link");
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../code/tests/fixtures")
            .join(lang)
            .join("repo");
        copy_tree(&source, &repo);
        let sandbox = Self {
            repo: repo.canonicalize().expect("repo"),
            home,
            bin,
        };
        sandbox.git(&["init", "--quiet"]);
        sandbox.git(&["remote", "add", "origin", ORIGIN]);
        sandbox
    }

    fn git(&self, args: &[&str]) {
        let status = Command::new(git_binary())
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .env_clear()
            .env("HOME", &self.home)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?}");
    }

    fn playbook(&self, args: &[&str]) -> std::process::Output {
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

fn expected_path(lang: &str, command: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/code_acceptance")
        .join(lang)
        .join(format!("{command}.txt"))
}

/// Runs `command`, after an index run unless it is the index command, and
/// compares the result to its file.
fn check(s: &Scenario, command: &str, args: &[&str]) {
    let sandbox = Sandbox::new(s.lang);
    if command != "index" {
        let indexed = sandbox.playbook(&["code", "index"]);
        assert!(indexed.status.success(), "{}", s.lang);
    }
    let mut full = vec!["code"];
    full.extend_from_slice(args);
    let out = sandbox.playbook(&full);
    let actual = format!(
        "exit: {}\n{}",
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout)
    );
    let path = expected_path(s.lang, command);
    if std::env::var_os(UPDATE_ENV).is_some() {
        fs::create_dir_all(path.parent().expect("dir")).expect("expected dir");
        fs::write(&path, &actual).expect("write expected");
    }
    let want = fs::read_to_string(&path).expect("committed expected output");
    assert_eq!(actual, want, "{} {command}", s.lang);
}

fn run_command(s: &Scenario, command: &str) {
    match command {
        "index" => check(s, command, &["index"]),
        "status" => check(s, command, &["status"]),
        "search" => check(s, command, &["search", s.name]),
        "symbol" => check(s, command, &["symbol", s.name]),
        "outline" => check(s, command, &["outline", s.file]),
        "read" => check(s, command, &["read", s.file, READ_RANGE]),
        "refs" => check(s, command, &["refs", s.name]),
        "context" => check(s, command, &["context", s.name]),
        other => panic!("unknown command {other}"),
    }
}

macro_rules! pair_tests {
    ($lang:ident, $name:expr, $file:expr; $($command:ident),+) => {
        mod $lang {
            use super::{run_command, Scenario};
            const SCENARIO: Scenario = Scenario {
                lang: stringify!($lang),
                name: $name,
                file: $file,
            };
            $(
                #[test]
                fn $command() {
                    run_command(&SCENARIO, stringify!($command));
                }
            )+
        }
    };
}

macro_rules! all_commands {
    ($lang:ident, $name:expr, $file:expr) => {
        pair_tests!($lang, $name, $file; index, status, search, symbol, outline, read, refs, context);
    };
}

all_commands!(rust, "area", "src/lib.rs");
all_commands!(typescript, "Square", "src/shapes.ts");
all_commands!(javascript, "Counter", "src/counter.js");
all_commands!(python, "Square", "pkg/shapes.py");
all_commands!(go, "Square", "shape.go");

#[test]
fn an_unsupported_language_filter_fails_with_the_value_error() {
    // Arrange
    let sandbox = Sandbox::new("rust");

    // Act
    let out = sandbox.playbook(&["code", "search", "area", "--lang", "cobol"]);

    // Assert
    assert_eq!(out.status.code(), Some(INVALID_USAGE_EXIT));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("invalid value 'cobol' for '--lang"), "{err}");
}

#[test]
fn an_out_of_range_limit_fails_with_the_range_error() {
    // Arrange
    let sandbox = Sandbox::new("rust");

    // Act
    let out = sandbox.playbook(&["code", "symbol", "area", "--limit", "0"]);

    // Assert
    assert_eq!(out.status.code(), Some(INVALID_USAGE_EXIT));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("is not in 1..=200"), "{err}");
}
