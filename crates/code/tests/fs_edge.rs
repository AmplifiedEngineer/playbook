// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! File-system edge cases and pathological files. Every test builds its own
//! scratch git repo and its own store, so tests never share state.

use playbook_code::{db, index, walker};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const WORKTREE: &str = "edge-worktree";
const TEST_TIMEOUT_SECONDS: u64 = 30;
const READY_POLL_MS: u64 = 20;
const READY_FILE: &str = "ready";
const HOLDER_ENV: &str = "FS_EDGE_HOLDER";
const HOLDER_TEST: &str = "lock_holder_child";
const HOLD_SECONDS: u64 = 300;
const SMALL_LIMIT_BYTES: u64 = 1024;
const SMALL_PROBE_BYTES: usize = 512;
const SOURCE: &str = "pub fn kept() -> u32 {\n    1\n}\n";

struct Scratch {
    root: PathBuf,
    store: PathBuf,
}

fn scratch(tag: &str) -> Scratch {
    let base = playbook_common::test_support::scratch_dir(&format!("edge-{tag}"));
    let root = base.join("repo");
    fs::create_dir_all(&root).expect("repo dir");
    git(&root, &["init", "--quiet"]);
    Scratch {
        root,
        store: base.join("store").join("index.sqlite3"),
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?}");
}

fn write(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
    fs::write(path, bytes).expect("write");
}

/// Runs `job` on a thread and fails the test when it outlives the timeout.
fn within_timeout<T: Send + 'static>(job: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || drop(tx.send(job())));
    rx.recv_timeout(Duration::from_secs(TEST_TIMEOUT_SECONDS))
        .expect("the run finished within the test timeout")
}

fn index_with(s: &Scratch, opts: index::Options) -> index::Report {
    let (root, store) = (s.root.clone(), s.store.clone());
    within_timeout(move || index::run(&root, &store, WORKTREE, false, &opts).expect("index run"))
}

fn index_default(s: &Scratch) -> index::Report {
    index_with(s, index::Options::default())
}

fn small_limits() -> index::Options {
    index::Options {
        limits: walker::Limits {
            max_file_bytes: SMALL_LIMIT_BYTES,
            binary_probe_bytes: SMALL_PROBE_BYTES,
        },
        ..index::Options::default()
    }
}

/// Path, status and error of every listed file.
fn files(s: &Scratch) -> Vec<(String, String, Option<String>)> {
    let conn = db::open_query(&s.store, WORKTREE).expect("query open");
    let mut stmt = conn
        .prepare("SELECT path, status, error FROM files ORDER BY path")
        .expect("prepare");
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query");
    rows.map(|r| r.expect("row")).collect()
}

fn indexed_paths(s: &Scratch) -> BTreeSet<String> {
    files(s)
        .into_iter()
        .filter(|(_, status, _)| status == "ok" || status == "partial")
        .map(|(path, _, _)| path)
        .collect()
}

fn state_of(s: &Scratch, path: &str) -> Option<(String, Option<String>)> {
    files(s)
        .into_iter()
        .find(|(p, _, _)| p == path)
        .map(|(_, status, error)| (status, error))
}

fn symbols(store: &Path) -> Vec<(String, String, i64)> {
    let conn = db::open_query(store, WORKTREE).expect("query open");
    let mut stmt = conn
        .prepare(
            "SELECT f.path, s.qualified_name, s.start_line FROM symbols s
             JOIN files f ON f.id = s.file_id ORDER BY 1, 3, 2",
        )
        .expect("prepare");
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query");
    rows.map(|r| r.expect("row")).collect()
}

#[test]
fn a_huge_file_is_skipped_as_too_large_and_the_run_ends_with_a_report() {
    // Arrange
    let s = scratch("huge");
    write(&s.root, "ok.rs", SOURCE.as_bytes());
    let huge = SOURCE.repeat(usize::try_from(SMALL_LIMIT_BYTES).expect("limit"));
    write(&s.root, "huge.rs", huge.as_bytes());

    // Act
    let report = index_with(&s, small_limits());

    // Assert
    assert_eq!(report.skipped, 1);
    assert!(!indexed_paths(&s).contains("huge.rs"));
    assert_eq!(
        state_of(&s, "huge.rs"),
        Some(("skipped".into(), Some("too_large".into())))
    );
}

#[test]
fn a_binary_file_is_skipped_as_binary_and_the_run_ends_with_a_report() {
    // Arrange
    let s = scratch("binary");
    write(&s.root, "ok.rs", SOURCE.as_bytes());
    write(&s.root, "blob.rs", &[0x7f, b'E', b'L', b'F', 0, 0, 1, 2]);

    // Act
    let report = index_default(&s);

    // Assert
    assert_eq!(report.skipped, 1);
    assert!(!indexed_paths(&s).contains("blob.rs"));
    assert_eq!(
        state_of(&s, "blob.rs"),
        Some(("skipped".into(), Some("binary".into())))
    );
}

#[test]
fn a_non_utf8_file_is_indexed_from_its_lossy_text() {
    // Arrange
    let s = scratch("non-utf8");
    let mut bytes = b"// caf\xe9 \xff\xfe\n".to_vec();
    bytes.extend_from_slice(SOURCE.as_bytes());
    write(&s.root, "latin.rs", &bytes);

    // Act
    let report = index_default(&s);

    // Assert
    assert_eq!(report.failed, 0);
    assert!(indexed_paths(&s).contains("latin.rs"));
    assert!(symbols(&s.store)
        .iter()
        .any(|(path, name, _)| path == "latin.rs" && name == "kept"));
}

