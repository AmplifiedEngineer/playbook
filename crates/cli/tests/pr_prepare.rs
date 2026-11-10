// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `pr prepare` against real scratch git repos with a local bare `origin`,
//! and a fake `GhClient` so no test needs GitHub. `prepare` runs git in the
//! process cwd, so every test serialises on it like `tests/gate_record.rs`.

#[path = "pr_support/cwd.rs"]
mod cwd;
#[path = "pr_support/fake_gh.rs"]
mod fake_gh;
#[path = "pr_support/fixture.rs"]
mod fixture;
#[path = "pr_support/layout.rs"]
mod layout;

use cwd::in_dir;
use fake_gh::FakeGh;
use fixture::{git, Fixture};
use playbook::pr::prepare::prepare;
use playbook::pr::shared::ExistingPr;
use std::fs;

trait Branch {
    fn branch(&self) -> String;
}

impl Branch for Fixture {
    fn branch(&self) -> String {
        git(&self.work, &["branch", "--show-current"])
    }
}

trait RunPrepare {
    fn run(&self, gh: &FakeGh, base: Option<&str>, ticket: Option<&str>) -> Result<String, String>;
}

impl RunPrepare for Fixture {
    fn run(&self, gh: &FakeGh, base: Option<&str>, ticket: Option<&str>) -> Result<String, String> {
        let branch = self.branch();
        in_dir(&self.work, || {
            prepare(&self.state(), &branch, gh, base, ticket)
        })
    }
}

fn has_line(out: &str, line: &str) -> bool {
    out.lines().any(|l| l == line)
}

const OVER_NOTE: &str = "needs explicit justification in the PR body";

#[test]
fn an_open_pr_returns_ok_with_the_redirect_and_runs_no_further_checks() {
    // Arrange
    let fx = Fixture::new("existing", "feat/x");
    fx.commit_lines("a.txt", 3);
    let gh = FakeGh {
        existing: Some(ExistingPr {
            url: "https://example.test/pr/9".into(),
            state: "OPEN".into(),
        }),
        ..FakeGh::default()
    };

    // Act
    let got = fx
        .run(&gh, None, None)
        .expect("an existing PR is a success path");

    // Assert
    assert_eq!(
        got,
        "A PR already exists: https://example.test/pr/9\n\
         Use /playbook:pr-comments or /playbook:quick-review instead."
    );
    assert!(
        gh.count("repo_default_branch") == 0,
        "no further checks may run"
    );
}

#[test]
fn a_closed_pr_does_not_trigger_the_redirect() {
    // Arrange
    let fx = Fixture::new("closed", "feat/x");
    fx.commit_lines("a.txt", 3);
    let gh = FakeGh {
        existing: Some(ExistingPr {
            url: "https://example.test/pr/9".into(),
            state: "MERGED".into(),
        }),
        ..FakeGh::default()
    };

    // Act
    let got = fx.run(&gh, None, None).expect("proceeds");

    // Assert
    assert!(has_line(&got, "branch=feat/x"), "got {got}");
}

#[test]
fn it_fetches_the_base_itself_so_a_stale_local_ref_cannot_hide_that_nothing_is_ahead() {
    // Arrange: the branch looks one commit ahead of the locally cached
    // `origin/main`, but origin's `main` already contains that commit.
    let fx = Fixture::new("stale", "feat/x");
    fx.commit_lines("a.txt", 3);
    git(&fx.work, &["push", "--quiet", "origin", "feat/x"]);
    git(
        &fx.bare(),
        &["update-ref", "refs/heads/main", "refs/heads/feat/x"],
    );

    // Act
    let got = fx.run(&FakeGh::default(), None, None);

    // Assert
    let err = got.expect_err("after a fresh fetch nothing is ahead");
    assert!(err.contains("nothing ahead of main"), "got {err}");
}

#[test]
fn the_base_flag_wins_and_the_repo_default_is_never_asked() {
    // Arrange
    let fx = Fixture::new("flag", "feat/x");
    fx.publish_base_as("release");
    fx.commit_lines("a.txt", 3);
    let gh = FakeGh {
        default_branch: Some("develop".into()),
        ..FakeGh::default()
    };

    // Act
    let got = fx.run(&gh, Some("release"), None).expect("prepares");

    // Assert
    assert!(
        has_line(&got, "base=release (source: --base flag)"),
        "got {got}"
    );
    assert!(gh.count("repo_default_branch") == 0);
}

