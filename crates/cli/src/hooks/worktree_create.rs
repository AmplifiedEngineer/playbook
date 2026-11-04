// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! WorktreeCreate hook: replaces Claude Code's default worktree creation so
//! worktrees land in the playbook cc launcher's `<parent>/.worktrees/<repo>/<name>`.
//! Prints only the absolute worktree path on stdout; all git output goes to
//! stderr. Falls back to `<repo>/.claude/worktrees/<name>` when that fails.

use crate::cc::worktree::{main_worktree, resolve_base};
use crate::common::{run_with_timeout, Payload};
use crate::hooks::{worktree_setup, worktree_target};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Claude Code's default branch prefix: `worktree-<name>`.
pub(crate) const BRANCH_PREFIX: &str = "worktree-";
const GIT_TIMEOUT: Duration = Duration::from_secs(45);

/// Entry point for dispatch; the exit code only matters via [`execute`].
pub fn run(payload: &Payload) {
    let _ = execute(payload);
}

/// Runs the hook, printing the path on success. Returns the process exit code.
pub fn execute(payload: &Payload) -> i32 {
    match create(payload) {
        Ok(path) => {
            println!("{}", path.display());
            0
        }
        Err(err) => {
            eprintln!("playbook worktree-create: {err}");
            1
        }
    }
}

fn create(payload: &Payload) -> Result<PathBuf, String> {
    let name = sanitize_name(&worktree_name(payload))?;
    let cwd = PathBuf::from(payload.field(".cwd"));
    if !cwd.is_absolute() {
        return Err("payload cwd is missing or not absolute".to_string());
    }
    let porcelain = git(&cwd, &["worktree", "list", "--porcelain"])
        .map_err(|e| format!("not a git repository at {}: {e}", cwd.display()))?;
    let main = main_worktree(&porcelain).ok_or("no main worktree found")?;
    let main_root = PathBuf::from(main);
    let branch = format!("{BRANCH_PREFIX}{name}");
    if !crate::cc::worktree::valid_branch_name(&main_root, &branch) {
        return Err(format!("{branch:?} is not a valid git branch name"));
    }
    let base_commit = payload.field(".base_commit");

    // The base is ours to choose only when the payload gave none: refresh the
    // cached `origin/HEAD` first so a new worktree does not start from an old default branch.
    if base_commit.is_empty() {
        worktree_setup::refresh_origin_head(&main_root);
    }
    // The branch may already be checked out at another path (an older run, or
    // the fallback location); git refuses a second checkout, so reuse that one.
    if let Some(existing) =
        checked_out_path(&porcelain, &branch).filter(|p| !same_path(p, &main_root))
    {
        if existing.is_dir() {
            return Ok(existing);
        }
        // Registered at a folder that is gone: drop the stale entry so git
        // lets the branch be checked out again.
        git(&main_root, &["worktree", "prune"])?;
    }
    let primary = primary_target(&main_root, &name)?;
    let placed = match add_worktree(&main_root, &primary, &branch, &base_commit) {
        Ok(created) => Ok((primary, created)),
        Err(why) => {
            eprintln!("playbook worktree-create: {why}; falling back to .claude/worktrees/{name}");
            let fallback = main_root.join(".claude").join("worktrees").join(&name);
            add_worktree(&main_root, &fallback, &branch, &base_commit)
                .map(|created| (fallback, created))
                .map_err(|e| format!("fallback also failed: {e}"))
        }
    };
    let (path, created) = placed?;
    if created {
        let copied = worktree_setup::copy_worktreeinclude(&main_root, &path);
        if copied > 0 {
            eprintln!("playbook worktree-create: copied {copied} file(s) from .worktreeinclude");
        }
        if worktree_target::clone_warm_target(&main_root, &path) {
            eprintln!("playbook worktree-create: cloned the main checkout's target folder");
        }
    }
    Ok(path)
}

fn worktree_name(payload: &Payload) -> String {
    let name = payload.field(".worktree_name");
    if name.is_empty() {
        payload.field(".name")
    } else {
        name
    }
}

/// A single safe path segment: no separators, no traversal, not empty.
fn sanitize_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    let bad = name.is_empty()
        || name == "."
        || name.contains("..")
        || name.contains(['/', '\\', '\0'])
        || name.starts_with('-');
    if bad {
        Err(format!("unsafe worktree name {raw:?}"))
    } else {
        Ok(name.to_string())
    }
}

