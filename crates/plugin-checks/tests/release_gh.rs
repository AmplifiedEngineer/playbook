// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Runs `scripts/release-gh.sh` (ADR 0039) against a fake `gh` that answers
//! from a table and logs every call.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn text_of(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A fake `gh` on PATH. Each line of `table` is `pattern<TAB>exit<TAB>output`;
/// the first pattern that matches the arguments as a shell glob answers,
/// on stdout, and also on stderr when the exit is not 0. A pattern that
/// starts with `1x ` answers once, so the next call gets the row after it.
/// An answer that is JSON goes through the call's `--jq` filter, as gh does.
/// Every call is logged, and a `--input` file is copied next to the log.
struct FakeGh {
    dir: PathBuf,
}

impl FakeGh {
    fn new(table: &[(&str, i32, &str)]) -> FakeGh {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("fake-gh-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("bin")).expect("fake gh dir");
        let gh = r#"#!/usr/bin/env bash
dir="$(cd "$(dirname "$0")/.." && pwd)"
args="$*"
printf '%s\n' "$args" >> "$dir/log"
prev=""
filter=""
for a in "$@"; do
  if [ "$prev" = "--input" ]; then cp "$a" "$dir/input.json"; fi
  if [ "$prev" = "--jq" ]; then filter="$a"; fi
  prev="$a"
done
n=0
while IFS=$'\t' read -r pattern code output; do
  n=$((n + 1))
  once=""
  case "$pattern" in "1x "*) pattern="${pattern#1x }"; once=1 ;; esac
  # shellcheck disable=SC2053
  if [[ "$args" == $pattern ]]; then
    if [ -n "$once" ]; then
      awk -v n="$n" 'NR != n' "$dir/table" > "$dir/table.new" && mv "$dir/table.new" "$dir/table"
    fi
    # Like gh, an answer goes to stdout, and an error body goes to stdout
    # with its message on stderr.
    if [ -n "$output" ]; then
      case "$code:$output" in
        0:\[* | 0:\{*) if [ -n "$filter" ]; then printf '%b\n' "$output" | jq -r "$filter"; else printf '%b\n' "$output"; fi ;;
        *) printf '%b\n' "$output" ;;
      esac
    fi
    if [ -n "$output" ] && [ "$code" != 0 ]; then printf '%b\n' "$output" >&2; fi
    exit "$code"
  fi
done < "$dir/table"
echo "fake gh: no answer for: $args" >&2
exit 99
"#;
        let path = dir.join("bin/gh");
        fs::write(&path, gh).expect("gh");
        Command::new("chmod")
            .args(["+x"])
            .arg(&path)
            .status()
            .expect("chmod");
        let fake = FakeGh { dir };
        fake.answer(table);
        fake
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new("bash")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/release-gh.sh"))
            .args(args)
            .env("PATH", path)
            .env("RELEASE_GH_POLL", "0")
            .current_dir(cwd)
            .output()
            .expect("bash")
    }

    /// Replaces the table, for a test that needs the fake's folder first.
    fn answer(&self, table: &[(&str, i32, &str)]) {
        let mut rows = String::new();
        for (pattern, code, output) in table {
            rows.push_str(&format!("{pattern}\t{code}\t{output}\n"));
        }
        fs::write(self.dir.join("table"), rows).expect("table");
    }

    fn log(&self) -> String {
        fs::read_to_string(self.dir.join("log")).unwrap_or_default()
    }
}