#[test]
fn the_repo_default_wins_over_symbolic_ref_when_no_flag_is_given() {
    // Arrange
    let fx = Fixture::new("default", "feat/x");
    fx.publish_base_as("develop");
    fx.publish_base_as("trunk");
    git(
        &fx.work,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk",
        ],
    );
    fx.commit_lines("a.txt", 3);
    let gh = FakeGh {
        default_branch: Some("develop".into()),
        ..FakeGh::default()
    };

    // Act
    let got = fx.run(&gh, None, None).expect("prepares");

    // Assert
    assert!(
        has_line(&got, "base=develop (source: repo default)"),
        "got {got}"
    );
}

#[test]
fn symbolic_ref_wins_over_the_main_fallback_when_the_repo_default_is_unavailable() {
    // Arrange
    let fx = Fixture::new("symref", "feat/x");
    fx.publish_base_as("trunk");
    git(
        &fx.work,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk",
        ],
    );
    fx.commit_lines("a.txt", 3);

    // Act
    let got = fx.run(&FakeGh::default(), None, None).expect("prepares");

    // Assert
    assert!(
        has_line(&got, "base=trunk (source: git symbolic-ref)"),
        "got {got}"
    );
}

#[test]
fn main_is_the_fallback_only_when_every_earlier_tier_is_unavailable() {
    // Arrange
    let fx = Fixture::new("fallback", "feat/x");
    fx.commit_lines("a.txt", 3);

    // Act
    let got = fx.run(&FakeGh::default(), None, None).expect("prepares");

    // Assert
    assert!(has_line(&got, "base=main (source: fallback)"), "got {got}");
}

#[test]
fn being_on_the_base_branch_is_a_hard_abort_naming_it() {
    // Arrange
    let fx = Fixture::new("onbase", "feat/x");
    git(&fx.work, &["checkout", "--quiet", "main"]);

    // Act
    let got = fx.run(&FakeGh::default(), None, None);

    // Assert
    let err = got.expect_err("on the base branch");
    assert!(err.contains("on the base branch (main)"), "got {err}");
}

#[test]
fn zero_commits_ahead_is_a_hard_abort() {
    // Arrange
    let fx = Fixture::new("zero", "feat/x");

    // Act
    let got = fx.run(&FakeGh::default(), None, None);

    // Assert
    let err = got.expect_err("nothing to PR");
    assert!(err.contains("nothing ahead of main"), "got {err}");
}

#[test]
fn size_verdicts_pin_every_threshold_boundary() {
    // Arrange
    let soft =
        |n: usize| format!("VERDICT size: SOFT - {n} lines is above the 500-line soft limit");
    let over = |n: usize| {
        format!(
            "VERDICT size: OVER - {n} lines is above the 1000-line enforced limit and {OVER_NOTE}"
        )
    };
    let cases: Vec<(usize, String)> = vec![
        (500, "VERDICT size: OK - 500 lines".to_string()),
        (501, soft(501)),
        (1000, soft(1000)),
        (1001, over(1001)),
        (1500, over(1500)),
    ];

    for (lines, expected) in cases {
        // Act
        let fx = Fixture::new(&format!("size{lines}"), "feat/x");
        fx.commit_lines("data.txt", lines);
        let got = fx
            .run(&FakeGh::default(), None, None)
            .expect("within the hard limit");

        // Assert
        assert!(
            has_line(&got, &expected),
            "{lines} lines: expected {expected:?} in\n{got}"
        );
    }
}

#[test]
fn more_than_1500_lines_is_a_hard_abort_naming_the_limit() {
    // Arrange
    let fx = Fixture::new("size1501", "feat/x");
    fx.commit_lines("data.txt", 1501);

    // Act
    let got = fx.run(&FakeGh::default(), None, None);

    // Assert
    let err = got.expect_err("over the hard limit");
    assert!(err.contains("1500-line hard size limit"), "got {err}");
}

