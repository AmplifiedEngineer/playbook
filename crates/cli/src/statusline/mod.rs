// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook statusline`: a port of `statusline.sh`. Reads Claude Code's
//! session JSON on stdin and prints ANSI lines. Lines 1 to 3 match the shell
//! script byte for byte. Line 4 is new in the Rust port and shows only when a
//! usage cap needs saying. Not wired into any settings file yet; the shell
//! script is still what runs.

pub mod budget;
mod fmt;
pub(crate) mod gh;
mod git;
mod lines;
mod telemetry;

use crate::common::slug::slugify;
use crate::json::statusline as pr_json;
use fmt::*;
use lines::{Opts, Session};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Environment as the render sees it. An empty value counts as unset, like the
/// script's `${VAR:-default}`.
pub struct Env {
    vars: HashMap<String, String>,
}

impl Env {
    pub fn from_process() -> Self {
        Self::from_pairs(std::env::vars())
    }

    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Env {
            vars: pairs
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    fn flag(&self, key: &str) -> bool {
        self.get(key).unwrap_or("true") == "true"
    }

    fn int(&self, key: &str, default: i64) -> i64 {
        self.get(key).map_or(default, int_part)
    }
}

/// Reads stdin, renders, prints. Always exits 0 so the line never breaks.
pub fn run() -> i32 {
    let mut raw = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut raw);
    let input = String::from_utf8_lossy(&raw);
    // Plugin disabled in Claude Code: print nothing.
    if crate::common::plugin_state::paused() {
        return 0;
    }
    let now = crate::common::time::now_secs();
    remember_rate_limits(&input, now);
    let out = render(&input, &Env::from_process(), now);
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(&out);
    let _ = stdout.flush();
    0
}

/// Keeps the plan limits Claude Code passes in (`rate_limits`, Pro and Max or
/// a gateway only) for `playbook usage`. Best effort and local: only
/// percentages, reset times and the gateway dollar limit are stored. A reading
/// that carries one window keeps the other as stored.
fn remember_rate_limits(input: &str, now: i64) {
    if !input.contains("rate_limits") {
        return;
    }
    crate::usage::limits_source::remember(&crate::common::paths::usage_db_dir(), input, now);
}

/// Whether the terminal is known to support OSC 8 hyperlinks.
fn supports_osc8(env: &Env) -> bool {
    match env.get("STATUSLINE_OSC8") {
        Some("true" | "1") => return true,
        Some("false" | "0") => return false,
        _ => {}
    }
    if matches!(
        env.get("TERM_PROGRAM"),
        Some("ghostty" | "iTerm.app" | "WezTerm" | "vscode" | "Hyper")
    ) || matches!(env.get("TERM"), Some("xterm-kitty" | "foot" | "foot-extra"))
    {
        return true;
    }
    if [
        "GHOSTTY_RESOURCES_DIR",
        "KITTY_WINDOW_ID",
        "WEZTERM_EXECUTABLE",
    ]
    .iter()
    .any(|k| env.get(k).is_some())
    {
        return true;
    }
    env.get("VTE_VERSION").is_some_and(|v| {
        v.bytes().all(|b| b.is_ascii_digit()) && v.parse::<i64>().unwrap_or(0) >= 5000
    })
}

/// Pre-`%b` OSC 8 link text, or just `text` when links are off.
fn osc8_link(env: &Env, url: &str, text: &str) -> String {
    if url.is_empty() || !supports_osc8(env) {
        return text.to_string();
    }
    format!("\\033]8;;{url}\\033\\\\{text}\\033]8;;\\033\\\\")
}

/// True when `path` sits under one of `STATUSLINE_CI_ROOTS`, or none is set.
fn is_workspace_project(env: &Env, path: &str) -> bool {
    let Some(roots) = env.get("STATUSLINE_CI_ROOTS") else {
        return true;
    };
    roots
        .split(':')
        .filter(|r| !r.is_empty())
        .any(|r| path.starts_with(&format!("{}/", r.strip_suffix('/').unwrap_or(r))))
}

/// Upper bound for terminal and bar widths, so a hostile value cannot balloon the output.
const MAX_WIDTH: i64 = 1000;

type Ci = (String, usize, usize, usize);

/// Repo facts line 1 needs, resolved once.
struct Repo {
    branch: String,
    in_repo: bool,
    host: String,
    owner: String,
    name: String,
    has_gh_path: bool,
}

