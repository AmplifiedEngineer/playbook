// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook statusline`: golden output for a few shapes, the behavioural
//! cases ported from the retired shell suite, and a determinism run over
//! every fixture.

mod common;

use common::{bin, have, run, strip_ansi, Scratch};
use playbook::common::par;
use playbook::statusline::{render, Env};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const GREEN: &str = "\x1b[38;2;166;227;161m";
const YELLOW: &str = "\x1b[38;2;249;226;175m";
const RED: &str = "\x1b[38;2;243;139;168m";
const DIM: &str = "\x1b[38;2;127;132;156m";
const ORANGE: &str = "\x1b[38;2;250;179;135m";
const RESET: &str = "\x1b[0m";

fn rust(home: &Path, root: &Path, payload: &str, env: &[(&str, &str)]) -> Vec<u8> {
    let mut cmd = Command::new(bin());
    cmd.arg("statusline");
    run(cmd, home, root, payload, env)
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_.-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// A git repo under `home` with a GitHub origin and an optional cached PR.
fn repo(home: &Path, branch: &str, remote: &str) -> PathBuf {
    let dir = home.join("proj/nested-repo");
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
    };
    git(&["init", "-q", "-b", branch]);
    git(&["remote", "add", "origin", remote]);
    dir
}

fn cache_file(home: &Path, kind: &str, repo: &Path, branch: &str, body: &str) {
    let dir = home.join(".cache/statusline");
    std::fs::create_dir_all(&dir).unwrap();
    let name = format!(
        "{kind}-{}.json",
        slug(&format!("{}::{branch}", repo.display()))
    );
    std::fs::write(dir.join(name), body).unwrap();
}

fn pr_json(state: &str, review: &str, rollup: &str, extra: &str) -> String {
    format!(
        r#"{{"number":42,"author":{{"login":"alice"}},"reviewDecision":"{review}","state":"{state}","mergedAt":"2020-01-02T03:04:05Z","closedAt":"2020-01-03T03:04:05Z","body":"","latestReviews":[],"reviewRequests":[],"statusCheckRollup":{rollup}{extra}}}"#
    )
}

fn payload(cwd: &Path, rest: &str) -> String {
    let rest = if rest.is_empty() {
        String::new()
    } else {
        format!(",{rest}")
    };
    format!(r#"{{"cwd":"{}"{rest}}}"#, cwd.display())
}

const FULL: &str = r#""session_id":"sess-full","model":{"display_name":"Opus 4.8"},"effort":{"level":"high"},"thinking":{"enabled":true},"context_window":{"used_percentage":72.5,"total_input_tokens":250000,"context_window_size":1000000,"current_usage":{"cache_creation_input_tokens":10,"cache_read_input_tokens":990}},"cost":{"total_cost_usd":1.2345,"total_duration_ms":600000},"rate_limits":{"five_hour":{"used_percentage":55.4,"resets_at":1000},"seven_day":{"used_percentage":60}}"#;

/// A reset time far enough ahead (year 2100) to be open whenever the tests run.
const WINDOW_STILL_OPEN: i64 = 4_102_444_800;

// ---- golden output ---------------------------------------------------------

#[test]
fn a_bare_cwd_prints_only_the_collapsed_path() {
    let s = Scratch::new();
    let out = text(&rust(&s.0, &s.0, &payload(&s.0, ""), &[]));
    assert_eq!(out, format!("{GREEN}~{RESET}\n"));
}

#[test]
fn a_full_session_prints_model_context_and_economics_lines() {
    let s = Scratch::new();
    let out = text(&rust(&s.0, &s.0, &payload(&s.0, FULL), &[]));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3);
    let sep = format!("{DIM} | {RESET}");
    let line2 = format!(
        "{ORANGE}Opus 4.8{RESET} {DIM}(high, thinking){RESET}{sep}{DIM}Ctx{RESET} {YELLOW}[███████░░░] 72%{RESET} {DIM}(250k/1000k){RESET} {DIM}→{RESET} {YELLOW}18%{RESET} {DIM}to compact{RESET} {DIM}·{RESET} {RED}⚠ context rot risk{RESET}"
    );
    assert_eq!(lines[1], line2);
    let line3 = format!(
        "{ORANGE}$1.2345{RESET} {DIM}($0.1235/min){RESET}{sep}{DIM}Cache{RESET} {GREEN}99%{RESET}{sep}{DIM}5h{RESET} {YELLOW}55%{RESET}{sep}{DIM}Rate 7d:{RESET} {YELLOW}60%{RESET}"
    );
    assert_eq!(lines[2], line3);
}

#[test]
fn a_failing_pr_prints_the_branch_the_pr_and_the_ci_badge() {
    if !have("git") {
        return;
    }
    let s = Scratch::new();
    let r = repo(&s.0, "chore/x", "https://github.com/testowner/testrepo.git");
    cache_file(
        &s.0,
        "pr",
        &r,
        "chore/x",
        &pr_json(
            "OPEN",
            "APPROVED",
            r#"[{"status":"completed","conclusion":"failure"}]"#,
            "",
        ),
    );
    let out = text(&rust(
        &s.0,
        &s.0,
        &payload(&r, ""),
        &[("STATUSLINE_OSC8", "false")],
    ));
    let line1 = out.lines().next().unwrap();
    let left = format!("{GREEN}~/proj/nested-repo{RESET} {RED}ϓ \x1b]8;;https://github.com/testowner/testrepo/tree/chore/x\x1b\\chore/x\x1b]8;;\x1b\\{RESET} {YELLOW}[$]{RESET}");
    let right = format!(
        "{ORANGE}PR #42{RESET} {RED}CI ✗ 1/1{RESET} {}@alice{RESET} {GREEN}APPROVED{RESET}",
        "\x1b[38;2;205;214;244m"
    );
    assert!(line1.starts_with(&left), "{line1:?}");
    assert!(line1.ends_with(&right), "{line1:?}");
}

// ---- behaviours ported from the retired shell status line tests -------------