#[test]
fn a_clean_tree_reports_ok_and_a_dirty_one_counts_the_files() {
    // Arrange
    let clean = Fixture::new("clean", "feat/x");
    clean.commit_lines("a.txt", 2);
    let dirty = Fixture::new("dirty", "feat/x");
    dirty.commit_lines("a.txt", 2);
    fs::write(dirty.work.join("u1.txt"), "x").expect("u1");
    fs::write(dirty.work.join("u2.txt"), "x").expect("u2");

    // Act
    let clean_out = clean.run(&FakeGh::default(), None, None).expect("clean");
    let dirty_out = dirty.run(&FakeGh::default(), None, None).expect("dirty");

    // Assert
    assert!(has_line(
        &clean_out,
        "VERDICT dirty: OK - nothing uncommitted"
    ));
    assert!(has_line(
        &dirty_out,
        "VERDICT dirty: WARN - 2 uncommitted file(s) will NOT be in the PR"
    ));
}

#[test]
fn test_verdicts_cover_a_test_file_inline_tests_and_none() {
    // Arrange
    let none = Fixture::new("tnone", "feat/x");
    none.commit_lines("data.txt", 2);
    let file = Fixture::new("tfile", "feat/x");
    file.commit_file("tests/foo.rs", "fn main() {}\n");
    let inline = Fixture::new("tinline", "feat/x");
    inline.commit_file("src/lib.rs", "#[test]\nfn it_works() {}\n");

    // Act
    let none_out = none.run(&FakeGh::default(), None, None).expect("none");
    let file_out = file.run(&FakeGh::default(), None, None).expect("file");
    let inline_out = inline.run(&FakeGh::default(), None, None).expect("inline");

    // Assert
    assert!(has_line(
        &none_out,
        "VERDICT tests: NONE - the diff adds no test files or inline test blocks; the readiness criteria expect tests for behaviour changes"
    ));
    assert!(has_line(
        &file_out,
        "VERDICT tests: OK - 1 test file(s) or inline test block(s) touched"
    ));
    assert!(has_line(
        &inline_out,
        "VERDICT tests: OK - 1 test file(s) or inline test block(s) touched"
    ));
}

#[test]
fn the_ticket_comes_from_the_flag_then_the_branch_and_the_line_is_never_omitted() {
    // Arrange
    let from_branch = Fixture::new("tk1", "feat/PROJ-1234-thing");
    from_branch.commit_lines("a.txt", 2);
    let from_flag = Fixture::new("tk2", "feat/PROJ-1234-thing");
    from_flag.commit_lines("a.txt", 2);
    let none = Fixture::new("tk3", "feat/plain");
    none.commit_lines("a.txt", 2);

    // Act
    let branch_out = from_branch
        .run(&FakeGh::default(), None, None)
        .expect("branch");
    let flag_out = from_flag
        .run(&FakeGh::default(), None, Some("ABC-9"))
        .expect("flag");
    let none_out = none.run(&FakeGh::default(), None, None).expect("none");

    // Assert
    assert_eq!(branch_out.lines().last(), Some("ticket=PROJ-1234"));
    assert_eq!(flag_out.lines().last(), Some("ticket=ABC-9"));
    assert_eq!(none_out.lines().last(), Some("ticket="));
}

#[test]
fn the_diff_is_written_to_the_state_dir_and_its_path_is_printed() {
    // Arrange
    let fx = Fixture::new("difffile", "feat/x");
    fx.commit_file("a.txt", "hello\n");

    // Act
    let got = fx.run(&FakeGh::default(), None, None).expect("prepares");

    // Assert
    let expected_path = fx.state().join("pr-diff.txt");
    assert!(
        has_line(&got, &format!("diff_file={}", expected_path.display())),
        "got {got}"
    );
    let diff = fs::read_to_string(&expected_path).expect("diff file exists");
    assert!(diff.contains("+hello"), "diff was {diff}");
}

#[test]
fn an_unknown_base_names_the_missing_remote_ref_instead_of_claiming_nothing_is_ahead() {
    // Arrange
    let fx = Fixture::new("nobase", "feat/x");
    fx.commit_lines("a.txt", 2);

    // Act
    let got = fx.run(&FakeGh::default(), Some("no-such-branch"), None);

    // Assert
    let err = got.expect_err("the base does not exist on origin");
    assert!(err.contains("origin/no-such-branch"), "got {err}");
    assert!(!err.contains("nothing ahead"), "got {err}");
}

