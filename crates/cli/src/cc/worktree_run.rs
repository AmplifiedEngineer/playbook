// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Wires the ported `cc::worktree` library into the `playbook cc launch worktree <branch>
//! [env-base]` subcommand, the Rust replacement for the retired shell launcher's
//! `playbook cc launch worktree`.
//!
//! Every git-driving decision already lives in [`crate::cc::worktree`]; this
//! module is composition only, keeping the order the
//! retired shell ran them in.
//!
//! OUTPUT CONTRACT: stdout carries ONLY the final worktree path on success, so
//! a shell can safely `cd "$(playbook cc worktree foo)"`. Every message, human
//! or diagnostic, goes to stderr, on both the success and the failure paths.

use crate::cc::worktree;
use crate::common::git;
use crate::common::run_with_timeout;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const GH_TIMEOUT: Duration = Duration::from_secs(5);

/// Every worktree lives under this remote; the shell hardcodes the same
/// value (`local REMOTE="origin"` in `_cc_worktree`, worktree.sh:464).
const REMOTE: &str = "origin";

// Exit codes match the retired shell launcher's, so a caller cannot tell the
// port apart from it by exit status alone.
const EXIT_USAGE_OR_INVALID_BRANCH: i32 = 2;
const EXIT_CREATE_FAILED: i32 = 3;
const EXIT_COULD_NOT_ENTER: i32 = 4;
const EXIT_OCCUPIED_ON_REMOTE: i32 = 5;
const EXIT_NOT_A_GIT_REPO: i32 = 10;

/// Runs the whole `playbook cc launch worktree <branch> [env-base]` flow starting from
/// `start_dir`, printing the worktree path to stdout on success and returning
/// the process exit code. `start_dir` stands in for the shell's implicit cwd
/// at the moment `_cc_worktree` is invoked, passed explicitly rather than
/// read from `std::env::current_dir()` here so tests never have to touch
/// process-global state to point this at a scratch repo.
pub fn run(start_dir: &Path, branch_raw: &str, env_base_arg: Option<&str>) -> i32 {
    match setup(start_dir, branch_raw, env_base_arg) {
        Ok((path, housekeep)) => {
            housekeep.run();
            println!("{}", path.display());
            0
        }
        Err(code) => code,
    }
}

/// The slow background half of a worktree setup: fetch, upstream, sync and
/// sweep. The caller runs it in place or hands it to a detached process.
pub struct Housekeep {
    repo_root: PathBuf,
    worktree_path: PathBuf,
    branch: String,
    no_push: bool,
}

impl Housekeep {
    pub fn new(repo_root: &Path, worktree_path: &Path, branch: &str, no_push: bool) -> Self {
        Housekeep {
            repo_root: repo_root.to_path_buf(),
            worktree_path: worktree_path.to_path_buf(),
            branch: branch.to_string(),
            no_push,
        }
    }

    pub fn run(&self) {
        run_housekeep(
            &self.repo_root,
            &self.worktree_path,
            &self.branch,
            self.no_push,
        );
    }

    /// Runs the housekeeping in its own silent process group, so it outlives
    /// the launcher and never writes into the session's terminal.
    pub fn spawn_detached(&self) {
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let mut cmd = Command::new(exe);
        cmd.args(["cc", "housekeep", "--repo-root"])
            .arg(&self.repo_root)
            .arg("--worktree")
            .arg(&self.worktree_path)
            .args(["--branch", &self.branch])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        if self.no_push {
            cmd.arg("--no-push");
        }
        let _ = cmd.spawn();
    }
}

/// The same flow as [`run`], returning the worktree path instead of printing
/// it, so the launcher can enter it. Messages still go to stderr. The
/// housekeeping is returned, not run.
pub fn setup(
    start_dir: &Path,
    branch_raw: &str,
    env_base_arg: Option<&str>,
) -> Result<(PathBuf, Housekeep), i32> {
    eprintln!("worktree: setting up '{branch_raw}'...");

    if !git::ok(start_dir, &["rev-parse", "--is-inside-work-tree"]) {
        eprintln!("worktree: not a git repository");
        return Err(EXIT_NOT_A_GIT_REPO);
    }
    let porcelain = git::raw(start_dir, &["worktree", "list", "--porcelain"]).unwrap_or_default();
    let Some(main_wt) = worktree::main_worktree(&porcelain) else {
        eprintln!("worktree: couldn't cd to main worktree:");
        return Err(EXIT_NOT_A_GIT_REPO);
    };
    let repo_root = PathBuf::from(main_wt);

    let branch = worktree::sanitize_branch(branch_raw);
    if branch.is_empty() {
        eprintln!("worktree: usage: worktree <branch-name> [env-base-folder]");
        return Err(EXIT_USAGE_OR_INVALID_BRANCH);
    }
    if !worktree::valid_branch_name(&repo_root, &branch) {
        eprintln!("worktree: invalid branch name: '{branch}'");
        return Err(EXIT_USAGE_OR_INVALID_BRANCH);
    }

    let repo_parent = repo_root.parent().unwrap_or(&repo_root);
    let configured_base = std::env::var("WORKTREE_BASE_DIR").ok();
    let wt_root = worktree::resolve_base(&repo_root, repo_parent, configured_base.as_deref());
    let _ = std::fs::create_dir_all(&wt_root);

    let base_ref = worktree::main_base_ref(&repo_root);
    worktree::initial_fetch(&repo_root, REMOTE, &base_ref, &branch);

    // Whether a stash was actually taken, not merely attempted: only that
    // governs whether restore_stash below has anything to pop.
    let stash_applied = worktree::needs_stash(&repo_root) && worktree::auto_stash(&repo_root);
    let no_push = env_flag("WORKTREE_NO_PUSH");

    // A plain sequential call, not a try/catch: every exit from
    // `prepare_and_finish` is a normal return, so this line always runs next,
    // which is what guarantees a stash taken above is never left behind on a
    // failure path.
    let outcome = prepare_and_finish(&repo_root, &wt_root, &branch, env_base_arg, &base_ref);
    worktree::restore_stash(&repo_root, stash_applied);

    match outcome {
        Ok(path) => {
            if no_push {
                eprintln!(
                    "worktree: --no-push set; will not auto-create the remote branch. Run 'git push -u {REMOTE} {branch}' when ready."
                );
            }
            eprintln!("Ready: {}", path.display());
            let housekeep = Housekeep::new(&repo_root, &path, &branch, no_push);
            Ok((path, housekeep))
        }
        Err(code) => Err(code),
    }
}

