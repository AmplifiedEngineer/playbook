// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook pr stack`: find the pull requests stacked with a PR. It reads the
//! GitHub stack API first and falls back to following the branch chain (each
//! PR targets the head branch of the PR below it), which covers Graphite,
//! ghstack, git-town and hand-made stacks. See ADR-0019.

use super::shared::gh;
use serde_json::{json, Value};

/// Default ceilings above which the review command asks again.
const DEFAULT_MAX_PRS: u64 = 6;
const DEFAULT_MAX_LINES: u64 = 3000;

/// Longest branch chain followed in either direction; guards against loops.
const MAX_CHAIN: usize = 25;

const PR_FIELDS: &str =
    "number,title,state,isDraft,baseRefName,headRefName,headRefOid,additions,deletions,changedFiles,author,mergedAt,url";

const STACK_QUERY: &str = r#"
  query($owner: String!, $name: String!, $pr: Int!) {
    repository(owner: $owner, name: $name) {
      pullRequest(number: $pr) {
        stack {
          entries(first: 50) {
            nodes {
              position
              pullRequest {
                number title state isDraft baseRefName headRefName headRefOid
                additions deletions changedFiles author { login } mergedAt url
              }
            }
          }
        }
      }
    }
  }"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Open,
    Merged,
    Closed,
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            State::Open => "open",
            State::Merged => "merged",
            State::Closed => "closed",
        }
    }

    /// What the review does with a PR in this state.
    fn role(self) -> &'static str {
        match self {
            State::Open => "review",
            State::Merged => "context",
            State::Closed => "skip",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackPr {
    pub number: u64,
    pub title: String,
    pub state: State,
    pub draft: bool,
    pub base: String,
    pub head: String,
    pub head_sha: String,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub author: String,
    pub url: String,
}

impl StackPr {
    pub fn lines(&self) -> u64 {
        self.additions + self.deletions
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Api,
    BranchChain,
    None,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Source::Api => "api",
            Source::BranchChain => "branch-chain",
            Source::None => "none",
        }
    }
}

/// A detected stack, bottom (closest to the trunk) first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stack {
    pub source: Source,
    pub repo: String,
    pub current: u64,
    pub prs: Vec<StackPr>,
    pub warnings: Vec<String>,
}

/// The ceilings `to_json` checks the stack against.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_prs: u64,
    pub max_lines: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_prs: DEFAULT_MAX_PRS,
            max_lines: DEFAULT_MAX_LINES,
        }
    }
}

impl Stack {
    fn open(&self) -> impl Iterator<Item = &StackPr> {
        self.prs.iter().filter(|p| p.state == State::Open)
    }

    /// True whenever the PR belongs to a stack, even one with a single open
    /// PR: the user always chooses the scope and the review depth.
    pub fn ask(&self) -> bool {
        self.source != Source::None
    }

    pub fn to_json(&self, limits: Limits) -> Value {
        let open_count = self.open().count() as u64;
        let open_lines: u64 = self.open().map(StackPr::lines).sum();
        let merged_count = self.prs.iter().filter(|p| p.state == State::Merged).count();
        let mut over = Vec::new();
        if open_count > limits.max_prs {
            over.push(format!(
                "{open_count} open PRs, limit {} (review.stackMaxPrs)",
                limits.max_prs
            ));
        }
        if open_lines > limits.max_lines {
            over.push(format!(
                "{open_lines} changed lines, limit {} (review.stackMaxLines)",
                limits.max_lines
            ));
        }
        let prs: Vec<Value> = self
            .prs
            .iter()
            .enumerate()
            .map(|(i, p)| {
                json!({
                    "position": i + 1,
                    "number": p.number,
                    "title": p.title,
                    "state": p.state.as_str(),
                    "role": p.state.role(),
                    "draft": p.draft,
                    "base": p.base,
                    "head": p.head,
                    "head_sha": p.head_sha,
                    "additions": p.additions,
                    "deletions": p.deletions,
                    "changed_files": p.changed_files,
                    "author": p.author,
                    "url": p.url,
                })
            })
            .collect();
        json!({
            "in_stack": self.source != Source::None,
            "source": self.source.as_str(),
            "repo": self.repo,
            "current": self.current,
            "ask": self.ask(),
            "open_count": open_count,
            "merged_count": merged_count,
            "open_lines": open_lines,
            "over_budget": !over.is_empty(),
            "over_budget_reasons": over,
            "prs": prs,
            "warnings": self.warnings,
        })
    }