#[test]
fn a_failed_fetch_adds_a_warn_line_and_the_run_continues() {
    // Arrange: origin/main exists locally, then origin becomes unreachable.
    let fx = Fixture::new("offline", "feat/x");
    fx.commit_lines("a.txt", 2);
    let gone = fx.bare().with_file_name("gone.git");
    git(
        &fx.work,
        &["remote", "set-url", "origin", gone.to_str().expect("utf8")],
    );

    // Act
    let out = fx.run(&FakeGh::default(), None, None).expect("not fatal");

    // Assert
    let warn = out
        .lines()
        .find(|l| l.starts_with("WARN fetch:"))
        .unwrap_or_else(|| panic!("no WARN fetch line in {out}"));
    assert!(
        warn.starts_with("WARN fetch: could not fetch origin/main (")
            && warn.ends_with("); results use the local ref"),
        "got {warn}"
    );
    let warn_at = out.lines().position(|l| l == warn).unwrap();
    let stat_at = out.lines().position(|l| l == "=== diff stat ===").unwrap();
    assert!(warn_at < stat_at, "the warning belongs with the verdicts");
}

#[test]
fn a_successful_fetch_prints_no_warn_line() {
    // Arrange
    let fx = Fixture::new("online", "feat/x");
    fx.commit_lines("a.txt", 2);

    // Act
    let out = fx.run(&FakeGh::default(), None, None).expect("prepares");

    // Assert
    assert!(!out.lines().any(|l| l.starts_with("WARN fetch:")), "{out}");
}

#[test]
fn a_base_starting_with_a_dash_is_refused() {
    // Arrange
    let fx = Fixture::new("dashbase", "feat/x");
    fx.commit_lines("a.txt", 2);

    // Act
    let err = fx
        .run(&FakeGh::default(), Some("--upload-pack=x"), None)
        .expect_err("refused");

    // Assert
    assert!(err.contains("starts with '-'"), "got {err}");
}

#[test]
fn a_branch_starting_with_a_dash_is_refused() {
    // Arrange
    let fx = Fixture::new("dashbranch", "feat/x");
    fx.commit_lines("a.txt", 2);
    let gh = FakeGh::default();

    // Act
    let err =
        in_dir(&fx.work, || prepare(&fx.state(), "-weird", &gh, None, None)).expect_err("refused");

    // Assert
    assert!(err.contains("starts with '-'"), "got {err}");
    assert_eq!(gh.count("pr_view"), 0, "nothing runs after the refusal");
}

// ---- JSON form ----

use playbook::pr::prepare::{gather, render_json, render_text, JsonInput};
use playbook::pr::rules::RulesText;

fn texts() -> RulesText {
    RulesText {
        writing: "## Loading the references".into(),
        prs: "IRON RULE Commit messages MUST".into(),
        github: "# Prohibited GitHub".into(),
        readiness: "Automated Testing".into(),
    }
}

fn json_of(fx: &Fixture, gh: &FakeGh, base: Option<&str>) -> Result<serde_json::Value, String> {
    let branch = fx.branch();
    in_dir(&fx.work, || {
        let outcome = gather(&fx.state(), &branch, gh, base, None, true)?;
        let root = fx.work.display().to_string();
        let t = texts();
        Ok(render_json(
            &outcome,
            &JsonInput {
                root: &root,
                mode: "ask",
                ready_flag: false,
                draft_default: true,
                issue: None,
                rules: Some(&t),
                rules_error: None,
                max_body_chars: 1200,
            },
        ))
    })
}

#[test]
fn json_a_clean_branch_is_ready_with_its_facts_and_rules() {
    let fx = Fixture::new("json-ready", "feat/x");
    fx.commit_lines("a.txt", 3);
    fx.commit_lines("b.txt", 3);
    fx.commit_lines("c.txt", 3);

    let got = json_of(&fx, &FakeGh::default(), Some("main")).expect("ready");

    assert_eq!(got["verdict"], "READY");
    assert_eq!(got["branch"], "feat/x");
    assert_eq!(got["base"], "main");
    assert_eq!(got["base_source"], "flag");
    assert_eq!(got["commits_ahead"], 3);
    assert_eq!(got["root"], fx.work.display().to_string());
    assert_eq!(got["rules"]["max_body_chars"], 1200);
    assert!(got["rules"]["prs"].as_str().unwrap().contains("IRON RULE"));
    assert_eq!(got["commit_log"].as_array().unwrap().len(), 3);
}