/// Everything from picking the target folder through housekeeping: the part
/// of the flow that can fail after the auto-stash decision. Factored out so
/// [`run`] can call [`worktree::restore_stash`] exactly once, unconditionally,
/// right after this returns, regardless of which branch below it took.
fn prepare_and_finish(
    repo_root: &Path,
    wt_root: &Path,
    branch: &str,
    env_base_arg: Option<&str>,
    base_ref: &str,
) -> Result<PathBuf, i32> {
    let target = resolve_target(repo_root, wt_root, branch);
    let env_base = worktree::find_env_base(repo_root, env_base_arg);

    let worktree_path = match worktree::prepare_worktree(
        repo_root,
        &target,
        branch,
        REMOTE,
        env_base.as_deref(),
    ) {
        worktree::WorktreeOutcome::Refused(current) => {
            eprintln!(
                "worktree: worktree at {} is on branch '{current}' which still exists on remote. Finish it or remove the worktree first.",
                target.display()
            );
            return Err(EXIT_OCCUPIED_ON_REMOTE);
        }
        worktree::WorktreeOutcome::CreateFailed => {
            eprintln!("git worktree add failed");
            return Err(EXIT_CREATE_FAILED);
        }
        worktree::WorktreeOutcome::Ready(path, env_copy) => {
            if let worktree::EnvCopy::RefusedNotGitignored(rel) = env_copy {
                eprintln!(
                    "worktree: {rel} is not gitignored in the source repo; skipping .env copy to avoid staging secrets."
                );
            }
            path
        }
    };

    if !worktree_path.is_dir() {
        eprintln!("worktree: couldn't cd to {}", worktree_path.display());
        return Err(EXIT_COULD_NOT_ENTER);
    }

    if worktree::attach_if_detached(&worktree_path, branch) {
        eprintln!("Worktree created detached. Attaching to {branch}...");
    }

    run_rebase(&worktree_path, branch, base_ref);

    Ok(worktree_path)
}

/// The JIRA-key-or-leaf folder for `branch`, disambiguated against whatever
/// already occupies that folder: `_wt_main`'s FOLDER/TARGET setup
/// (worktree.sh:282-294), composed from the already-ported [`worktree::jira_key`],
/// [`worktree::branch_leaf`] and [`worktree::folder_for_branch`].
fn resolve_target(repo_root: &Path, wt_root: &Path, branch: &str) -> PathBuf {
    let jira = worktree::jira_key(branch);
    let provisional = jira
        .clone()
        .unwrap_or_else(|| worktree::branch_leaf(branch).to_string());
    let provisional_target = wt_root.join(&provisional);

    // Only worth asking who occupies the folder when a JIRA key even picked
    // one: a plain leaf folder has no collision to disambiguate.
    let occupied_by = if jira.is_some() && provisional_target.is_dir() {
        let porcelain =
            git::raw(repo_root, &["worktree", "list", "--porcelain"]).unwrap_or_default();
        branch_at(&porcelain, &provisional_target)
    } else {
        None
    };

    let folder = worktree::folder_for_branch(branch, occupied_by.as_deref());
    if folder != provisional {
        eprintln!(
            "Worktree {} in use by '{}'. Using '{folder}' instead.",
            jira.as_deref().unwrap_or(""),
            occupied_by.as_deref().unwrap_or("")
        );
    }
    wt_root.join(folder)
}