    pub fn to_text(&self, limits: Limits) -> String {
        if self.source == Source::None {
            return format!("PR #{} is not part of a stack.", self.current);
        }
        let mut out = format!(
            "Stack of {} PRs in {} (found by {}):\n",
            self.prs.len(),
            self.repo,
            self.source.as_str()
        );
        for (i, p) in self.prs.iter().enumerate() {
            let mark = if p.number == self.current {
                " <- this PR"
            } else {
                ""
            };
            out.push_str(&format!(
                "  {}. #{} [{}] {} (+{} -{}){}\n",
                i + 1,
                p.number,
                p.state.as_str(),
                p.title,
                p.additions,
                p.deletions,
                mark
            ));
        }
        let j = self.to_json(limits);
        out.push_str(&format!(
            "Open: {}, merged (context only): {}, open changed lines: {}\n",
            j["open_count"], j["merged_count"], j["open_lines"]
        ));
        if let Some(reasons) = j["over_budget_reasons"].as_array() {
            for r in reasons {
                out.push_str(&format!("Over budget: {}\n", r.as_str().unwrap_or("")));
            }
        }
        for w in &self.warnings {
            out.push_str(&format!("warning: {w}\n"));
        }
        out
    }
}

fn parse_state(v: &Value) -> State {
    match v.as_str().unwrap_or("").to_ascii_uppercase().as_str() {
        "OPEN" => State::Open,
        "MERGED" => State::Merged,
        _ => State::Closed,
    }
}

/// Reads one PR object. The GraphQL node and `gh pr view/list --json` share
/// these field names.
fn parse_pr(v: &Value) -> Option<StackPr> {
    let text = |k: &str| v[k].as_str().unwrap_or("").to_string();
    let num = |k: &str| v[k].as_u64().unwrap_or(0);
    let mut state = parse_state(&v["state"]);
    if state == State::Closed && v["mergedAt"].as_str().is_some_and(|s| !s.is_empty()) {
        state = State::Merged;
    }
    Some(StackPr {
        number: v["number"].as_u64()?,
        title: text("title"),
        state,
        draft: v["isDraft"].as_bool().unwrap_or(false),
        base: text("baseRefName"),
        head: text("headRefName"),
        head_sha: text("headRefOid"),
        additions: num("additions"),
        deletions: num("deletions"),
        changed_files: num("changedFiles"),
        author: v["author"]["login"].as_str().unwrap_or("").to_string(),
        url: text("url"),
    })
}

pub(crate) type Gh<'a> = &'a (dyn Fn(&[&str]) -> Result<String, String> + Sync);

fn json_of(raw: &str, what: &str) -> Result<Value, String> {
    serde_json::from_str(raw).map_err(|e| format!("could not read {what} from gh: {e}"))
}

/// Entries from the stack API, bottom first. `Ok(None)` when the PR has no
/// stack; `Err` when the call or the field is unavailable (older GHES).
fn api_stack(run: Gh, owner: &str, name: &str, pr: u64) -> Result<Option<Vec<StackPr>>, String> {
    let raw = run(&[
        "api",
        "graphql",
        "-f",
        &format!("query={STACK_QUERY}"),
        "-F",
        &format!("owner={owner}"),
        "-F",
        &format!("name={name}"),
        "-F",
        &format!("pr={pr}"),
    ])?;
    let v = json_of(&raw, "the stack")?;
    let nodes = &v["data"]["repository"]["pullRequest"]["stack"]["entries"]["nodes"];
    let Some(nodes) = nodes.as_array() else {
        return Ok(None);
    };
    let mut entries: Vec<(u64, StackPr)> = nodes
        .iter()
        .filter_map(|n| Some((n["position"].as_u64()?, parse_pr(&n["pullRequest"])?)))
        .collect();
    entries.sort_by_key(|(pos, _)| *pos);
    Ok(Some(entries.into_iter().map(|(_, p)| p).collect()))
}

fn list_prs(run: Gh, flag: &str, branch: &str) -> Vec<StackPr> {
    let Ok(raw) = run(&[
        "pr", "list", "--state", "all", flag, branch, "--json", PR_FIELDS, "--limit", "20",
    ]) else {
        return Vec::new();
    };
    let mut prs: Vec<StackPr> = json_of(&raw, "the PR list")
        .ok()
        .and_then(|v| {
            v.as_array()
                .map(|a| a.iter().filter_map(parse_pr).collect())
        })
        .unwrap_or_default();
    prs.sort_by_key(|p| p.number);
    prs
}