#[test]
fn json_an_open_pr_is_pr_exists() {
    let fx = Fixture::new("json-exists", "feat/x");
    fx.commit_lines("a.txt", 3);
    let gh = FakeGh {
        existing: Some(ExistingPr {
            url: "https://example.test/pr/9".into(),
            state: "OPEN".into(),
        }),
        ..FakeGh::default()
    };

    let got = json_of(&fx, &gh, None).expect("success path");

    assert_eq!(got["verdict"], "PR_EXISTS");
    assert_eq!(got["existing_pr"], "https://example.test/pr/9");
}

#[test]
fn parity_size_limits_pin_the_json_edges() {
    for (lines, size, refused) in [
        (500, "OK", false),
        (501, "SOFT", false),
        (1000, "SOFT", false),
        (1001, "OVER", false),
        (1500, "OVER", false),
        (1501, "", true),
    ] {
        let fx = Fixture::new(&format!("json-size-{lines}"), "feat/x");
        fx.commit_lines("a.txt", lines);

        let got = json_of(&fx, &FakeGh::default(), Some("main"));

        match (refused, got) {
            (true, Err(e)) => assert!(e.contains("hard size limit"), "{e}"),
            (false, Ok(v)) => assert_eq!(v["checks"]["size"], size, "{lines} lines"),
            (r, g) => panic!("{lines}: refused={r} got {g:?}"),
        }
    }
}

#[test]
fn parity_stacked_base_a_branch_on_another_open_pr_head_is_stacked() {
    let fx = Fixture::new("json-stack", "feat/parent");
    fx.commit_lines("a.txt", 3);
    git(&fx.work, &["push", "--quiet", "origin", "feat/parent"]);
    git(&fx.work, &["checkout", "--quiet", "-b", "feat/child"]);
    fx.commit_lines("b.txt", 3);
    let gh = FakeGh {
        heads: vec!["feat/parent".into()],
        ..FakeGh::default()
    };

    let got = json_of(&fx, &gh, None).expect("ready");

    assert_eq!(got["stacked"], true);
    assert_eq!(got["base_source"], "stack");
    assert_eq!(got["base"], "feat/parent");
    assert_eq!(got["commits_ahead"], 1);
}

#[test]
fn text_mode_matches_the_golden_output() {
    let fx = Fixture::new("golden", "feat/x");
    fx.commit_lines("a.txt", 3);
    let branch = fx.branch();
    let state = fx.state();
    let text = in_dir(&fx.work, || {
        render_text(
            &gather(
                &state,
                &branch,
                &FakeGh::default(),
                Some("main"),
                None,
                false,
            )
            .unwrap(),
        )
    });

    let normalised: String = text
        .lines()
        .map(|l| {
            if l.starts_with("state_dir=") {
                "state_dir=<STATE>".to_string()
            } else if l.starts_with("diff_file=") {
                "diff_file=<STATE>/pr-diff.txt".to_string()
            } else if let Some((sha, rest)) = l.split_once(' ') {
                if sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
                    format!("<sha> {rest}")
                } else {
                    l.to_string()
                }
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    let golden = include_str!("golden/pr_prepare_text.txt");
    assert_eq!(normalised.trim_end(), golden.trim_end());
}

#[test]
fn run_json_refuses_conflicting_flags_and_a_wrong_worktree() {
    let fx = Fixture::new("run-json", "feat/x");
    fx.commit_lines("a.txt", 3);
    let gh = FakeGh::default();

    let (conflict, code) =
        playbook::pr::prepare::run_json(&gh, Some("--auto --ask"), None, None, None);
    assert_eq!(conflict["verdict"], "REFUSED");
    assert_eq!(code, 1);

    let phrase = format!("Worktree root: {}, branch: other", fx.work.display());
    let (wrong, code) = playbook::pr::prepare::run_json(&gh, Some(&phrase), None, None, None);
    assert_eq!(wrong["verdict"], "WRONG_DIRECTORY");
    assert_eq!(code, 1);
}