#[test]
fn context_rot_shows_at_250k_and_not_at_45k() {
    let s = Scratch::new();
    let body = |tokens: u32, window: u32| {
        payload(
            &s.0,
            &format!(
                r#""session_id":"sess-rot","model":{{"display_name":"Sonnet 4.5"}},"context_window":{{"used_percentage":50,"total_input_tokens":{tokens},"context_window_size":{window}}}"#
            ),
        )
    };
    let hot = text(&rust(&s.0, &s.0, &body(250000, 300000), &[]));
    assert!(hot.contains("context rot risk") && hot.contains("250k/300k"));
    let cold = text(&rust(&s.0, &s.0, &body(45000, 200000), &[]));
    assert!(!cold.contains("context rot risk"));
}

#[test]
fn the_five_hour_quota_lives_on_line_three_not_line_two() {
    let s = Scratch::new();
    let out = text(&rust(&s.0, &s.0, &payload(&s.0, FULL), &[]));
    let lines: Vec<&str> = out.lines().collect();
    assert!(!lines[1].contains("5h"));
    assert!(lines[2].contains("5h"));
}

#[test]
fn a_home_with_glob_metacharacters_still_collapses_to_a_tilde() {
    let s = Scratch::new();
    let home = s.0.join("a[b]c");
    std::fs::create_dir_all(&home).unwrap();
    let out = text(&rust(
        &home,
        &s.0,
        &payload(&home, r#""session_id":"sess-glob""#),
        &[],
    ));
    assert!(
        out.contains('~') && !out.contains(home.to_str().unwrap()),
        "{out:?}"
    );
}

fn telemetry_dir(home: &Path, sid: &str) -> PathBuf {
    home.join(".config/playbook/runtime").join(sid)
}

fn session_payload(home: &Path, sid: &str, used: u32) -> String {
    payload(
        home,
        &format!(
            r#""session_id":"{sid}","cost":{{"total_cost_usd":1.5}},"context_window":{{"used_percentage":{used}}}"#
        ),
    )
}

#[test]
fn a_sample_with_cost_and_usage_is_appended() {
    let s = Scratch::new();
    rust(&s.0, &s.0, &session_payload(&s.0, "sess-sample", 42), &[]);
    let log = std::fs::read_to_string(telemetry_dir(&s.0, "sess-sample").join("telemetry.jsonl"))
        .unwrap();
    assert!(
        log.contains(r#""cost_usd":1.5"#) && log.contains(r#""used_pct":42"#),
        "{log}"
    );
}

#[test]
fn the_capture_marker_follows_the_threshold_and_its_override() {
    let s = Scratch::new();
    rust(&s.0, &s.0, &session_payload(&s.0, "over", 75), &[]);
    assert!(telemetry_dir(&s.0, "over").join("capture-due").is_file());
    rust(&s.0, &s.0, &session_payload(&s.0, "under", 40), &[]);
    assert!(!telemetry_dir(&s.0, "under").join("capture-due").exists());
    rust(
        &s.0,
        &s.0,
        &session_payload(&s.0, "override", 40),
        &[("CC_CAPTURE_AT", "30")],
    );
    assert!(telemetry_dir(&s.0, "override")
        .join("capture-due")
        .is_file());
}

#[test]
fn staying_above_the_threshold_fires_once_and_each_crossing_is_tallied() {
    let s = Scratch::new();
    let dir = telemetry_dir(&s.0, "latch");
    let hi = session_payload(&s.0, "latch", 75);
    let lo = session_payload(&s.0, "latch", 40);
    rust(&s.0, &s.0, &hi, &[]);
    assert!(dir.join("capture-due").is_file());
    std::fs::remove_file(dir.join("capture-due")).unwrap();
    rust(&s.0, &s.0, &hi, &[]);
    rust(&s.0, &s.0, &hi, &[]);
    assert!(!dir.join("capture-due").exists());
    rust(&s.0, &s.0, &lo, &[]);
    rust(&s.0, &s.0, &hi, &[]);
    assert!(dir.join("capture-due").is_file());
    rust(&s.0, &s.0, &lo, &[]);
    rust(&s.0, &s.0, &hi, &[]);
    assert_eq!(
        std::fs::read_to_string(dir.join("capture-crossings")).unwrap(),
        "3"
    );
}

#[test]
fn an_unwritable_session_dir_still_renders() {
    use std::os::unix::fs::PermissionsExt;
    let s = Scratch::new();
    let dir = telemetry_dir(&s.0, "ro");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let out = rust(&s.0, &s.0, &session_payload(&s.0, "ro", 80), &[]);
    assert!(!out.is_empty());
    let is_root = rustix::process::geteuid().is_root();
    assert!(!dir.join("telemetry.jsonl").exists() || is_root);
}

#[test]
fn a_missing_session_id_writes_nothing() {
    let s = Scratch::new();
    let out = rust(
        &s.0,
        &s.0,
        &payload(&s.0, r#""context_window":{"used_percentage":55}"#),
        &[],
    );
    assert!(!out.is_empty());
    assert!(!s.0.join(".config/playbook/runtime").exists());
}

fn ci_out(roots: Option<&str>) -> String {
    let s = Scratch::new();
    let r = repo(
        &s.0,
        "chore/no-ticket",
        "https://github.com/testowner/testrepo.git",
    );
    cache_file(
        &s.0,
        "pr",
        &r,
        "chore/no-ticket",
        &pr_json(
            "OPEN",
            "APPROVED",
            r#"[{"status":"completed","conclusion":"failure"}]"#,
            "",
        ),
    );
    let roots = roots.map(|x| x.replace("HOME", s.0.to_str().unwrap()));
    let mut env = vec![("STATUSLINE_OSC8", "false")];
    if let Some(r) = roots.as_deref() {
        env.push(("STATUSLINE_CI_ROOTS", r));
    }
    text(&rust(&s.0, &s.0, &payload(&r, ""), &env))
}

#[test]
fn the_ci_badge_follows_statusline_ci_roots() {
    if !have("git") {
        return;
    }
    assert!(ci_out(None).contains("CI ✗ 1/1"));
    assert!(!ci_out(Some("/nonexistent-root")).contains("CI ✗"));
    assert!(ci_out(Some("HOME")).contains("CI ✗ 1/1"));
}

fn jira_out(base: Option<&str>) -> String {
    let s = Scratch::new();
    let r = repo(
        &s.0,
        "feature/PROJ-123-thing",
        "https://github.com/testowner/testrepo.git",
    );
    cache_file(
        &s.0,
        "pr",
        &r,
        "feature/PROJ-123-thing",
        &pr_json("OPEN", "APPROVED", "[]", ""),
    );
    let mut env = vec![("STATUSLINE_OSC8", "true")];
    if let Some(b) = base {
        env.push(("STATUSLINE_JIRA_BASE_URL", b));
    }
    text(&rust(&s.0, &s.0, &payload(&r, ""), &env))
}

#[test]
fn the_jira_ticket_links_only_when_a_base_url_is_configured() {
    if !have("git") {
        return;
    }
    let plain = jira_out(None);
    assert!(
        plain.contains("\x1b[38;2;148;226;213mPROJ-123\x1b[0m")
            && !plain.contains("jira.example")
            && !plain.contains("/browse/")
    );
    let linked = jira_out(Some("https://jira.example.test"));
    assert!(linked.contains("https://jira.example.test/browse/PROJ-123"));
    let slash = jira_out(Some("https://jira.example.test/"));
    assert!(
        slash.contains("https://jira.example.test/browse/PROJ-123") && !slash.contains("//browse")
    );
}

// ---- time-dependent segments, with an injected clock -----------------------

fn env_for(home: &Path) -> Env {
    env_with_columns(home, None)
}

/// A clean environment rooted at `home`, with COLUMNS set when given.
fn env_with_columns(home: &Path, columns: Option<&str>) -> Env {
    let home = home.to_str().unwrap();
    let mut pairs = vec![
        ("HOME", home),
        ("CLAUDE_CONFIG_DIR", home),
        ("PWD", home),
        ("PATH", ""),
    ];
    if let Some(c) = columns {
        pairs.push(("COLUMNS", c));
    }
    Env::from_pairs(pairs)
}

#[test]
fn session_age_and_the_five_hour_countdown_use_the_given_clock() {
    let s = Scratch::new();
    let dir = telemetry_dir(&s.0, "age");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("start-ts"), "1000\n").unwrap();
    let body = payload(
        &s.0,
        r#""session_id":"age","rate_limits":{"five_hour":{"used_percentage":10,"resets_at":10000}}"#,
    );
    let out = text(&render(&body, &env_for(&s.0), 1000 + 3660));
    assert!(
        out.contains("Up\x1b[0m \x1b[38;2;166;227;161m1h1m"),
        "{out:?}"
    );
    assert!(out.contains("(1h29m left)"), "{out:?}");
    let young = text(&render(&body, &env_for(&s.0), 1000 + 599));
    assert!(!young.contains("Up"));
}

fn compact_line(window: Option<&str>) -> String {
    let s = Scratch::new();
    let body = payload(
        &s.0,
        r#""context_window":{"used_percentage":80,"context_window_size":1000000}"#,
    );
    let mut pairs = vec![
        ("HOME", s.0.to_str().unwrap()),
        ("PWD", s.0.to_str().unwrap()),
        ("PATH", ""),
    ];
    if let Some(w) = window {
        pairs.push(("CLAUDE_CODE_AUTO_COMPACT_WINDOW", w));
    }
    text(&render(&body, &Env::from_pairs(pairs), 1000))
}

#[test]
fn a_configured_compact_window_sets_the_gap_from_the_model_window() {
    let out = compact_line(Some("850000"));
    assert!(out.contains("5%\x1b"), "{out:?}");
    assert!(!out.contains("10%\x1b"), "{out:?}");
    assert!(out.contains("to compact"), "{out:?}");
}

#[test]
fn a_usage_past_the_configured_compact_window_says_compacting_next_turn() {
    let out = compact_line(Some("800000"));
    assert!(out.contains("COMPACTING NEXT TURN"), "{out:?}");
}

#[test]
fn an_unset_compact_window_keeps_the_ninety_percent_default() {
    let out = compact_line(None);
    assert!(out.contains("10%\x1b"), "{out:?}");
}

#[test]
fn a_non_integer_reset_drops_the_whole_five_hour_segment() {
    let s = Scratch::new();
    let body = payload(
        &s.0,
        r#""rate_limits":{"five_hour":{"used_percentage":10,"resets_at":"2026-09-28T12:00:00Z"}}"#,
    );
    assert!(!text(&render(&body, &env_for(&s.0), 5)).contains("5h"));
}

// ---- fixtures --------------------------------------------------------------

struct Case {
    name: &'static str,
    branch: &'static str,
    remote: &'static str,
    cache: Option<(&'static str, String)>,
    rest: &'static str,
    env: Vec<(&'static str, &'static str)>,
    stdin: Option<&'static str>,
    mode: &'static str,
}

fn case(name: &'static str) -> Case {
    Case {
        name,
        branch: "",
        remote: "",
        cache: None,
        rest: "",
        env: vec![],
        stdin: None,
        mode: "",
    }
}

fn cases() -> Vec<Case> {
    let gh = "https://github.com/testowner/testrepo.git";
    let fail = r#"[{"status":"completed","conclusion":"failure"}]"#;
    let running = r#"[{"status":"in_progress","conclusion":null}]"#;
    let graphql = |nodes: &str| {
        format!(
            r#"{{"data":{{"repository":{{"ref":{{"target":{{"statusCheckRollup":{{"contexts":{{"nodes":{nodes}}}}}}}}}}}}}}}"#
        )
    };
    let reviews = r#","latestReviews":[{"author":{"login":"bob"},"state":"APPROVED","submittedAt":"2020-01-01T00:00:00Z"},{"author":{"login":"carol"},"state":"CHANGES_REQUESTED","submittedAt":"2019-01-01T00:00:00Z"},{"author":{"login":"coderabbitai"},"state":"APPROVED","submittedAt":"2020-01-01T00:00:00Z"}],"reviewRequests":[{"login":"dave"},{"name":"core-team"}]"#;
    let mut v = vec![
        case("empty stdin"),
        Case {
            stdin: Some("not json"),
            ..case("invalid json")
        },
        Case {
            rest: FULL,
            ..case("full session, no repo")
        },
        Case {
            rest: r#""context_window":{"used_percentage":95}"#,
            ..case("compacting next turn")
        },
        Case {
            rest: r#""context_window":{"used_percentage":95}"#,
            env: vec![("CLAUDE_AUTOCOMPACT_PCT_OVERRIDE", "98")],
            ..case("compact override")
        },
        Case {
            rest: r#""context_window":{"used_percentage":85.5,"total_input_tokens":999}"#,
            ..case("red context, small tokens")
        },
        Case {
            rest: r#""model":{"display_name":"M"},"thinking":{"enabled":true}"#,
            ..case("thinking only")
        },
        Case {
            rest: r#""cost":{"total_cost_usd":0},"rate_limits":{"five_hour":{"used_percentage":90.5,"resets_at":5},"seven_day":{"used_percentage":50}}"#,
            ..case("zero cost, hot 5h, cool 7d")
        },
        Case {
            rest: r#""context_window":{"current_usage":{"cache_creation_input_tokens":500,"cache_read_input_tokens":100}}"#,
            ..case("cold cache")
        },
        Case {
            rest: FULL,
            env: vec![
                ("STATUSLINE_SHOW_MODEL", "false"),
                ("STATUSLINE_SHOW_CONTEXT", "false"),
                ("STATUSLINE_SHOW_CACHE_RATIO", "false"),
                ("STATUSLINE_SHOW_RATE_LIMITS", "false"),
            ],
            ..case("segments off")
        },
        Case {
            rest: FULL,
            env: vec![("STATUSLINE_CTX_BAR_WIDTH", "20")],
            ..case("wide bar")
        },
        Case {
            rest: FULL,
            env: vec![("COLUMNS", "2000")],
            ..case("wide terminal")
        },
        Case {
            branch: "main",
            remote: gh,
            rest: "",
            ..case("repo without cache")
        },
        Case {
            branch: "main",
            remote: gh,
            mode: "dirty",
            ..case("dirty repo")
        },
        Case {
            branch: "main",
            remote: "https://gitlab.com/o/r.git",
            ..case("non-github remote")
        },
        Case {
            branch: "main",
            remote: gh,
            env: vec![("STATUSLINE_SHOW_GIT", "false")],
            ..case("git off")
        },
        Case {
            branch: "chore/x",
            remote: gh,
            cache: Some(("pr", pr_json("OPEN", "APPROVED", fail, ""))),
            ..case("pr with failing ci")
        },
        Case {
            branch: "chore/x",
            remote: gh,
            cache: Some(("pr", pr_json("OPEN", "REVIEW_REQUIRED", running, ""))),
            ..case("pr with running ci")
        },
        Case {
            branch: "chore/x",
            remote: gh,
            cache: Some(("pr", pr_json("OPEN", "CHANGES_REQUESTED", "[]", reviews))),
            ..case("pr with reviewers")
        },
        Case {
            branch: "chore/x",
            remote: gh,
            cache: Some(("pr", pr_json("OPEN", "", "[]", ""))),
            ..case("pr without review")
        },
        Case {
            branch: "chore/x",
            remote: gh,
            cache: Some(("pr", pr_json("MERGED", "APPROVED", "[]", reviews))),
            ..case("merged pr")
        },
        Case {
            branch: "chore/x",
            remote: gh,
            cache: Some(("pr", pr_json("CLOSED", "", "[]", ""))),
            ..case("closed pr")
        },
        Case {
            branch: "chore/x",
            remote: gh,
            cache: Some(("pr", pr_json("OPEN", "APPROVED", fail, ""))),
            env: vec![("STATUSLINE_OSC8", "true")],
            ..case("pr with links")
        },
        Case {
            branch: "chore/x",
            remote: gh,
            cache: Some(("pr", pr_json("OPEN", "APPROVED", fail, ""))),
            env: vec![("STATUSLINE_SHOW_PR", "false")],
            ..case("pr off")
        },
        Case {
            branch: "chore/x",
            remote: gh,
            cache: Some(("pr", pr_json("OPEN", "APPROVED", fail, ""))),
            env: vec![("STATUSLINE_SHOW_CI", "false")],
            ..case("ci off")
        },
        Case {
            branch: "feature/PROJ-123-x",
            remote: gh,
            cache: Some(("pr", pr_json("OPEN", "APPROVED", "[]", ""))),
            env: vec![
                ("STATUSLINE_OSC8", "true"),
                ("STATUSLINE_JIRA_BASE_URL", "https://j.example/"),
            ],
            ..case("jira from branch")
        },
        Case {
            branch: "chore/x",
            remote: gh,
            cache: Some((
                "pr",
                pr_json("OPEN", "APPROVED", "[]", r#","body":"see ABC-9 and DEF-1""#)
                    .replace(r#""body":"","#, ""),
            )),
            ..case("jira from body")
        },
        Case {
            branch: "chore/x",
            remote: gh,
            cache: Some((
                "ci",
                graphql(
                    r#"[{"__typename":"CheckRun","status":"COMPLETED","conclusion":"FAILURE"}]"#,
                ),
            )),
            ..case("standalone failing ci")
        },
        Case {
            branch: "chore/x",
            remote: gh,
            cache: Some((
                "ci",
                graphql(r#"[{"__typename":"StatusContext","state":"PENDING"}]"#),
            )),
            ..case("standalone running ci")
        },
        Case {
            branch: "chore/x",
            remote: gh,
            cache: Some((
                "ci",
                graphql(r#"[{"__typename":"StatusContext","state":"SUCCESS"}]"#),
            )),
            ..case("standalone passing ci")
        },
    ];
    v.push(Case {
        branch: "main",
        remote: gh,
        mode: "unstaged",
        ..case("unstaged change")
    });
    v.push(Case {
        branch: "main",
        remote: gh,
        mode: "detached",
        ..case("detached head")
    });
    v.push(Case {
        rest: r#""rate_limits":{"five_hour":{"used_percentage":10,"resets_at":"2026-09-28T12:00:00Z"}}"#,
        ..case("non-integer reset")
    });
    v.push(Case {
        stdin: Some(r#"{"cwd":"/tmp/a\\cb"}"#),
        ..case("backslash c in cwd")
    });
    v.push(Case {
        stdin: Some(r#"{"cwd":"/tmp/x\\ny\\tz"}"#),
        ..case("backslashes in cwd")
    });
    v
}

fn build(c: &Case, home: &Path) -> (PathBuf, String) {
    let cwd = if c.branch.is_empty() {
        home.to_path_buf()
    } else {
        repo(home, c.branch, c.remote)
    };
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(&cwd)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
    };
    match c.mode {
        "dirty" => {
            std::fs::write(cwd.join("f"), "x").unwrap();
            git(&["add", "f"]);
        }
        "unstaged" => {
            std::fs::write(cwd.join("f"), "x").unwrap();
            git(&["add", "f"]);
            git(&["commit", "-q", "-m", "x"]);
            std::fs::write(cwd.join("f"), "y").unwrap();
        }
        "detached" => {
            git(&["commit", "-q", "--allow-empty", "-m", "x"]);
            git(&["checkout", "-q", "--detach"]);
        }
        _ => {}
    }
    if let Some((kind, body)) = &c.cache {
        cache_file(home, kind, &cwd, c.branch, body);
    }
    let stdin = match c.stdin {
        Some(s) => s.to_string(),
        None if c.name == "empty stdin" => String::new(),
        None => payload(&cwd, c.rest),
    };
    (cwd, stdin)
}

/// Renders `c` in two fresh homes and describes the difference, if any.
fn renders_differ(c: &Case, days: &regex::Regex) -> Option<String> {
    let a = Scratch::new();
    let b = Scratch::new();
    let (_, in_a) = build(c, &a.0);
    let (_, in_b) = build(c, &b.0);
    // Homes differ per run and ages tick over midnight, so normalise both.
    let norm = |out: Vec<u8>, home: &Path| {
        let t = text(&out).replace(home.to_str().unwrap(), "<HOME>");
        days.replace_all(&t, "Nd ago").into_owned()
    };
    let first = norm(rust(&a.0, &a.0, &in_a, &c.env), &a.0);
    let second = norm(rust(&b.0, &b.0, &in_b, &c.env), &b.0);
    (first != second).then(|| format!("{}\n  first:  {first:?}\n  second: {second:?}", c.name))
}

#[test]
fn every_fixture_renders_the_same_bytes_on_two_runs() {
    let days = regex::Regex::new(r"\d+d ago").unwrap();
    // Eight at a time: each fixture runs git and the binary several times,
    // and one after another they took 11 s on a macOS runner.
    let failures: Vec<String> = par::map(&cases(), 8, |c| renders_differ(c, &days))
        .into_iter()
        .flatten()
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

// ---- rate limit history ----------------------------------------------------

fn statusline_status(home: &Path, payload: &str) -> std::process::ExitStatus {
    use std::io::Write;
    let mut child = Command::new(bin())
        .arg("statusline")
        .env_clear()
        .env("HOME", home)
        .env("PWD", home)
        .env("PATH", "/usr/bin:/bin")
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    child.wait().unwrap()
}

#[test]
fn a_reading_adds_one_history_line_and_a_second_one_right_after_adds_none() {
    let s = Scratch::new();
    let data = s.0.join(".config/playbook/data");
    // The window must still be open, or the merge ignores the reading.
    let body = payload(
        &s.0,
        &FULL.replace(
            r#""resets_at":1000"#,
            &format!(r#""resets_at":{WINDOW_STILL_OPEN}"#),
        ),
    );

    assert!(statusline_status(&s.0, &body).success());
    assert!(statusline_status(&s.0, &body).success());

    let history = std::fs::read_to_string(data.join("rate_limits.jsonl")).unwrap();
    assert_eq!(history.lines().count(), 1, "{history}");
    assert!(history.contains(r#""five_hour":55.4"#), "{history}");
    assert!(!history.contains("sess-full"), "{history}");
}

#[test]
fn an_unwritable_data_folder_still_exits_zero() {
    use std::os::unix::fs::PermissionsExt;
    let s = Scratch::new();
    let data = s.0.join(".config/playbook/data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o500)).unwrap();

    let status = statusline_status(&s.0, &payload(&s.0, FULL));

    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(status.success(), "{status:?}");
}

#[test]
fn a_history_file_that_cannot_be_appended_to_still_exits_zero_and_is_left_alone() {
    use std::os::unix::fs::PermissionsExt;
    let s = Scratch::new();
    let data = s.0.join(".config/playbook/data");
    std::fs::create_dir_all(&data).unwrap();
    let file = data.join("rate_limits.jsonl");
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let before = format!(
        "{{\"t\":{},\"five_hour\":1.0,\"seven_day\":null}}\n",
        secs - 1000
    );
    std::fs::write(&file, &before).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o400)).unwrap();
    // a user that can write anyway (root) is not the case under test
    let can_write = std::fs::OpenOptions::new().append(true).open(&file).is_ok();

    let status = statusline_status(&s.0, &payload(&s.0, FULL));

    assert!(status.success(), "{status:?}");
    assert!(can_write || std::fs::read_to_string(&file).unwrap() == before);
}

// ---- line 4: budget projections --------------------------------------------

const BUDGET_NOW: i64 = 1_000_000;
const DAY_SECS: i64 = 86_400;
const SEVEN_DAYS_SECS: i64 = 7 * DAY_SECS;

const FIVE_HOURS_SECS: i64 = 5 * 3600;

/// The colour-stripped lines for the given `rate_limits` members at a fixed
/// clock.
fn budget_lines(members: &str, columns: Option<&str>) -> Vec<String> {
    let s = Scratch::new();
    let body = payload(&s.0, &format!(r#""rate_limits":{{{members}}}"#));
    let out = render(&body, &env_with_columns(&s.0, columns), BUDGET_NOW);
    strip_ansi(&text(&out))
        .lines()
        .map(str::to_string)
        .collect()
}

/// A window member that is `used` percent spent after `elapsed` seconds of a
/// window that is `length` seconds long.
fn window(key: &str, used: f64, elapsed: i64, length: i64) -> String {
    let resets_at = BUDGET_NOW + length - elapsed;
    format!(r#""{key}":{{"used_percentage":{used},"resets_at":{resets_at}}}"#)
}

/// The colour-stripped lines for a 7 day window at `used` percent that has run
/// for `elapsed` seconds of its seven days, at a fixed clock.
fn seven_day_lines(used: f64, elapsed: i64, columns: Option<&str>) -> Vec<String> {
    budget_lines(
        &window("seven_day", used, elapsed, SEVEN_DAYS_SECS),
        columns,
    )
}

/// The same for a 5 hour window.
fn five_hour_lines(used: f64, elapsed: i64) -> Vec<String> {
    budget_lines(&window("five_hour", used, elapsed, FIVE_HOURS_SECS), None)
}

/// Both windows alarming: 7d cap in 2d3h and 5h cap in 1h20m.
fn both_alarm_lines(columns: Option<&str>) -> Vec<String> {
    let members = format!(
        "{},{}",
        window("seven_day", 60.0, 275_400, SEVEN_DAYS_SECS),
        window("five_hour", 60.0, 2 * 3600, FIVE_HOURS_SECS),
    );
    budget_lines(&members, columns)
}

#[test]
fn a_reached_seven_day_cap_adds_a_fourth_line_and_leaves_line_three_alone() {
    let lines = seven_day_lines(100.0, 3 * DAY_SECS, None);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[1], "Rate 7d: 100%");
    assert_eq!(lines[2], "! 7d CAP HIT");
}

#[test]
fn a_seven_day_pace_past_the_cap_names_the_time_left() {
    // 60 percent after 3 of 7 days is 20 percent a day, so 40 left lasts 2 days.
    let lines = seven_day_lines(60.0, 3 * DAY_SECS, None);
    assert_eq!(lines.last().unwrap(), "! 7d cap in 2d", "{lines:?}");
    assert_eq!(lines[1], "Rate 7d: 60%");
}

#[test]
fn a_seven_day_window_within_the_cap_adds_no_line() {
    // Only the first line prints, so a fourth would show as a longer output.
    for (used, elapsed) in [(0.0, 3 * DAY_SECS), (49.9, 3 * DAY_SECS + DAY_SECS / 2)] {
        let lines = seven_day_lines(used, elapsed, None);
        assert_eq!(lines.len(), 1, "{lines:?}");
    }
}

#[test]
fn a_seven_day_window_already_reset_adds_no_line() {
    let s = Scratch::new();
    let body = payload(
        &s.0,
        r#""rate_limits":{"seven_day":{"used_percentage":100,"resets_at":1000}}"#,
    );
    let out = strip_ansi(&text(&render(&body, &env_for(&s.0), BUDGET_NOW)));
    assert_eq!(out.lines().count(), 2, "{out:?}");
}

#[test]
fn expired_or_undated_windows_keep_the_three_line_layout() {
    // In FULL the 5 hour window reset long before the clock and the 7 day
    // window has no reset time, so neither can make a budget line.
    let s = Scratch::new();
    let body = payload(&s.0, FULL);
    let out = strip_ansi(&text(&render(&body, &env_for(&s.0), BUDGET_NOW)));
    assert_eq!(out.lines().count(), 3, "{out:?}");
}

#[test]
fn the_seven_day_alarm_survives_any_terminal_width() {
    for columns in [None, Some("5"), Some("not-a-number")] {
        let lines = seven_day_lines(100.0, 3 * DAY_SECS, columns);
        assert_eq!(lines.last().unwrap(), "! 7d CAP HIT", "{columns:?}");
    }
}

#[test]
fn the_budget_line_is_red_and_never_relies_on_colour_alone() {
    let s = Scratch::new();
    let body = payload(
        &s.0,
        &format!(
            r#""rate_limits":{{"seven_day":{{"used_percentage":100,"resets_at":{}}}}}"#,
            BUDGET_NOW + DAY_SECS
        ),
    );
    let out = text(&render(&body, &env_for(&s.0), BUDGET_NOW));
    assert!(
        out.ends_with(&format!("{RED}! 7d CAP HIT{RESET}\n")),
        "{out:?}"
    );
}

#[test]
fn a_reached_five_hour_cap_adds_a_fourth_line() {
    let lines = five_hour_lines(100.0, 3600);
    assert_eq!(lines.last().unwrap(), "! 5h CAP HIT", "{lines:?}");
}

#[test]
fn a_five_hour_pace_past_the_cap_names_the_time_left() {
    // 60 percent after 2 hours is 30 percent an hour, so 40 left lasts 1h20m.
    let lines = five_hour_lines(60.0, 2 * 3600);
    assert_eq!(lines.last().unwrap(), "! 5h cap in 1h20m", "{lines:?}");
}

#[test]
fn a_five_hour_window_under_an_hour_in_adds_no_line() {
    let lines = five_hour_lines(60.0, 1800);
    assert_eq!(lines.len(), 2, "{lines:?}");
}

#[test]
fn both_alarms_share_line_four_with_the_seven_day_first() {
    let lines = both_alarm_lines(None);
    assert_eq!(
        lines.last().unwrap(),
        "! 7d cap in 2d3h | ! 5h cap in 1h20m",
        "{lines:?}"
    );
}

#[test]
fn the_five_hour_alarm_is_dropped_whole_when_the_line_is_too_narrow() {
    // The joined line needs 36 columns; the 5h alarm has no short form.
    let both = "! 7d cap in 2d3h | ! 5h cap in 1h20m";
    let lines = both_alarm_lines(Some("36"));
    assert_eq!(lines.last().unwrap(), both, "{lines:?}");
    let lines = both_alarm_lines(Some("35"));
    assert_eq!(lines.last().unwrap(), "! 7d cap in 2d3h", "{lines:?}");
}

// ---- line 4: the Enterprise monthly spend limit ----------------------------

/// The Enterprise usage cache shape: no windows, a 1500.00 USD limit, 986.40
/// used, fetched at `FETCHED`.
const ENTERPRISE_CACHE: &str =
    include_str!("../../telemetry/tests/fixtures/spend/enterprise-usage-based.json");
/// `fetchedAtMs` of the fixture in seconds: 2026-09-03 UTC.
const FETCHED: i64 = 1_788_400_000;
const HOUR_SECS: i64 = 3600;
/// The 10d14h age of the owner's stale cache.
const STALE_AGE: i64 = 10 * DAY_SECS + 14 * HOUR_SECS;
/// The longest `.claude.json` that is read: 512 KiB.
const MAX_CONFIG_BYTES: usize = 512 * 1024;

/// Colour-stripped output lines for a payload at `now`, with `cache` as the
/// Claude config file and `budget` as `usage.budget.monthlyUsd` when given.
fn monthly_lines(
    cache: Option<&str>,
    budget: Option<u32>,
    members: &str,
    now: i64,
    columns: Option<&str>,
) -> Vec<String> {
    let s = Scratch::new();
    if let Some(cache) = cache {
        std::fs::write(s.0.join(".claude.json"), cache).unwrap();
    }
    if let Some(usd) = budget {
        let dir = s.0.join(".config/playbook");
        std::fs::create_dir_all(&dir).unwrap();
        let body = format!(r#"{{"usage":{{"budget":{{"monthlyUsd":{usd}}}}}}}"#);
        std::fs::write(dir.join("config.json"), body).unwrap();
    }
    let body = payload(&s.0, members);
    let out = render(&body, &env_with_columns(&s.0, columns), now);
    strip_ansi(&text(&out))
        .lines()
        .map(str::to_string)
        .collect()
}

fn enterprise(budget: Option<u32>, age: i64) -> Vec<String> {
    monthly_lines(Some(ENTERPRISE_CACHE), budget, "", FETCHED + age, None)
}

#[test]
fn a_fresh_enterprise_cache_on_pace_to_pass_the_limit_alarms_with_the_time_left() {
    let lines = enterprise(None, HOUR_SECS);
    assert_eq!(
        lines.last().unwrap(),
        "! $986/$1500 mo, cap in 1d",
        "{lines:?}"
    );
}

#[test]
fn a_lower_own_budget_is_the_limit_and_says_so() {
    let lines = enterprise(Some(1000), HOUR_SECS);
    assert_eq!(
        lines.last().unwrap(),
        "! $986/$1000 mo budget, cap in <1m",
        "{lines:?}"
    );
    let lines = enterprise(Some(900), HOUR_SECS);
    assert_eq!(
        lines.last().unwrap(),
        "! $986/$900 mo budget CAP HIT",
        "{lines:?}"
    );
}

#[test]
fn a_stale_cache_shows_its_age_and_makes_no_projection() {
    let lines = enterprise(None, STALE_AGE);
    assert_eq!(
        lines.last().unwrap(),
        "$986/$1500 mo (10d14h old)",
        "{lines:?}"
    );
    let lines = enterprise(Some(1000), STALE_AGE);
    assert_eq!(
        lines.last().unwrap(),
        "$986/$1000 mo budget (10d14h old)",
        "{lines:?}"
    );
}

#[test]
fn a_stale_cache_over_an_own_budget_still_says_the_cap_was_hit() {
    // Spend only grows within a month, so a stale figure over the limit holds.
    let lines = enterprise(Some(900), STALE_AGE);
    assert_eq!(
        lines.last().unwrap(),
        "! $986/$900 mo budget CAP HIT (10d14h old)",
        "{lines:?}"
    );
}

#[test]
fn a_cache_from_an_earlier_month_adds_no_line() {
    let lines = enterprise(None, 40 * DAY_SECS);
    assert_eq!(lines.len(), 1, "{lines:?}");
}

#[test]
fn an_enterprise_seat_never_gets_a_five_hour_or_not_known_note() {
    for age in [HOUR_SECS, STALE_AGE] {
        let lines = enterprise(None, age).join("\n");
        assert!(
            !lines.contains("5h") && !lines.contains("not known"),
            "{lines}"
        );
    }
}

#[test]
fn a_missing_or_corrupt_config_file_adds_no_line() {
    for cache in [None, Some("{not json"), Some("{}")] {
        let lines = monthly_lines(cache, None, "", FETCHED + HOUR_SECS, None);
        assert_eq!(lines.len(), 1, "{cache:?} {lines:?}");
    }
}

/// The fixture padded with a string member to exactly `len` bytes.
fn padded(len: usize) -> String {
    let base = ENTERPRISE_CACHE.trim_end();
    let head = &base[..base.len() - 1];
    let fill = len - head.len() - r#","pad":"""#.len() - 1;
    format!(r#"{head},"pad":"{}"}}"#, "x".repeat(fill))
}

#[test]
fn a_config_file_over_the_size_cap_is_skipped_and_one_at_the_cap_is_read() {
    let at_cap = monthly_lines(
        Some(&padded(MAX_CONFIG_BYTES)),
        None,
        "",
        FETCHED + HOUR_SECS,
        None,
    );
    assert_eq!(at_cap.len(), 2, "{at_cap:?}");
    let over = monthly_lines(
        Some(&padded(MAX_CONFIG_BYTES + 1)),
        None,
        "",
        FETCHED + HOUR_SECS,
        None,
    );
    assert_eq!(over.len(), 1, "{over:?}");
}

#[test]
fn a_gateway_figure_on_stdin_wins_over_the_cache() {
    let now = FETCHED + HOUR_SECS;
    let members = format!(
        r#""rate_limits":{{"spend_limit":{{"used_percentage":82,"resets_at":{},"used_usd":410,"limit_usd":500,"period":"monthly"}}}}"#,
        now + 10 * DAY_SECS
    );
    let lines = monthly_lines(Some(ENTERPRISE_CACHE), None, &members, now, None);
    assert!(
        lines.last().unwrap().starts_with("! $410/$500 mo, cap in"),
        "{lines:?}"
    );
}

#[test]
fn the_monthly_figure_drops_its_words_before_the_seven_day_alarm_goes() {
    let now = FETCHED + HOUR_SECS;
    let members = format!(
        r#""rate_limits":{{"seven_day":{{"used_percentage":60,"resets_at":{}}}}}"#,
        now + 7 * DAY_SECS - 275_400
    );
    let seven = "! 7d cap in 2d3h";
    let wide = monthly_lines(Some(ENTERPRISE_CACHE), None, &members, now, None);
    assert_eq!(
        wide.last().unwrap(),
        &format!("{seven} | ! $986/$1500 mo, cap in 1d")
    );
    let narrow = monthly_lines(Some(ENTERPRISE_CACHE), None, &members, now, Some("36"));
    assert_eq!(
        narrow.last().unwrap(),
        &format!("{seven} | ! $986/$1500 mo")
    );
    let tight = monthly_lines(Some(ENTERPRISE_CACHE), None, &members, now, Some("20"));
    assert_eq!(tight.last().unwrap(), seven);
}

#[test]
fn a_gateway_figure_needs_no_config_file_and_ignores_a_corrupt_one() {
    let now = FETCHED + HOUR_SECS;
    let members = format!(
        r#""rate_limits":{{"spend_limit":{{"used_percentage":82,"resets_at":{},"used_usd":600,"limit_usd":500,"period":"monthly"}}}}"#,
        now + 10 * DAY_SECS
    );
    for cache in [None, Some("{not json")] {
        let lines = monthly_lines(cache, None, &members, now, None);
        assert_eq!(lines.last().unwrap(), "! $600/$500 mo CAP HIT", "{lines:?}");
    }
}

#[test]
fn the_config_file_is_found_under_claude_config_dir_or_else_home() {
    let s = Scratch::new();
    let cfg = s.0.join("cfg");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(cfg.join(".claude.json"), ENTERPRISE_CACHE).unwrap();
    let home = s.0.to_str().unwrap();
    let body = payload(&s.0, "");
    let now = FETCHED + HOUR_SECS;
    let last = |pairs: Vec<(&str, &str)>| {
        let out = render(&body, &Env::from_pairs(pairs), now);
        strip_ansi(&text(&out)).lines().last().unwrap().to_string()
    };
    let moved = last(vec![
        ("HOME", home),
        ("CLAUDE_CONFIG_DIR", cfg.to_str().unwrap()),
    ]);
    assert_eq!(moved, "! $986/$1500 mo, cap in 1d");
    // With the variable unset the file is looked for under HOME, where it is not.
    let unset = last(vec![("HOME", home)]);
    assert_ne!(unset, moved);
    std::fs::write(s.0.join(".claude.json"), ENTERPRISE_CACHE).unwrap();
    assert_eq!(last(vec![("HOME", home)]), moved);
}

#[test]
fn a_cache_is_paced_from_its_fetch_time_not_from_now() {
    // 986.40 used 2d1h into the month, read 6 hours later against a 13000 limit.
    // Paced from the fetch the limit is passed. Paced from now it would only be
    // near, and the alarm would be lost.
    let big = ENTERPRISE_CACHE.replace("150000", "1300000");
    let lines = monthly_lines(Some(&big), None, "", FETCHED + 6 * HOUR_SECS, None);
    assert_eq!(
        lines.last().unwrap(),
        "! $986/$13000 mo, cap in 25d",
        "{lines:?}"
    );
}

// ---- line 4: a manual budget against this machine's own spend --------------

/// The file the local month total is kept in, under the statusline cache.
const LOCAL_TOTAL_FILE: &str = "monthly-local-total";
/// Seconds a stored local total is trusted.
const LOCAL_TOTAL_TTL: i64 = 10;

/// A home with `usage.monthlyUsd` 1000 and one usage event 1 hour before
/// `FETCHED + HOUR_SECS`, in the current month at that time.
fn local_home(db: bool) -> (Scratch, f64) {
    let s = Scratch::new();
    let dir = s.0.join(".config/playbook");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.json"),
        r#"{"usage":{"budget":{"monthlyUsd":1000}}}"#,
    )
    .unwrap();
    let mut event = playbook::usage::UsageEvent {
        event_id: "e1".to_string(),
        timestamp: FETCHED,
        account: "a".to_string(),
        agent: "claude-code".to_string(),
        model: "claude-opus-4-1".to_string(),
        input_tokens: 1_000_000,
        ..playbook::usage::UsageEvent::default()
    };
    event.apply_pricing();
    if db {
        let path = playbook::usage::data::usage_db(&s.0);
        let conn = playbook::usage::db::open_db(&path).unwrap();
        playbook::usage::db::upsert_usage_event(&conn, &event).unwrap();
    }
    (s, event.cost_usd)
}

fn lines_of(s: &Scratch, now: i64) -> Vec<String> {
    let body = payload(&s.0, "");
    strip_ansi(&text(&render(&body, &env_for(&s.0), now)))
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn a_manual_budget_alone_shows_this_machines_spend_marked_local() {
    let (s, cost) = local_home(true);
    let lines = lines_of(&s, FETCHED + HOUR_SECS);
    let want = format!("${cost:.0}/$1000 mo budget (local)");
    assert!(lines.last().unwrap().starts_with(&want), "{lines:?}");
}

#[test]
fn without_a_usage_database_a_manual_budget_still_shows_nothing() {
    let (s, _) = local_home(false);
    let lines = lines_of(&s, FETCHED + HOUR_SECS);
    assert!(!lines.join("\n").contains("mo budget"), "{lines:?}");
    assert!(!s.0.join(".config/playbook/data/usage.db").exists());
}

#[test]
fn the_claude_cache_keeps_priority_over_local_spend() {
    let (s, _) = local_home(true);
    std::fs::write(s.0.join(".claude.json"), ENTERPRISE_CACHE).unwrap();
    let lines = lines_of(&s, FETCHED + HOUR_SECS);
    assert_eq!(
        lines.last().unwrap(),
        "! $986/$1000 mo budget, cap in <1m",
        "{lines:?}"
    );
}

#[test]
fn the_gateway_reading_keeps_priority_over_local_spend() {
    let (s, _) = local_home(true);
    let now = FETCHED + HOUR_SECS;
    let members = format!(
        r#""rate_limits":{{"spend_limit":{{"used_percentage":82,"resets_at":{},"used_usd":410,"limit_usd":500,"period":"monthly"}}}}"#,
        now + 10 * DAY_SECS
    );
    let body = payload(&s.0, &members);
    let out = strip_ansi(&text(&render(&body, &env_for(&s.0), now)));
    assert!(out.contains("$410/$500 mo"), "{out}");
    assert!(!out.contains("(local)"), "{out}");
}

#[test]
fn the_local_total_is_reused_for_ten_seconds_without_reading_the_database() {
    let (s, cost) = local_home(true);
    let now = FETCHED + HOUR_SECS;
    let want = format!("${cost:.0}/$1000 mo budget (local)");
    assert!(lines_of(&s, now).last().unwrap().starts_with(&want));
    assert!(s
        .0
        .join(".cache/statusline")
        .join(LOCAL_TOTAL_FILE)
        .is_file());
    std::fs::remove_file(playbook::usage::data::usage_db(&s.0)).unwrap();
    let hit = lines_of(&s, now + LOCAL_TOTAL_TTL - 1);
    assert!(hit.last().unwrap().starts_with(&want), "{hit:?}");
    let miss = lines_of(&s, now + LOCAL_TOTAL_TTL);
    assert!(!miss.join("\n").contains("mo budget"), "{miss:?}");
}