#[cfg(unix)]
#[test]
fn a_dangling_symlink_is_skipped_without_a_panic() {
    // Arrange
    let s = scratch("dangling");
    write(&s.root, "ok.rs", SOURCE.as_bytes());
    std::os::unix::fs::symlink("no-such-target.rs", s.root.join("link.rs")).expect("symlink");

    // Act
    let report = index_default(&s);

    // Assert
    assert_eq!(report.failed, 0);
    assert!(!indexed_paths(&s).contains("link.rs"));
    assert_eq!(
        state_of(&s, "link.rs"),
        Some(("skipped".into(), Some("symlink".into())))
    );
}

#[test]
fn nested_gitignore_files_and_negations_decide_what_is_listed() {
    // Arrange
    let s = scratch("gitignore");
    write(&s.root, ".gitignore", b"*.log\n");
    write(&s.root, "sub/.gitignore", b"gen/\n!keep.log\n");
    write(&s.root, "a.rs", SOURCE.as_bytes());
    write(&s.root, "drop.log", b"noise\n");
    write(&s.root, "sub/b.rs", SOURCE.as_bytes());
    write(&s.root, "sub/keep.log", b"kept\n");
    write(&s.root, "sub/gen/x.rs", SOURCE.as_bytes());

    // Act
    index_default(&s);

    // Assert
    let listed: BTreeSet<String> = files(&s).into_iter().map(|(p, _, _)| p).collect();
    let want = [
        ".gitignore",
        "a.rs",
        "sub/.gitignore",
        "sub/b.rs",
        "sub/keep.log",
    ];
    assert_eq!(listed, want.iter().map(ToString::to_string).collect());
}

#[cfg(unix)]
#[test]
fn an_unreadable_file_counts_as_failed_and_the_rest_is_indexed() {
    use std::os::unix::fs::PermissionsExt;
    // Arrange
    assert!(
        !rustix::process::geteuid().is_root(),
        "permission tests need a normal user; a root process reads every file"
    );
    let s = scratch("unreadable");
    write(&s.root, "ok.rs", SOURCE.as_bytes());
    write(&s.root, "secret.rs", SOURCE.as_bytes());
    let secret = s.root.join("secret.rs");
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).expect("chmod");

    // Act
    let report = index_default(&s);

    // Assert
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o644)).expect("chmod back");
    assert_eq!(report.failed, 1);
    assert!(indexed_paths(&s).contains("ok.rs"));
    assert!(!indexed_paths(&s).contains("secret.rs"));
}

/// Child side of the kill tests. In a normal run the variable is absent and
/// this passes at once. As a child it takes the lock named by the variable,
/// writes the ready file, then waits to be killed.
#[test]
fn lock_holder_child() {
    let Ok(spec) = std::env::var(HOLDER_ENV) else {
        return;
    };
    let (kind, rest) = spec.split_once('|').expect("kind|store|ready");
    let (store, ready) = rest.split_once('|').expect("kind|store|ready");
    let store = PathBuf::from(store);
    let _run_lock;
    let _store_lock;
    match kind {
        "run" => _run_lock = Some(index::acquire_lock(&store, WORKTREE).expect("run lock")),
        _ => {
            let file = fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(db::store_lock_path(&store))
                .expect("store lock file");
            file.lock().expect("store lock");
            _store_lock = Some(file);
        }
    }
    fs::write(ready, b"ok").expect("ready file");
    std::thread::sleep(Duration::from_secs(HOLD_SECONDS));
}

fn spawn_holder(s: &Scratch, kind: &str) -> Child {
    fs::create_dir_all(s.store.parent().expect("store dir")).expect("store dir");
    let ready = s.store.with_file_name(READY_FILE);
    let child = Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", HOLDER_TEST, "--nocapture"])
        .env(
            HOLDER_ENV,
            format!("{kind}|{}|{}", s.store.display(), ready.display()),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn holder");
    let started = Instant::now();
    while !ready.exists() {
        assert!(
            started.elapsed() < Duration::from_secs(TEST_TIMEOUT_SECONDS),
            "the holder never signalled readiness"
        );
        std::thread::sleep(Duration::from_millis(READY_POLL_MS));
    }
    child
}

fn kill_then_index_equals_a_fresh_index(kind: &str) {
    // Arrange
    let s = scratch(&format!("kill-{kind}"));
    write(&s.root, "a.rs", SOURCE.as_bytes());
    write(&s.root, "b.py", b"def helper():\n    return 1\n");
    let mut holder = spawn_holder(&s, kind);
    let blocked = index::run(
        &s.root,
        &s.store,
        WORKTREE,
        false,
        &index::Options::default(),
    );
    holder.kill().expect("kill -9");
    holder.wait().expect("reap");

    // Act
    let after = index_default(&s);

    // Assert
    if kind == "run" {
        assert!(blocked.is_err(), "a live holder blocks the run");
    }
    assert_eq!(after.failed, 0);
    let fresh = scratch(&format!("fresh-{kind}"));
    write(&fresh.root, "a.rs", SOURCE.as_bytes());
    write(&fresh.root, "b.py", b"def helper():\n    return 1\n");
    index_default(&fresh);
    assert_eq!(symbols(&s.store), symbols(&fresh.store));
    assert_eq!(indexed_paths(&s), indexed_paths(&fresh));
}

#[cfg(unix)]
#[test]
fn a_killed_run_lock_holder_leaves_no_stale_lock() {
    kill_then_index_equals_a_fresh_index("run");
}

#[cfg(unix)]
#[test]
fn a_killed_store_lock_holder_leaves_no_stale_lock() {
    kill_then_index_equals_a_fresh_index("store");
}