/// Prefer an open PR, then a merged one, then the lowest number.
fn pick(mut found: Vec<StackPr>, what: &str, warnings: &mut Vec<String>) -> Option<StackPr> {
    if found.len() > 1 {
        let rank = |p: &StackPr| match p.state {
            State::Open => 0,
            State::Merged => 1,
            State::Closed => 2,
        };
        found.sort_by_key(|p| (rank(p), p.number));
        warnings.push(format!(
            "{what} matches {} PRs; following #{}",
            found.len(),
            found[0].number
        ));
    }
    found.into_iter().next()
}

/// Follow the chain of PRs below and above `start` by branch name.
fn branch_chain(
    run: Gh,
    start: StackPr,
    default_branch: &str,
    warnings: &mut Vec<String>,
) -> Vec<StackPr> {
    let mut chain = vec![start.clone()];
    let mut seen = vec![start.number];
    let mut base = start.base.clone();
    while chain.len() < MAX_CHAIN && base != default_branch && !base.is_empty() {
        let below = pick(
            list_prs(run, "--head", &base),
            &format!("branch '{base}'"),
            warnings,
        );
        match below {
            Some(p) if !seen.contains(&p.number) => {
                seen.push(p.number);
                base = p.base.clone();
                chain.insert(0, p);
            }
            _ => break,
        }
    }
    let mut head = start.head;
    while chain.len() < MAX_CHAIN {
        let above = pick(
            list_prs(run, "--base", &head),
            &format!("base '{head}'"),
            warnings,
        );
        match above {
            Some(p) if !seen.contains(&p.number) => {
                seen.push(p.number);
                head = p.head.clone();
                chain.push(p);
            }
            _ => break,
        }
    }
    chain
}

/// Detect the stack around `pr` (or the PR of the current branch).
pub fn detect(run: Gh, pr: Option<u64>) -> Result<Stack, String> {
    let repo_json = run(&["repo", "view", "--json", "nameWithOwner,defaultBranchRef"])?;
    let repo_v = json_of(&repo_json, "the repository")?;
    let repo = repo_v["nameWithOwner"].as_str().unwrap_or("").to_string();
    let default_branch = repo_v["defaultBranchRef"]["name"]
        .as_str()
        .unwrap_or("main")
        .to_string();
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| format!("unexpected repo name: {repo}"))?;
    let number = match pr {
        Some(n) => n,
        None => run(&["pr", "view", "--json", "number", "-q", ".number"])
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .ok_or("error: no PR for current branch; pass a PR number")?,
    };

    let mut warnings = Vec::new();
    let mut source = Source::Api;
    let mut prs = match api_stack(run, owner, name, number) {
        Ok(Some(prs)) if prs.iter().any(|p| p.number == number) => prs,
        Ok(_) => Vec::new(),
        Err(e) => {
            warnings.push(format!("stack API unavailable, using branch names: {e}"));
            Vec::new()
        }
    };
    if prs.len() < 2 {
        let raw = run(&["pr", "view", &number.to_string(), "--json", PR_FIELDS])?;
        let start =
            parse_pr(&json_of(&raw, "the PR")?).ok_or("gh returned a PR without a number")?;
        prs = branch_chain(run, start, &default_branch, &mut warnings);
        source = Source::BranchChain;
    }
    if prs.len() < 2 {
        source = Source::None;
    }
    if let Some(p) = prs.iter().find(|p| p.number == number) {
        match p.state {
            State::Merged => warnings.push(format!("#{number} is merged and will not be reviewed")),
            State::Closed => warnings.push(format!("#{number} is closed and will not be reviewed")),
            State::Open => {}
        }
    }
    for p in prs.iter().filter(|p| p.state == State::Closed) {
        warnings.push(format!(
            "#{} is closed without merging; it is skipped",
            p.number
        ));
    }
    Ok(Stack {
        source,
        repo,
        current: number,
        prs,
        warnings,
    })
}

/// Parsed arguments of `playbook pr stack`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Parsed {
    pub pr: Option<u64>,
    pub json: bool,
    pub max_prs: Option<u64>,
    pub max_lines: Option<u64>,
    pub context: bool,
    pub warnings: Vec<String>,
}

