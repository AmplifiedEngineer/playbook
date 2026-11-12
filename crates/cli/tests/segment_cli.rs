// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook segment branch|size` against real scratch git repos: the branch
//! name and base per delivery topology, and the size report.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Repo {
    root: PathBuf,
    work: PathBuf,
    origin: PathBuf,
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

impl Repo {
    /// A work clone on `main` with one commit, and a bare `origin` it tracks.
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "playbook-segment-{tag}-{}-{n}",
            playbook::testing::run_id()
        ));
        fs::create_dir_all(&root).expect("scratch dir");
        let root = root.canonicalize().expect("scratch dir resolves");
        let origin = root.join("origin.git");
        let work = root.join("work");
        fs::create_dir_all(&origin).expect("origin dir");
        fs::create_dir_all(&work).expect("work dir");
        git_in(&origin, &["init", "-q", "--bare", "-b", "main"]);
        git_in(&work, &["init", "-q", "-b", "main"]);
        git_in(
            &work,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        let repo = Self { root, work, origin };
        repo.commit("seed.txt", 1);
        git_in(&repo.work, &["push", "-q", "origin", "main"]);
        repo
    }

    /// A commit that adds `lines` lines in `file`.
    fn commit(&self, file: &str, lines: usize) -> String {
        let body = (0..lines)
            .map(|i| format!("line {i}\n"))
            .collect::<String>();
        fs::write(self.work.join(file), body).expect("write file");
        git_in(&self.work, &["add", "."]);
        git_in(
            &self.work,
            &["-c", "commit.gpgsign=false", "commit", "-q", "-m", file],
        );
        git_in(&self.work, &["rev-parse", "HEAD"])
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_playbook"))
            .arg("segment")
            .args(args)
            .arg("--dir")
            .arg(&self.work)
            .env("HOME", &self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("playbook runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        )
    }

    fn head_branch(&self) -> String {
        git_in(&self.work, &["rev-parse", "--abbrev-ref", "HEAD"])
    }
}

const NAMING: [&str; 8] = [
    "--type",
    "feat",
    "--plan-slug",
    "my-plan",
    "--title",
    "Parser core",
    "--n",
    "2",
];

fn with(extra: &[&str]) -> Vec<String> {
    NAMING
        .iter()
        .chain(extra)
        .map(|s| (*s).to_string())
        .collect()
}

fn run_with(repo: &Repo, sub: &str, extra: &[&str]) -> (i32, String, String) {
    let mut args = vec![sub.to_string()];
    args.extend(with(extra));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    repo.run(&refs)
}

#[test]
fn independent_branches_off_the_default_branch() {
    let repo = Repo::new("independent");
    let main = git_in(&repo.work, &["rev-parse", "main"]);

    let (code, out, err) = run_with(&repo, "branch", &["--topology", "independent"]);

    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out,
        format!("branch=feat/my-plan-s2-parser-core base={main} base_ref=main")
    );
    assert_eq!(repo.head_branch(), "feat/my-plan-s2-parser-core");
}

#[test]
fn stacked_branches_off_the_previous_segment() {
    let repo = Repo::new("stacked");
    git_in(
        &repo.work,
        &["switch", "-q", "-c", "feat/my-plan-s1-schema"],
    );
    let prev_tip = repo.commit("one.txt", 3);
    git_in(&repo.work, &["switch", "-q", "main"]);

    let (code, out, err) = run_with(
        &repo,
        "branch",
        &[
            "--topology",
            "stacked",
            "--prev-branch",
            "feat/my-plan-s1-schema",
        ],
    );

    assert_eq!(code, 0, "{err}");
    assert!(out.contains(&format!("base={prev_tip} base_ref=feat/my-plan-s1-schema")));
    assert_eq!(repo.head_branch(), "feat/my-plan-s2-parser-core");
}

#[test]
fn stacked_fetches_a_previous_branch_that_is_not_local() {
    let repo = Repo::new("stacked-fetch");
    git_in(
        &repo.work,
        &["switch", "-q", "-c", "feat/my-plan-s1-schema"],
    );
    let prev_tip = repo.commit("one.txt", 3);
    git_in(
        &repo.work,
        &["push", "-q", "origin", "feat/my-plan-s1-schema"],
    );
    git_in(&repo.work, &["switch", "-q", "main"]);
    git_in(
        &repo.work,
        &["branch", "-q", "-D", "feat/my-plan-s1-schema"],
    );

    let (code, out, err) = run_with(
        &repo,
        "branch",
        &[
            "--topology",
            "stacked",
            "--prev-branch",
            "feat/my-plan-s1-schema",
        ],
    );

    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains(&format!(
            "base={prev_tip} base_ref=origin/feat/my-plan-s1-schema"
        )),
        "{out}"
    );
}

#[test]
fn land_branches_off_the_fetched_origin_default_even_when_local_main_is_stale() {
    let repo = Repo::new("land");
    // origin/main moves on while local main stays behind.
    let other = repo.root.join("other");
    git_in(
        &repo.root,
        &["clone", "-q", repo.origin.to_str().unwrap(), "other"],
    );
    fs::write(other.join("later.txt"), "later\n").unwrap();
    git_in(&other, &["add", "."]);
    git_in(
        &other,
        &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "later"],
    );
    git_in(&other, &["push", "-q", "origin", "main"]);
    let origin_tip = git_in(&other, &["rev-parse", "HEAD"]);

    let (code, out, err) = run_with(
        &repo,
        "branch",
        &["--topology", "stacked", "--prev-branch", "gone", "--land"],
    );

    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains(&format!("base={origin_tip} base_ref=origin/main")),
        "{out}"
    );
}

#[test]
fn single_keeps_the_shared_branch_and_refuses_the_default_branch() {
    let repo = Repo::new("single");
    let (code, _, err) = run_with(&repo, "branch", &["--topology", "single"]);
    assert_eq!(code, 1);
    assert!(err.contains("default branch"), "{err}");

    git_in(&repo.work, &["switch", "-q", "-c", "feat/my-plan"]);
    let tip = repo.commit("a.txt", 2);
    let (code, out, err) = run_with(&repo, "branch", &["--topology", "single"]);

    assert_eq!(code, 0, "{err}");
    assert_eq!(out, format!("branch=feat/my-plan base={tip} base_ref=HEAD"));
    assert_eq!(repo.head_branch(), "feat/my-plan");
}

#[test]
fn an_existing_segment_branch_is_an_error() {
    let repo = Repo::new("exists");
    git_in(&repo.work, &["branch", "feat/my-plan-s2-parser-core"]);

    let (code, _, err) = run_with(&repo, "branch", &["--topology", "independent"]);

    assert_eq!(code, 1);
    assert!(err.contains("already exists"), "{err}");
}

#[test]
fn size_counts_changed_lines_and_files() {
    let repo = Repo::new("size");
    git_in(&repo.work, &["switch", "-q", "-c", "feat/x"]);
    repo.commit("a.txt", 6);
    repo.commit("b.txt", 5);

    let (_, out, _) = repo.run(&["size", "--base", "main"]);

    assert_eq!(out, "lines=11 files=2");
}