impl Repo {
    fn branch_is_real(&self) -> bool {
        !self.branch.is_empty() && self.branch != "detached"
    }
}

fn read_cache(path: &Path, now: i64, ttl: i64, refresh: impl FnOnce()) -> String {
    let age = gh::age_of(path, now, 999_999);
    let body = std::fs::read(path)
        .map(|b| {
            String::from_utf8_lossy(&b)
                .trim_end_matches('\n')
                .to_string()
        })
        .unwrap_or_default();
    if age >= ttl {
        refresh();
    }
    body
}

/// The PR half of line 1, plus the CI rollup it already holds.
fn pr_right(
    env: &Env,
    repo: &Repo,
    cwd: &str,
    cache_dir: &Path,
    gh_bin: Option<&Path>,
    now: i64,
) -> (String, Option<Ci>) {
    let none = (String::new(), None);
    let Some(gh_bin) = gh_bin else { return none };
    if !(env.flag("STATUSLINE_SHOW_PR")
        && repo.in_repo
        && repo.branch_is_real()
        && repo.has_gh_path)
    {
        return none;
    }
    let cache = cache_dir.join(format!(
        "pr-{}.json",
        slugify(&format!("{cwd}::{}", repo.branch))
    ));
    let ttl = env.int("STATUSLINE_PR_CACHE_TTL", 60);
    let body = read_cache(&cache, now, ttl, || gh::refresh_pr(gh_bin, &cache, now));
    if body.is_empty() {
        return none;
    }
    let ci = (env.flag("STATUSLINE_SHOW_CI") && is_workspace_project(env, cwd))
        .then(|| pr_json::ci_rollup(&body));

    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let number = pr_json::pr_number_value(&v);
    if number.is_empty() {
        return (String::new(), ci);
    }
    let author = pr_json::str_field(&v, &["author", "login"]);
    let review = pr_json::str_field(&v, &["reviewDecision"]);
    let state = pr_json::str_field(&v, &["state"]);
    let merged_at = pr_json::str_field(&v, &["mergedAt"]);
    let closed_at = pr_json::str_field(&v, &["closedAt"]);
    let pending = pr_json::pending_logins(&v);
    let completed = pr_json::completed_reviews(&v, &pending);

    let jira = crate::json::statusline::jira_key(&repo.branch)
        .map(str::to_string)
        .unwrap_or_else(|| pr_json::jira_from_body(&pr_json::str_field(&v, &["body"])));

    let label = format!("{ORANGE}PR #{number}{RESET}");
    let url = format!(
        "https://{}/{}/{}/pull/{number}",
        repo.host, repo.owner, repo.name
    );
    let mut right = if repo.owner.is_empty() || repo.name.is_empty() {
        label
    } else {
        osc8_link(env, &url, &label)
    };

    if let Some((st, failed, running, total)) = &ci {
        match st.as_str() {
            "fail" => right.push_str(&format!(" {RED}CI ✗ {failed}/{total}{RESET}")),
            "running" => right.push_str(&format!(" {YELLOW}CI ● {running}/{total}{RESET}")),
            _ => {}
        }
    }
    if !jira.is_empty() {
        let mut jira_label = format!("{TEAL}{jira}{RESET}");
        if let Some(base) = env.get("STATUSLINE_JIRA_BASE_URL") {
            let base = base.strip_suffix('/').unwrap_or(base);
            jira_label = osc8_link(env, &format!("{base}/browse/{jira}"), &jira_label);
        }
        right.push(' ');
        right.push_str(&jira_label);
    }
    if !author.is_empty() {
        right.push_str(&format!(" {WHITE}@{author}{RESET}"));
    }

    let mut show_reviewers = true;
    let ago = |ts: &str| gh::iso_to_epoch(ts).map(|e| fmt_ago(now - e));
    match state.as_str() {
        "MERGED" | "CLOSED" => {
            let (word, color, ts) = if state == "MERGED" {
                ("MERGED", MAUVE, &merged_at)
            } else {
                ("CLOSED", RED, &closed_at)
            };
            right.push_str(&format!(" {color}{word}{RESET}"));
            if let Some(a) = (!ts.is_empty()).then(|| ago(ts)).flatten() {
                right.push_str(&format!(" {DIM}({a} ago){RESET}"));
            }
            show_reviewers = false;
        }
        _ => match review.as_str() {
            "APPROVED" => right.push_str(&format!(" {GREEN}APPROVED{RESET}")),
            "CHANGES_REQUESTED" => right.push_str(&format!(" {RED}CHANGES REQUESTED{RESET}")),
            "REVIEW_REQUIRED" => right.push_str(&format!(" {YELLOW}REVIEW REQUIRED{RESET}")),
            _ => {
                right.push_str(&format!(" {DIM}NO REVIEW{RESET}"));
                show_reviewers = false;
            }
        },
    }

    if !completed.is_empty() && show_reviewers {
        right.push_str(&format!(" {DIM}by{RESET}"));
        for entry in completed.iter().filter(|e| !e.is_empty()) {
            let (color, rest) = entry.split_once(':').unwrap_or((entry, ""));
            let (name, ts) = rest.split_once(':').unwrap_or((rest, ""));
            let paint = match color {
                "g" => GREEN,
                "r" => RED,
                _ => DIM,
            };
            right.push_str(&format!(" {paint}{name}{RESET}"));
            if let Some(a) = (!ts.is_empty()).then(|| ago(ts)).flatten() {
                right.push_str(&format!(" {DIM}({a} ago){RESET}"));
            }
        }
    }
    let waiting: Vec<&str> = pending
        .iter()
        .map(String::as_str)
        .filter(|n| !n.is_empty())
        .collect();
    if !waiting.is_empty() {
        right.push_str(&format!(
            " {DIM}waiting on{RESET} {YELLOW}{}{RESET}",
            waiting.join(", ")
        ));
    }
    (right, ci)
}