pub fn parse(args: &str) -> Parsed {
    let mut p = Parsed::default();
    let mut toks = args.split_whitespace();
    while let Some(tok) = toks.next() {
        match tok {
            "--json" => p.json = true,
            "--context" => p.context = true,
            "--max-prs" => p.max_prs = toks.next().and_then(|v| v.parse().ok()),
            "--max-lines" => p.max_lines = toks.next().and_then(|v| v.parse().ok()),
            "--auto" | "--ask" => {}
            _ => match tok.strip_prefix('#').unwrap_or(tok).parse::<u64>() {
                Ok(n) => p.pr = Some(n),
                Err(_) => p
                    .warnings
                    .push(format!("warning: ignoring unknown arg '{tok}'")),
            },
        }
    }
    p
}

/// Run the command; `Err` is the text for stderr (exit 1).
pub fn run(args: &str) -> Result<String, String> {
    if let Some(rest) = args.trim_start().strip_prefix("map") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return super::stack_map::run(rest);
        }
    }
    let parsed = parse(args);
    for w in &parsed.warnings {
        eprintln!("{w}");
    }
    let limits = Limits {
        max_prs: parsed.max_prs.unwrap_or(DEFAULT_MAX_PRS),
        max_lines: parsed.max_lines.unwrap_or(DEFAULT_MAX_LINES),
    };
    let stack = detect(&|a| gh(a), parsed.pr)?;
    if parsed.context {
        return super::stack_context::write(&|a| gh(a), &stack, limits);
    }
    if parsed.json {
        serde_json::to_string_pretty(&stack.to_json(limits)).map_err(|e| e.to_string())
    } else {
        Ok(stack.to_text(limits))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fx {
        ($n:literal) => {
            include_str!(concat!("../../tests/fixtures/pr_stack/", $n))
        };
    }

    fn fixture(name: &str) -> &'static str {
        match name {
            "repo" => fx!("repo.json"),
            "api-none" => fx!("api-none.json"),
            "api-2" => fx!("api-2.json"),
            "api-5" => fx!("api-5.json"),
            "api-middle-merged" => fx!("api-middle-merged.json"),
            "api-top-closed" => fx!("api-top-closed.json"),
            "pr-1" => fx!("pr-1.json"),
            "pr-2" => fx!("pr-2.json"),
            "pr-3" => fx!("pr-3.json"),
            "head-b1" => fx!("head-b1.json"),
            "head-b2" => fx!("head-b2.json"),
            "base-b1" => fx!("base-b1.json"),
            "base-b2" => fx!("base-b2.json"),
            "base-b3" => fx!("base-b3.json"),
            _ => "[]",
        }
    }

    /// A fake `gh`: `api` is the GraphQL fixture name, or `None` for a failing
    /// call (the field is unknown).
    fn fake(api: Option<&'static str>) -> impl Fn(&[&str]) -> Result<String, String> {
        move |args: &[&str]| {
            let last = |i: usize| args.get(i).copied().unwrap_or("");
            match (last(0), last(1)) {
                ("repo", "view") => Ok(fixture("repo").to_string()),
                ("api", "graphql") => api
                    .map(|n| fixture(n).to_string())
                    .ok_or_else(|| "Field 'stack' doesn't exist on type 'PullRequest'".to_string()),
                ("pr", "view") => Ok(fixture(&format!("pr-{}", last(2))).to_string()),
                ("pr", "list") => {
                    let branch = last(5);
                    let kind = if last(4) == "--head" { "head" } else { "base" };
                    Ok(fixture(&format!("{kind}-{branch}")).to_string())
                }
                _ => Err("unexpected gh call".to_string()),
            }
        }
    }

    fn numbers(s: &Stack) -> Vec<u64> {
        s.prs.iter().map(|p| p.number).collect()
    }

    #[test]
    fn no_stack_reports_none_and_does_not_ask() {
        let run = fake(Some("api-none"));
        // pr-1 has base main and nothing targets b1 in this fixture set
        let s = detect(
            &|a| match (a.first(), a.get(4)) {
                (Some(&"pr"), Some(&"--base")) => Ok("[]".into()),
                _ => run(a),
            },
            Some(1),
        )
        .unwrap();
        assert_eq!(s.source, Source::None);
        assert!(!s.ask());
        assert_eq!(
            s.to_text(Limits::default()),
            "PR #1 is not part of a stack."
        );
    }

    #[test]
    fn two_stack_from_the_api() {
        let s = detect(&fake(Some("api-2")), Some(2)).unwrap();
        assert_eq!(s.source, Source::Api);
        assert_eq!(numbers(&s), [1, 2]);
        assert!(s.ask());
        assert_eq!(s.prs[1].base, "b1");
        assert_eq!(s.prs[0].head_sha, "sha1");
    }

    #[test]
    fn five_stack_keeps_order_and_counts_lines() {
        let s = detect(&fake(Some("api-5")), Some(3)).unwrap();
        assert_eq!(numbers(&s), [1, 2, 3, 4, 5]);
        let j = s.to_json(Limits::default());
        assert_eq!(j["open_count"], 5);
        assert_eq!(j["open_lines"], 60);
        assert_eq!(j["over_budget"], false);
    }

    #[test]
    fn over_budget_names_the_key_to_change() {
        let s = detect(&fake(Some("api-5")), Some(3)).unwrap();
        let j = s.to_json(Limits {
            max_prs: 4,
            max_lines: 50,
        });
        assert_eq!(j["over_budget"], true);
        let reasons = j["over_budget_reasons"].to_string();
        assert!(reasons.contains("review.stackMaxPrs") && reasons.contains("review.stackMaxLines"));
    }

    #[test]
    fn merged_prs_are_context_and_never_reviewed() {
        let s = detect(&fake(Some("api-middle-merged")), Some(3)).unwrap();
        assert_eq!(numbers(&s), [1, 2, 3, 4]);
        let j = s.to_json(Limits::default());
        let roles: Vec<&str> = j["prs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["context", "context", "review", "review"]);
        assert_eq!(j["merged_count"], 2);
        assert_eq!(j["open_count"], 2);
    }

    #[test]
    fn a_stack_with_one_open_pr_still_asks() {
        let mut s = detect(&fake(Some("api-middle-merged")), Some(3)).unwrap();
        s.prs.retain(|p| p.number != 4);
        assert_eq!(s.open().count(), 1);
        assert!(s.ask());
    }

    #[test]
    fn closed_top_pr_is_skipped_with_a_warning() {
        let s = detect(&fake(Some("api-top-closed")), Some(1)).unwrap();
        let j = s.to_json(Limits::default());
        assert_eq!(j["prs"][2]["role"], "skip");
        assert!(s
            .warnings
            .iter()
            .any(|w| w.contains("#3 is closed without merging")));
        assert_eq!(j["open_count"], 2);
    }

    #[test]
    fn naming_a_merged_pr_warns_that_it_is_not_reviewed() {
        let s = detect(&fake(Some("api-middle-merged")), Some(1)).unwrap();
        assert!(s.warnings.iter().any(|w| w.contains("#1 is merged")));
    }

    #[test]
    fn branch_chain_is_found_from_any_pr_when_the_api_has_no_stack() {
        for start in [1, 2, 3] {
            let s = detect(&fake(Some("api-none")), Some(start)).unwrap();
            assert_eq!(s.source, Source::BranchChain, "from #{start}");
            assert_eq!(numbers(&s), [1, 2, 3], "from #{start}");
        }
    }

    #[test]
    fn an_unknown_api_field_falls_back_to_the_branch_chain_with_a_warning() {
        let s = detect(&fake(None), Some(2)).unwrap();
        assert_eq!(s.source, Source::BranchChain);
        assert_eq!(numbers(&s), [1, 2, 3]);
        assert!(s
            .warnings
            .iter()
            .any(|w| w.contains("stack API unavailable")));
    }

    #[test]
    fn a_branch_with_two_child_prs_follows_the_open_one_and_warns() {
        let run = fake(Some("api-none"));
        let s = detect(
            &|a| {
                if a.first() == Some(&"pr") && a.get(1) == Some(&"list") && a.get(5) == Some(&"b1")
                {
                    return Ok(format!("[{},{}]", fx!("pr-3.json"), fx!("pr-2.json")));
                }
                run(a)
            },
            Some(1),
        )
        .unwrap();
        assert!(s.warnings.iter().any(|w| w.contains("matches 2 PRs")));
    }

    #[test]
    fn parse_takes_a_number_flags_and_limits() {
        let p = parse("#12 --json --max-prs 3 --max-lines 99 --bogus");
        assert_eq!(p.pr, Some(12));
        assert!(p.json);
        assert_eq!((p.max_prs, p.max_lines), (Some(3), Some(99)));
        assert_eq!(p.warnings.len(), 1);
    }
}