impl Drop for FakeGh {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// A git checkout under the fake's folder, as the caller of `pr` has, and
/// its HEAD.
fn checkout(gh: &FakeGh) -> (PathBuf, String) {
    let work = gh.dir.join("work");
    fs::create_dir_all(work.join("Formula")).expect("work");
    fs::write(work.join("Formula/pbtop.rb"), "class Pbtop\nend\n").expect("formula");
    fs::write(work.join("body.md"), "Body.\n").expect("body");
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .current_dir(&work)
            .output()
            .expect("git");
        assert!(out.status.success(), "{}", text_of(&out));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    let head = git(&["rev-parse", "HEAD"]);
    (work, head)
}

const PR_ARGS: [&str; 7] = [
    "pr",
    "o/tap",
    "release/v1.0.0",
    "chore(release): playbook 1.0.0",
    "body.md",
    ".",
    "Formula/pbtop.rb",
];

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const NOT_FOUND: &str = r#"{"message":"Not Found","status":"404"}"#;

const DEFAULT_BRANCH: &str = r#"{"default_branch":"main"}"#;
const COMMIT: &str = r#"{"data":{"createCommitOnBranch":{"commit":{"oid":"def456"}}}}"#;
const PULL: &str = r#"{"number":34}"#;
const OPEN_ON_BRANCH: &str = "api repos/o/tap/pulls?head=o:release/v1.0.0&state=open --jq length";

/// The answer to reading the head of a branch.
fn head_ref(sha: &str) -> String {
    format!(r#"{{"object":{{"type":"commit","sha":"{sha}"}}}}"#)
}

const RAW: &str = "api -H Accept: application/vnd.github.raw repos/o/tap/contents/Formula/pbtop.rb?ref=release/v1.0.0";

#[test]
fn an_open_or_merged_pull_request_from_the_app_is_reused() {
    // Arrange
    let open = FakeGh::new(&[]);
    let (open_work, _) = checkout(&open);
    open.answer(&[
        ("api repos/o/tap --jq .default_branch", 0, DEFAULT_BRANCH),
        (
            "pr list * --state open *",
            0,
            r#"[{"number":5,"isCrossRepository":true},{"number":12,"isCrossRepository":false}]"#,
        ),
        (RAW, 0, "class Pbtop\\nend"),
        ("pr merge 12 --repo o/tap --auto --squash", 0, ""),
    ]);
    let merged = FakeGh::new(&[]);
    let (merged_work, _) = checkout(&merged);
    merged.answer(&[
        ("api repos/o/tap --jq .default_branch", 0, DEFAULT_BRANCH),
        ("pr list * --state open *", 0, "[]"),
        (
            "pr list * --state merged *",
            0,
            r#"[{"number":7,"isCrossRepository":false}]"#,
        ),
    ]);

    // Act
    let reused = open.run(&open_work, &PR_ARGS);
    let done = merged.run(&merged_work, &PR_ARGS);

    // Assert
    assert_eq!(text_of(&reused), "pr=12\nstate=open\n");
    assert_eq!(text_of(&done), "pr=7\nstate=merged\n");
    assert!(
        open.log().contains("pr merge 12 --repo o/tap --auto"),
        "auto-merge is turned on again"
    );
    assert!(
        open.log()
            .contains("--head release/v1.0.0 --base main --state open --app igorjs"),
        "{}",
        open.log()
    );
    assert!(!open.log().contains("graphql") && !merged.log().contains("graphql"));
}

#[test]
fn an_open_pull_request_that_differs_from_the_checkout_is_not_reused() {
    // Arrange
    let gh = FakeGh::new(&[]);
    let (work, _) = checkout(&gh);
    gh.answer(&[
        ("api repos/o/tap --jq .default_branch", 0, DEFAULT_BRANCH),
        (
            "pr list * --state open *",
            0,
            r#"[{"number":12,"isCrossRepository":false}]"#,
        ),
        (RAW, 0, "class Pbtop\\n  # older\\nend"),
    ]);

    // Act
    let out = gh.run(&work, &PR_ARGS);

    // Assert
    assert!(!out.status.success());
    assert!(
        text_of(&out).contains("Formula/pbtop.rb on o/tap#12 differs from the checkout"),
        "{}",
        text_of(&out)
    );
    assert!(!gh.log().contains("pr merge"), "{}", gh.log());
}

#[test]
fn a_file_outside_the_checkout_stops_the_pull_request_before_any_call() {
    // Arrange
    let gh = FakeGh::new(&[]);
    let (work, _) = checkout(&gh);
    std::os::unix::fs::symlink("/etc/hosts", work.join("Formula/link.rb")).expect("symlink");
    let mut parent = PR_ARGS;
    parent[6] = "../body.md";
    let mut link = PR_ARGS;
    link[6] = "Formula/link.rb";
    fs::create_dir_all(gh.dir.join("outside")).expect("outside");
    fs::write(gh.dir.join("outside/secret.rb"), "secret\n").expect("secret");
    std::os::unix::fs::symlink(gh.dir.join("outside"), work.join("Formula/out")).expect("symlink");
    let mut through = PR_ARGS;
    through[6] = "Formula/out/secret.rb";

    // Act
    let up = gh.run(&work, &parent);
    let linked = gh.run(&work, &link);
    let escaped = gh.run(&work, &through);

    // Assert
    assert!(
        text_of(&up).contains("'../body.md' is not a plain path"),
        "{}",
        text_of(&up)
    );
    assert!(
        text_of(&linked).contains("Formula/link.rb is a symlink"),
        "{}",
        text_of(&linked)
    );
    assert!(
        text_of(&escaped).contains("Formula/out/secret.rb is outside the checkout"),
        "{}",
        text_of(&escaped)
    );
    assert_eq!(gh.log(), "", "nothing is called");
}

#[test]
fn a_new_pull_request_gets_one_signed_commit_and_auto_merge() {
    // Arrange
    let gh = FakeGh::new(&[]);
    let (work, head) = checkout(&gh);
    gh.answer(&[
        ("pr list *", 0, ""),
        ("api repos/o/tap --jq .default_branch", 0, DEFAULT_BRANCH),
        ("api repos/o/tap/git/ref/heads/main *", 0, &head_ref(&head)),
        ("api repos/o/tap/git/ref/heads/release/v1.0.0", 1, NOT_FOUND),
        ("api -X POST repos/o/tap/git/refs *", 0, ""),
        ("api graphql *", 0, COMMIT),
        ("api -X POST repos/o/tap/pulls *", 0, PULL),
        ("pr merge 34 --repo o/tap --auto --squash", 0, ""),
    ]);

    // Act
    let out = gh.run(&work, &PR_ARGS);
    let input = fs::read_to_string(gh.dir.join("input.json")).expect("the commit payload");

    // Assert
    assert!(out.status.success(), "{}", text_of(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "pr=34\nstate=open\n");
    assert!(
        input.contains(&format!("\"expectedHeadOid\": \"{head}\"")),
        "{input}"
    );
    assert!(input.contains("\"path\": \"Formula/pbtop.rb\""), "{input}");
    assert!(input.contains("Y2xhc3MgUGJ0b3AKZW5kCg=="), "{input}");
    assert!(
        input.contains("\"headline\": \"chore(release): playbook 1.0.0\""),
        "{input}"
    );
    assert!(
        gh.log()
            .contains(&format!("-f ref=refs/heads/release/v1.0.0 -f sha={head}")),
        "{}",
        gh.log()
    );
    assert!(!gh.log().contains("-X DELETE"));
}

#[test]
fn a_rerun_drops_a_stale_branch_and_falls_back_for_a_merge_queue() {
    // Arrange
    let gh = FakeGh::new(&[]);
    let (work, head) = checkout(&gh);
    gh.answer(&[
        ("pr list *", 0, ""),
        ("api repos/o/tap --jq .default_branch", 0, DEFAULT_BRANCH),
        ("api repos/o/tap/git/ref/heads/main *", 0, &head_ref(&head)),
        ("api repos/o/tap/git/ref/heads/release/v1.0.0", 0, "{}"),
        (OPEN_ON_BRANCH, 0, "[]"),
        (
            "api -X DELETE repos/o/tap/git/refs/heads/release/v1.0.0",
            0,
            "",
        ),
        ("api -X POST repos/o/tap/git/refs *", 0, ""),
        ("api graphql *", 0, COMMIT),
        ("api -X POST repos/o/tap/pulls *", 0, PULL),
        (
            "pr merge 34 --repo o/tap --auto --squash",
            1,
            "merge queue: no merge method",
        ),
        ("pr merge 34 --repo o/tap --auto", 0, ""),
    ]);

    // Act
    let out = gh.run(&work, &PR_ARGS);

    // Assert
    assert!(out.status.success(), "{}", text_of(&out));
    let log = gh.log();
    let delete = log
        .find("api -X DELETE repos/o/tap/git/refs/heads/release/v1.0.0")
        .expect("the stale branch is deleted");
    let create = log
        .find("api -X POST repos/o/tap/git/refs")
        .expect("the branch is created");
    assert!(delete < create, "{log}");
    assert!(
        gh.log().ends_with("pr merge 34 --repo o/tap --auto\n"),
        "{}",
        gh.log()
    );
}

#[test]
fn a_pull_request_is_refused_when_main_moved_or_the_branch_is_not_a_release() {
    // Arrange
    let gh = FakeGh::new(&[]);
    let (work, head) = checkout(&gh);
    gh.answer(&[
        ("pr list *", 0, ""),
        ("api repos/o/tap --jq .default_branch", 0, DEFAULT_BRANCH),
        ("api repos/o/tap/git/ref/heads/main *", 0, &head_ref(B)),
    ]);
    let mut feature = PR_ARGS;
    feature[2] = "main";

    // Act
    let moved = gh.run(&work, &PR_ARGS);
    let wrong = gh.run(&work, &feature);

    // Assert
    assert!(!moved.status.success());
    assert!(
        text_of(&moved).contains(&format!("moved from {head} to {B}")),
        "{}",
        text_of(&moved)
    );
    assert!(
        !gh.log().contains("refs"),
        "nothing is written: {}",
        gh.log()
    );
    assert!(
        text_of(&wrong).contains("branch 'main' is not release/vX.Y.Z"),
        "{}",
        text_of(&wrong)
    );
}

#[test]
fn a_tag_is_created_once_and_never_moved() {
    // Arrange
    let lightweight = format!(r#"{{"object":{{"type":"commit","sha":"{A}"}}}}"#);
    let annotated = format!(r#"{{"object":{{"type":"tag","sha":"{B}"}}}}"#);
    let fresh = FakeGh::new(&[
        ("api repos/o/r/git/ref/tags/v1.0.0", 1, NOT_FOUND),
        ("api -X POST repos/o/r/git/refs *", 0, ""),
    ]);
    let same = FakeGh::new(&[("api repos/o/r/git/ref/tags/v1.0.0", 0, &lightweight)]);
    let signed = FakeGh::new(&[
        ("api repos/o/r/git/ref/tags/v1.0.0", 0, &annotated),
        (
            &format!("api repos/o/r/git/tags/{B} --jq .object.sha"),
            0,
            A,
        ),
    ]);
    let moved = FakeGh::new(&[(
        "api repos/o/r/git/ref/tags/v1.0.0",
        0,
        &lightweight.replace(A, B),
    )]);
    let broken = FakeGh::new(&[(
        "api repos/o/r/git/ref/tags/v1.0.0",
        1,
        r#"{"message":"Server Error","status":"502"}"#,
    )]);
    let refused = FakeGh::new(&[
        ("api repos/o/r/git/ref/tags/v1.0.0", 1, NOT_FOUND),
        (
            "api -X POST repos/o/r/git/refs *",
            1,
            "Repository rule violations found",
        ),
    ]);
    let raced = FakeGh::new(&[
        ("1x api repos/o/r/git/ref/tags/v1.0.0", 1, NOT_FOUND),
        (
            "api -X POST repos/o/r/git/refs *",
            1,
            "Reference already exists",
        ),
        ("api repos/o/r/git/ref/tags/v1.0.0", 0, &lightweight),
    ]);
    let args = ["tag", "o/r", "v1.0.0", A];

    // Act
    let created = fresh.run(&fresh.dir, &args);
    let exists = same.run(&same.dir, &args);
    let followed = signed.run(&signed.dir, &args);
    let elsewhere = moved.run(&moved.dir, &args);
    let unknown = broken.run(&broken.dir, &args);
    let blocked = refused.run(&refused.dir, &args);
    let late = raced.run(&raced.dir, &args);

    // Assert
    assert_eq!(text_of(&created), "tag=v1.0.0\nstate=created\n");
    assert!(
        fresh
            .log()
            .contains(&format!("-f ref=refs/tags/v1.0.0 -f sha={A}")),
        "{}",
        fresh.log()
    );
    assert_eq!(text_of(&exists), "tag=v1.0.0\nstate=exists\n");
    assert_eq!(text_of(&followed), "tag=v1.0.0\nstate=exists\n");
    assert!(!same.log().contains("POST") && !signed.log().contains("POST"));
    assert!(text_of(&elsewhere).contains(&format!("points at {B}, not {A}")));
    assert!(!unknown.status.success());
    assert!(
        text_of(&unknown).contains("could not read tag v1.0.0"),
        "{}",
        text_of(&unknown)
    );
    assert!(!broken.log().contains("POST"));
    assert_eq!(blocked.status.code(), Some(3), "{}", text_of(&blocked));
    assert!(text_of(&blocked).contains("Repository rule violations"));
    assert_eq!(text_of(&late), "tag=v1.0.0\nstate=exists\n");
}

#[test]
fn a_release_is_created_only_when_missing() {
    // Arrange
    let exists = FakeGh::new(&[("release view v1.0.0 *", 0, "https://x/v1.0.0")]);
    let missing = FakeGh::new(&[
        ("release view *", 1, ""),
        ("release create *", 0, "https://x/new"),
    ]);
    let raced = FakeGh::new(&[
        ("1x release view *", 1, ""),
        ("release create *", 1, "already exists"),
        ("release view *", 0, "https://x/v1.0.0"),
    ]);
    let args = ["release", "o/r", "v1.0.0", "playbook 1.0.0", "notes.md"];

    // Act
    let kept = exists.run(&exists.dir, &args);
    let made = missing.run(&missing.dir, &args);
    let late = raced.run(&raced.dir, &args);

    // Assert
    assert_eq!(text_of(&kept), "url=https://x/v1.0.0\n");
    assert!(!exists.log().contains("release create"));
    assert!(missing
        .log()
        .contains("release create v1.0.0 --repo o/r --verify-tag --latest"));
    assert_eq!(text_of(&made), "url=https://x/new\n");
    assert!(late.status.success(), "{}", text_of(&late));
    assert!(
        String::from_utf8_lossy(&late.stdout).ends_with("url=https://x/v1.0.0\n"),
        "{}",
        text_of(&late)
    );
}

#[test]
fn waiting_reports_the_merge_commit_or_why_it_stopped() {
    // Arrange
    let merged = FakeGh::new(&[("pr view 34 *", 0, &format!("MERGED CLEAN {A}"))]);
    let closed = FakeGh::new(&[("pr view 34 *", 0, "CLOSED CLEAN ")]);
    let open = FakeGh::new(&[("pr view 34 *", 0, "OPEN CLEAN ")]);
    // A failed poll prints nothing on stdout, like gh.
    let failing = FakeGh::new(&[("pr view 34 *", 1, "")]);
    let recovered = FakeGh::new(&[
        ("1x pr view 34 *", 1, ""),
        ("pr view 34 *", 0, &format!("MERGED CLEAN {A}")),
    ]);

    // Act
    let done = merged.run(&merged.dir, &["wait", "o/r", "34", "60"]);
    let gone = closed.run(&closed.dir, &["wait", "o/r", "34", "60"]);
    let late = open.run(&open.dir, &["wait", "o/r", "34", "0"]);
    let flaky = failing.run(&failing.dir, &["wait", "o/r", "34", "0"]);
    let bad = open.run(&open.dir, &["wait", "o/r", "34", "1+1"]);
    let no_pr = open.run(&open.dir, &["wait", "o/r", "", "60"]);
    let retried = recovered.run(&recovered.dir, &["wait", "o/r", "34", "60"]);

    // Assert
    assert_eq!(text_of(&done), format!("sha={A}\n"));
    assert!(
        text_of(&gone).contains("closed without a merge"),
        "{}",
        text_of(&gone)
    );
    assert!(
        text_of(&late).contains("did not merge within 0 seconds"),
        "{}",
        text_of(&late)
    );
    assert_eq!(flaky.status.code(), Some(1), "{}", text_of(&flaky));
    assert!(
        text_of(&flaky).contains("did not merge within 0 seconds"),
        "{}",
        text_of(&flaky)
    );
    assert_eq!(text_of(&retried), format!("sha={A}\n"));
    assert_eq!(bad.status.code(), Some(2));
    assert_eq!(no_pr.status.code(), Some(2), "{}", text_of(&no_pr));
}

#[test]
fn an_open_pull_request_of_another_author_keeps_its_branch() {
    // Arrange
    let gh = FakeGh::new(&[]);
    let (work, head) = checkout(&gh);
    gh.answer(&[
        ("pr list *", 0, ""),
        ("api repos/o/tap --jq .default_branch", 0, DEFAULT_BRANCH),
        ("api repos/o/tap/git/ref/heads/main *", 0, &head_ref(&head)),
        ("api repos/o/tap/git/ref/heads/release/v1.0.0", 0, "{}"),
        (OPEN_ON_BRANCH, 0, r#"[{"number":40}]"#),
    ]);

    // Act
    let out = gh.run(&work, &PR_ARGS);

    // Assert
    assert!(!out.status.success());
    assert!(
        text_of(&out).contains("an open pull request in o/tap uses release/v1.0.0"),
        "{}",
        text_of(&out)
    );
    assert!(!gh.log().contains("-X DELETE"), "{}", gh.log());
    assert!(!gh.log().contains("-X POST"), "{}", gh.log());
}

#[test]
fn a_commit_that_github_did_not_create_opens_no_pull_request() {
    // Arrange
    let gh = FakeGh::new(&[]);
    let (work, head) = checkout(&gh);
    gh.answer(&[
        ("pr list *", 0, ""),
        ("api repos/o/tap --jq .default_branch", 0, DEFAULT_BRANCH),
        ("api repos/o/tap/git/ref/heads/main *", 0, &head_ref(&head)),
        ("api repos/o/tap/git/ref/heads/release/v1.0.0", 1, NOT_FOUND),
        ("api -X POST repos/o/tap/git/refs *", 0, ""),
        (
            "api graphql *",
            0,
            r#"{"data":{"createCommitOnBranch":null}}"#,
        ),
    ]);

    // Act
    let out = gh.run(&work, &PR_ARGS);

    // Assert
    assert!(!out.status.success());
    assert!(
        text_of(&out).contains("GitHub did not create the commit on o/tap release/v1.0.0"),
        "{}",
        text_of(&out)
    );
    assert!(!gh.log().contains("repos/o/tap/pulls"), "{}", gh.log());
}

#[test]
fn a_pull_request_that_refuses_auto_merge_fails_the_run() {
    // Arrange
    let gh = FakeGh::new(&[]);
    let (work, head) = checkout(&gh);
    gh.answer(&[
        ("pr list *", 0, ""),
        ("api repos/o/tap --jq .default_branch", 0, DEFAULT_BRANCH),
        ("api repos/o/tap/git/ref/heads/main *", 0, &head_ref(&head)),
        ("api repos/o/tap/git/ref/heads/release/v1.0.0", 1, NOT_FOUND),
        ("api -X POST repos/o/tap/git/refs *", 0, ""),
        ("api graphql *", 0, COMMIT),
        ("api -X POST repos/o/tap/pulls *", 0, PULL),
        (
            "pr merge 34 --repo o/tap --auto --squash",
            1,
            "auto-merge is off",
        ),
        ("pr merge 34 --repo o/tap --auto", 1, "auto-merge is off"),
    ]);

    // Act
    let out = gh.run(&work, &PR_ARGS);

    // Assert
    assert!(!out.status.success());
    assert!(
        text_of(&out).contains("could not turn on auto-merge for o/tap#34"),
        "{}",
        text_of(&out)
    );
}

#[test]
fn a_tag_or_release_that_is_not_a_version_is_refused_before_any_call() {
    // Arrange
    let gh = FakeGh::new(&[]);

    // Act
    let tag = gh.run(&gh.dir, &["tag", "o/r", "1.0.0", A]);
    let release = gh.run(
        &gh.dir,
        &["release", "o/r", "../heads/main", "t", "notes.md"],
    );

    // Assert
    assert!(
        text_of(&tag).contains("'1.0.0' is not a release tag vX.Y.Z"),
        "{}",
        text_of(&tag)
    );
    assert!(
        text_of(&release).contains("'../heads/main' is not a release tag vX.Y.Z"),
        "{}",
        text_of(&release)
    );
    assert_eq!(gh.log(), "", "nothing is called");
}

#[test]
fn the_release_state_is_none_only_when_github_says_not_found() {
    // Arrange
    let draft = FakeGh::new(&[("release view v1.0.0 --repo o/r *", 0, r#"{"isDraft":true}"#)]);
    let public = FakeGh::new(&[(
        "release view v1.0.0 --repo o/r *",
        0,
        r#"{"isDraft":false}"#,
    )]);
    let missing = FakeGh::new(&[("release view v1.0.0 --repo o/r *", 1, "release not found")]);
    let broken = FakeGh::new(&[("release view v1.0.0 --repo o/r *", 1, "HTTP 502")]);
    let args = ["state", "o/r", "v1.0.0"];

    // Act
    let d = draft.run(&draft.dir, &args);
    let p = public.run(&public.dir, &args);
    let m = missing.run(&missing.dir, &args);
    let b = broken.run(&broken.dir, &args);

    // Assert
    assert_eq!(String::from_utf8_lossy(&d.stdout), "release=draft\n");
    assert_eq!(String::from_utf8_lossy(&p.stdout), "release=published\n");
    assert_eq!(String::from_utf8_lossy(&m.stdout), "release=none\n");
    assert!(!b.status.success());
    assert!(
        text_of(&b).contains("could not read the release v1.0.0 in o/r: HTTP 502"),
        "{}",
        text_of(&b)
    );
    assert_eq!(
        String::from_utf8_lossy(&b.stdout),
        "",
        "no state on a failed read"
    );
}

#[test]
fn find_reports_an_open_or_merged_pull_request_from_the_app() {
    // Arrange
    let open = FakeGh::new(&[
        ("api repos/o/tap --jq .default_branch", 0, DEFAULT_BRANCH),
        (
            "pr list * --state open *",
            0,
            r#"[{"number":12,"isCrossRepository":false}]"#,
        ),
    ]);
    let merged = FakeGh::new(&[
        ("api repos/o/tap --jq .default_branch", 0, DEFAULT_BRANCH),
        ("pr list * --state open *", 0, "[]"),
        (
            "pr list * --state merged *",
            0,
            r#"[{"number":7,"isCrossRepository":false}]"#,
        ),
    ]);
    let none = FakeGh::new(&[
        ("api repos/o/tap --jq .default_branch", 0, DEFAULT_BRANCH),
        ("pr list *", 0, "[]"),
    ]);
    let args = ["find", "o/tap", "release/v1.0.0"];

    // Act
    let o = open.run(&open.dir, &args);
    let m = merged.run(&merged.dir, &args);
    let n = none.run(&none.dir, &args);
    let bad = none.run(&none.dir, &["find", "o/tap", "main"]);

    // Assert
    assert_eq!(text_of(&o), "pr=12\nstate=open\n");
    assert_eq!(text_of(&m), "pr=7\nstate=merged\n");
    assert!(n.status.success(), "{}", text_of(&n));
    assert_eq!(text_of(&n), "");
    assert!(
        open.log()
            .contains("--head release/v1.0.0 --base main --state open --app igorjs"),
        "{}",
        open.log()
    );
    assert!(!bad.status.success());
}