fn primary_target(main_root: &Path, name: &str) -> Result<PathBuf, String> {
    let parent = main_root
        .parent()
        .ok_or("main worktree has no parent dir")?;
    let configured = std::env::var("WORKTREE_BASE_DIR").ok();
    Ok(resolve_base(main_root, parent, configured.as_deref()).join(name))
}

/// Reuses `target` when it is already a registered worktree, else adds it.
/// Returns whether a new worktree was created (false when one was reused).
fn add_worktree(
    main_root: &Path,
    target: &Path,
    branch: &str,
    base_commit: &str,
) -> Result<bool, String> {
    if is_registered(main_root, target) {
        if target.is_dir() {
            return Ok(false);
        }
        // Registered but its folder is gone: drop the stale entry, then re-add.
        git(main_root, &["worktree", "prune"])?;
    }
    // An empty folder git does not know is what a crashed run leaves behind.
    if target.is_dir() && std::fs::read_dir(target).is_ok_and(|mut d| d.next().is_none()) {
        let _ = std::fs::remove_dir(target);
    }
    if target.exists() {
        return Err(format!(
            "{} exists but is not a worktree of this repo",
            target.display()
        ));
    }
    let parent = target.parent().ok_or("target has no parent dir")?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;

    let target_s = target.to_string_lossy().to_string();
    let branch_exists = git(
        main_root,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok();
    let base = base_ref(main_root, base_commit);
    let args: Vec<&str> = if branch_exists {
        vec!["worktree", "add", &target_s, branch]
    } else {
        vec!["worktree", "add", "-b", branch, &target_s, &base]
    };
    git(main_root, &args).map(|_| true)
}

/// Payload `base_commit`, else the cached `origin/HEAD`, else local `HEAD`.
fn base_ref(main_root: &Path, base_commit: &str) -> String {
    let candidates = [base_commit, "refs/remotes/origin/HEAD", "HEAD"];
    for candidate in candidates {
        if candidate.is_empty() || candidate.starts_with('-') {
            continue;
        }
        let spec = format!("{candidate}^{{commit}}");
        if git(main_root, &["rev-parse", "--verify", "--quiet", &spec]).is_ok() {
            return candidate.to_string();
        }
    }
    "HEAD".to_string()
}

/// Whether `path` is a registered worktree of the repo at `repo`.
pub(crate) fn is_registered(repo: &Path, path: &Path) -> bool {
    let Ok(porcelain) = git(repo, &["worktree", "list", "--porcelain"]) else {
        return false;
    };
    registered_paths(&porcelain)
        .iter()
        .any(|p| same_path(p, path))
}

/// The registered worktree path that has `branch` checked out, if any.
fn checked_out_path(porcelain: &str, branch: &str) -> Option<PathBuf> {
    let wanted = format!("branch refs/heads/{branch}");
    porcelain
        .split("\n\n")
        .find(|block| block.lines().any(|l| l == wanted))
        .and_then(|block| block.lines().find_map(|l| l.strip_prefix("worktree ")))
        .map(PathBuf::from)
}

pub(crate) fn registered_paths(porcelain: &str) -> Vec<PathBuf> {
    porcelain
        .lines()
        .filter_map(|l| l.strip_prefix("worktree ").map(PathBuf::from))
        .collect()
}

pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// Runs git in `dir`; stderr (and stdout of mutating commands) is echoed to
/// our stderr so stdout stays reserved for the path. Returns stdout on success.
pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    command.arg("-C").arg(dir).args(args);
    let out =
        run_with_timeout(&mut command, GIT_TIMEOUT).ok_or("git timed out or failed to run")?;
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if args.first() == Some(&"worktree") && args.get(1) == Some(&"add") {
        let _ = std::io::stderr().write_all(stdout.as_bytes());
    }
    if out.status.success() {
        let _ = std::io::stderr().write_all(stderr.as_bytes());
        Ok(stdout)
    } else {
        Err(stderr.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_accepts_plain_names() {
        assert_eq!(sanitize_name("probe").unwrap(), "probe");
        assert_eq!(
            sanitize_name("bright-running-fox").unwrap(),
            "bright-running-fox"
        );
    }

    #[test]
    fn sanitize_rejects_traversal_and_separators() {
        for bad in ["", " ", ".", "..", "../x", "a/b", "a\\b", "-rf", "x..y"] {
            assert!(sanitize_name(bad).is_err(), "{bad:?} should be rejected");
        }
    }
}