/// The branch checked out at `target`, from `git worktree list --porcelain`
/// output. A miniature of `cc::worktree`'s private `target_worktree_info`,
/// duplicated here since that helper only returns the registration flag
/// alongside the branch and is not part of the module's public surface.
fn branch_at(porcelain: &str, target: &Path) -> Option<String> {
    let target_str = target.to_string_lossy();
    let mut in_target = false;
    for line in porcelain.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            in_target = path == target_str.as_ref();
        } else if in_target {
            if let Some(b) = line.strip_prefix("branch ") {
                return Some(b.trim_start_matches("refs/heads/").to_string());
            }
        }
    }
    None
}

/// Gathers what [`worktree::maybe_rebase`] needs and reports a conflict, the
/// only outcome it leaves to the caller (see its own doc comment). The
/// `--ai-resolve` spawn path is not wired here either: `maybe_rebase`'s doc
/// defers it to "whatever wires the CLI", and this Work Unit is wiring for a
/// `cd`-only shim, not an interactive resolver.
fn run_rebase(worktree_path: &Path, branch: &str, base_ref: &str) {
    let current_branch = git::raw(worktree_path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let git_user = git::raw(worktree_path, &["config", "user.name"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let branch_author = git::raw(
        worktree_path,
        &["log", "-1", "--format=%an", &current_branch],
    )
    .map(|s| s.trim().to_string())
    .unwrap_or_default();
    let gh_user = gh_login();

    let ctx = worktree::RebaseContext {
        current_branch: &current_branch,
        git_user: &git_user,
        branch_author: &branch_author,
        gh_user: &gh_user,
        wanted_branch: branch,
        base_ref,
    };

    if let Some(worktree::RebaseOutcome::Conflicted) =
        worktree::maybe_rebase(&ctx, worktree_path, REMOTE)
    {
        eprintln!(
            "Rebase conflict on {current_branch} onto {REMOTE}/{base_ref}. Aborting (pass --ai-resolve to let Claude fix it)."
        );
    }
}

/// The GitHub login `_wt_maybe_rebase` uses to spot a branch named for you
/// (worktree.sh:409), empty when `gh` is absent, unauthenticated, or slow.
fn gh_login() -> String {
    let mut command = Command::new("gh");
    command.args(["api", "user", "--jq", ".login"]);
    match run_with_timeout(&mut command, GH_TIMEOUT) {
        Some(out) if out.status.success() => {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        _ => String::new(),
    }
}

/// Runs the ported background block (worktree.sh:372-384) SYNCHRONOUSLY.
///
/// The shell backgrounds and disowns this block so it outlives the shell that
/// spawned it (worktree.sh:385-386); how a Rust process should do the
/// equivalent (a detached child, a thread the CLI declines to join, something
/// else) is an open design question this Work Unit defers rather than
/// guesses at, matching [`worktree::housekeep`]'s own doc comment.
///
/// `housekeep`'s own nested stale-worktree reaper ([`worktree::cleanup_stale_with`])
/// stays permanently neutered here via a marker freshly touched every call,
/// the same way it always has: that reaper is the pre-`gh-366` port, kept
/// only for the tests that still exercise it directly, and is not this
/// binary's active stale-worktree path any more. The real cleanup now runs
/// through [`crate::gc::worktrees::sweep_locked`] instead, called separately below with
/// its own classification-and-landed-signal logic and its own
/// `worktreeCleanup.*` config gate, in-process rather than as a subprocess.
fn run_housekeep(repo_root: &Path, worktree_path: &Path, branch: &str, no_push: bool) {
    let now = crate::common::time::now_secs();

    let fetch_marker = worktree::fetch_cache_marker_path(repo_root)
        .unwrap_or_else(|| std::env::temp_dir().join("playbook-wt-fetch-scratch"));
    let cleanup_marker = std::env::temp_dir().join("playbook-wt-housekeep-neutral");
    let _ = std::fs::write(&cleanup_marker, now.to_string());

    let ctx = worktree::HousekeepContext {
        worktree: worktree_path,
        repo_root,
        remote: REMOTE,
        branch,
        no_push,
    };
    worktree::housekeep(&ctx, &fetch_marker, &cleanup_marker, &[], now);

    let home = crate::common::home_dir();
    let slug = crate::common::repo_slug();
    let repo_slug = if slug.is_empty() {
        None
    } else {
        Some(slug.as_str())
    };
    // `worktree_path`, not `repo_root`: `sweep` protects whichever worktree
    // it is invoked FROM, and this call runs from inside the worktree this
    // very invocation just created, so passing the main checkout here would
    // leave the new worktree unprotected and reapable the instant it looks
    // merged (a fresh branch with no divergent commits is trivially
    // "merged" into its own base).
    // A busy gc run means the sweep is skipped, and the next trigger covers it.
    let _ = crate::gc::worktrees::sweep_locked(
        &home,
        &*crate::gc::default_clock(),
        (worktree_path, repo_slug),
        false,
    );
}

/// Matches the shell's `[[ -n "${VAR:-}" && "$VAR" != "0" ]]` truthiness test
/// (worktree.sh:486), reused here for `WORKTREE_NO_PUSH`.
fn env_flag(name: &str) -> bool {
    matches!(std::env::var(name), Ok(v) if !v.is_empty() && v != "0")
}