/// CI badge for a branch with no PR, from a cached GraphQL rollup.
fn standalone_ci(
    env: &Env,
    repo: &Repo,
    cwd: &str,
    cache_dir: &Path,
    gh_bin: &Path,
    now: i64,
) -> Option<Ci> {
    let cache = cache_dir.join(format!(
        "ci-{}.json",
        slugify(&format!("{cwd}::{}", repo.branch))
    ));
    let ttl = env.int("STATUSLINE_CI_CACHE_TTL", 60);
    let body = read_cache(&cache, now, ttl, || {
        gh::refresh_ci(gh_bin, &cache, now, &repo.owner, &repo.name, &repo.branch)
    });
    (!body.is_empty()).then(|| pr_json::ci_rollup(&pr_json::graphql_ci_checks(&body)))
}

fn ci_badge(ci: &Ci) -> Option<String> {
    let (state, failed, running, total) = ci;
    match state.as_str() {
        "fail" => Some(format!("{RED}CI ✗ {failed}/{total}{RESET}")),
        "running" => Some(format!("{YELLOW}CI ● {running}/{total}{RESET}")),
        _ => None,
    }
}

fn ensure_cache_dir(dir: &Path) {
    if !dir.is_dir() && std::fs::create_dir_all(dir).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
    }
}

/// `$PWD` as bash sees it: the inherited value only when it names the real
/// working directory, otherwise the working directory itself.
fn logical_pwd(env: &Env) -> String {
    let real = std::env::current_dir().ok();
    let inherited = env.get("PWD").map(PathBuf::from);
    match (inherited, real) {
        (Some(p), Some(r)) if p.canonicalize().ok() == r.canonicalize().ok() => {
            p.to_string_lossy().into_owned()
        }
        (Some(p), None) => p.to_string_lossy().into_owned(),
        (_, Some(r)) => r.to_string_lossy().into_owned(),
        (None, None) => String::new(),
    }
}

