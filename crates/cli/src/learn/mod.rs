// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook learn preflight`: Phase 0 of `/playbook:learn`. Names the
//! repo, the memory store and the commit count, probes which tools are
//! reachable, and tallies the JIRA keys in recent history.

pub mod collect;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

/// Longest a preflight probe (`git`, `gh`, `acli`) may run before it counts
/// as unavailable, so a hung network never hangs the command.
const PROBE_TIMEOUT: Duration = crate::common::git::SLOW_TIMEOUT;

fn run(program: &str, args: &[&str], dir: &Path) -> Option<String> {
    crate::common::proc::text_in(program, args, dir, PROBE_TIMEOUT)
}

fn ok(program: &str, args: &[&str], dir: &Path) -> bool {
    crate::common::proc::succeeds_in(program, args, dir, PROBE_TIMEOUT)
}

/// `owner/name` from an origin URL such as `git@host:owner/name.git`.
fn repo_from_url(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/').trim_end_matches(".git");
    let mut parts = url.rsplit(['/', ':']);
    let name = parts.next().filter(|n| !n.is_empty())?;
    let owner = parts.next().filter(|o| !o.is_empty())?;
    Some(format!("{owner}/{name}"))
}

/// `uniq -c | sort -rn | head` over the project keys in `log`: the part of
/// each `ABC-123` before the dash, with its count, highest first.
fn jira_histogram(log: &str, limit: usize) -> Vec<(usize, String)> {
    let re = crate::common::re::static_regex(r"[A-Z][A-Z0-9]+-[0-9]+");
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for m in re.find_iter(log) {
        let key = m.as_str().rsplit_once('-').map_or(m.as_str(), |(k, _)| k);
        *counts.entry(key.to_string()).or_default() += 1;
    }
    let mut rows: Vec<(usize, String)> = counts.into_iter().map(|(k, n)| (n, k)).collect();
    // `sort -rn` on `count key` lines: count first, then the line text, both reversed.
    rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    rows.truncate(limit);
    rows
}

/// The preflight report for the repo containing `dir`.
pub fn preflight(dir: &Path, home: &Path) -> Result<String, String> {
    let root = run("git", &["rev-parse", "--show-toplevel"], dir).ok_or("not in a git repo")?;
    let root_path = Path::new(&root);
    let repo = run(
        "gh",
        &[
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "-q",
            ".nameWithOwner",
        ],
        root_path,
    )
    .filter(|r| !r.is_empty())
    .or_else(|| {
        repo_from_url(&run("git", &["remote", "get-url", "origin"], root_path).unwrap_or_default())
    })
    .ok_or(
        "no origin remote and gh cannot name the repo; add one with `git remote add origin <url>`",
    )?;
    let store = crate::common::paths::playbook_root_from(home)
        .join("memory")
        .join(&repo);
    let commits =
        run("git", &["rev-list", "--count", "HEAD"], root_path).unwrap_or_else(|| "0".to_string());
    let mut out = vec![
        format!("Repo:    {repo}"),
        format!("Root:    {root}"),
        format!("Store:   {}", store.display()),
        format!("Commits: {commits}"),
    ];
    out.push(
        if ok("gh", &["auth", "status"], root_path) {
            "gh:   ok"
        } else {
            "gh:   UNAVAILABLE (PRs skipped)"
        }
        .to_string(),
    );
    match run("acli", &["--version"], root_path) {
        Some(v) => {
            out.push(format!(
                "acli: present ({})",
                v.lines().next().unwrap_or("")
            ));
            for (label, service) in [("jira:       ", "jira"), ("confluence:", "confluence")] {
                let authed = ok("acli", &[service, "auth", "status"], root_path);
                out.push(format!(
                    "acli {label} {}",
                    if authed { "authed" } else { "NOT authed" }
                ));
            }
        }
        None if ok("acli", &["--version"], root_path) => out.push("acli: present ()".into()),
        None => out.push("acli: absent".into()),
    }
    out.push("JIRA keys in history:".to_string());
    let log = run("git", &["log", "--oneline", "-500"], root_path).unwrap_or_default();
    for (n, key) in jira_histogram(&log, 10) {
        out.push(format!("{n:>7} {key}"));
    }
    Ok(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repo_name_comes_from_https_and_ssh_urls() {
        assert_eq!(
            repo_from_url("https://github.com/o/r.git\n").as_deref(),
            Some("o/r")
        );
        assert_eq!(
            repo_from_url("git@github.com:o/r.git").as_deref(),
            Some("o/r")
        );
        assert_eq!(
            repo_from_url("https://github.com/o/r").as_deref(),
            Some("o/r")
        );
        assert_eq!(
            repo_from_url("https://github.com/o/r/").as_deref(),
            Some("o/r")
        );
        assert_eq!(repo_from_url(""), None);
        assert_eq!(repo_from_url("   \n"), None);
        assert_eq!(repo_from_url("r"), None);
    }

    #[test]
    fn the_histogram_counts_every_key_and_orders_by_count() {
        let log = "a1 ABC-1 fix\nb2 ABC-2 and XY1-9\nc3 ABC-3\nd4 no key\n";
        assert_eq!(
            jira_histogram(log, 10),
            vec![(3, "ABC".to_string()), (1, "XY1".to_string())]
        );
    }

    #[test]
    fn ties_sort_by_key_descending_like_sort_rn() {
        let rows = jira_histogram("AAA-1 BBB-2", 10);
        assert_eq!(rows[0].1, "BBB");
    }

    #[test]
    fn the_histogram_is_capped() {
        let log: String = (b'A'..=b'Z')
            .map(|c| format!("{0}{0}-1 ", c as char))
            .collect();
        assert_eq!(jira_histogram(&log, 10).len(), 10);
    }
}