/// Renders the whole status line for `input`, as the bytes to print.
pub fn render(input: &str, env: &Env, now: i64) -> Vec<u8> {
    let home = env
        .get("HOME")
        .map(str::to_string)
        .unwrap_or_else(|| crate::common::home_dir().to_string_lossy().into_owned());
    let cache_dir: PathBuf = match (env.get("STATUSLINE_CACHE_DIR"), env.get("XDG_CACHE_HOME")) {
        (Some(d), _) => PathBuf::from(d),
        (None, Some(x)) => PathBuf::from(format!("{x}/statusline")),
        (None, None) => PathBuf::from(format!("{home}/.cache/statusline")),
    };
    ensure_cache_dir(&cache_dir);

    let pwd = logical_pwd(env);
    let session = if input.is_empty() {
        Session::default()
    } else {
        Session::parse(input, &pwd)
    };
    let cwd = if session.cwd.is_empty() {
        pwd
    } else {
        session.cwd.clone()
    };

    if !session.session_id.is_empty() {
        telemetry::record(
            &home,
            &session.session_id,
            &session.cost_usd,
            &session.used,
            env.int("CC_CAPTURE_AT", 70),
            now,
        );
    }

    let display = if cwd.starts_with(&home) {
        format!("~{}", &cwd[home.len()..])
    } else {
        cwd.clone()
    };
    let mut left = format!("{GREEN}{display}{RESET}");

    let mut repo = Repo {
        branch: String::new(),
        in_repo: false,
        host: String::new(),
        owner: String::new(),
        name: String::new(),
        has_gh_path: false,
    };
    if env.flag("STATUSLINE_SHOW_GIT") {
        if let Some(info) = git::inspect(&cwd) {
            repo.in_repo = true;
            repo.branch = info.branch;
            let mut link = String::new();
            if let Some((host, path)) = gh::remote_parse(&info.remote_url) {
                let (owner, name) = path.split_once('/').unwrap_or((&path, ""));
                repo.owner = owner.to_string();
                repo.name = name.to_string();
                repo.has_gh_path = true;
                if repo.branch_is_real() {
                    link = format!("https://{host}/{path}/tree/{}", repo.branch);
                }
                repo.host = host;
            }
            let shown = if link.is_empty() {
                repo.branch.clone()
            } else {
                format!("\\033]8;;{link}\\033\\\\{}\\033]8;;\\033\\\\", repo.branch)
            };
            left.push_str(&format!(" {RED}ϓ {shown}{RESET}"));
            let mark = if info.dirty { "[+]" } else { "[$]" };
            left.push_str(&format!(" {YELLOW}{mark}{RESET}"));
        }
    }

    let gh_bin = env.get("PATH").and_then(|p| gh::find_in_path("gh", p));
    let (mut right, mut ci) = pr_right(env, &repo, &cwd, &cache_dir, gh_bin.as_deref(), now);

    let standalone = env.flag("STATUSLINE_SHOW_CI")
        && is_workspace_project(env, &cwd)
        && repo.in_repo
        && repo.branch_is_real()
        && !repo.owner.is_empty()
        && !repo.name.is_empty()
        && ci.is_none();
    if let (true, Some(gh_bin)) = (standalone, gh_bin.as_deref()) {
        ci = standalone_ci(env, &repo, &cwd, &cache_dir, gh_bin, now);
        if let Some(badge) = ci.as_ref().and_then(ci_badge) {
            if !right.is_empty() {
                right.push(' ');
            }
            right.push_str(&badge);
        }
    }

    let width = env.get("COLUMNS").map_or(120, int_part).clamp(0, MAX_WIDTH);
    let pad = (width - visible_len(&left) as i64 - visible_len(&right) as i64).max(1);
    let (mut out, stopped) = percent_b(&left);
    if !stopped {
        if right.is_empty() {
            out.push(b'\n');
        } else {
            out.extend(std::iter::repeat_n(b' ', pad as usize));
            let (r, stopped) = percent_b(&right);
            out.extend(r);
            if !stopped {
                out.push(b'\n');
            }
        }
    }

    let claude_json = env
        .get("CLAUDE_CONFIG_DIR")
        .map_or_else(|| PathBuf::from(&home), PathBuf::from)
        .join(".claude.json");
    let budget = budget::build(input, now, &claude_json, Path::new(&home));
    let opts = Opts {
        show_model: env.flag("STATUSLINE_SHOW_MODEL"),
        show_context: env.flag("STATUSLINE_SHOW_CONTEXT"),
        show_session_age: env.flag("STATUSLINE_SHOW_SESSION_AGE"),
        show_cache_ratio: env.flag("STATUSLINE_SHOW_CACHE_RATIO"),
        show_rate_limits: env.flag("STATUSLINE_SHOW_RATE_LIMITS"),
        bar_width: env.int("STATUSLINE_CTX_BAR_WIDTH", 10).clamp(0, MAX_WIDTH),
        compact_trigger: env
            .get("CLAUDE_CODE_AUTO_COMPACT_WINDOW")
            .and_then(|w| fmt::compact_trigger_pct(w, &session.ctx_window_size))
            .unwrap_or_else(|| env.int("CLAUDE_AUTOCOMPACT_PCT_OVERRIDE", 90)),
        home,
        now,
    };
    for line in [
        lines::line_2(&session, &opts),
        lines::line_3(&session, &opts),
        lines::line_4(&budget, width as usize),
    ]
    .into_iter()
    .flatten()
    {
        let (bytes, stopped) = percent_b(&line);
        out.extend(bytes);
        if !stopped {
            out.push(b'\n');
        }
    }
    out
}
