// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The drafting commit behind `playbook commit run -A` and the hidden legacy
//! `playbook commit all`: stage, draft a conventional message with a small
//! model (or from the staged files when it is off or fails), then commit, rebase when behind and push through the
//! same code as the message-file path of `commit run`.

use super::model::{self, MessageRunner};
use super::typecheck::{kind_of, Kind};
use super::{current_branch, git_text, prepare, run};
use std::path::Path;

/// Longest commit header the drafter writes.
const MAX_HEADER: usize = 72;
/// What `prepare` prints when nothing is staged.
const NOTHING_STAGED: &str = "NO_STAGED_CHANGES";
/// Folder that holds the crates, so the crate name is the area.
const CRATES_DIR: &str = "crates";
const DEPS_SCOPE: &str = "deps";

/// Flags of one run.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub message: Option<String>,
    pub auto: bool,
    pub no_signoff: bool,
    /// `git add -A` first.
    pub stage_all: bool,
    /// `git add -u` first.
    pub stage_update: bool,
    /// Replace the last commit; its message is kept unless `message` is set.
    pub amend: bool,
    /// Skip the conventional commit type guard.
    pub no_type_check: bool,
    /// Skip the prose check on a given message; a drafted one is never checked.
    pub no_prose_check: bool,
}

/// One staged file: its git status letter and path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub status: char,
    pub path: String,
}

/// Reads `git diff --name-status` lines. A rename keeps its new path.
pub fn parse_changes(name_status: &str) -> Vec<Change> {
    name_status
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let status = parts.next()?.chars().next()?;
            let path = parts.next_back()?;
            Some(Change {
                status,
                path: path.to_string(),
            })
        })
        .collect()
}

/// The area a path belongs to: the crate for `crates/<name>/...`, `plugin`
/// for `plugins/...`, `ci` for `.github/...`, else the top folder. A file in
/// the repo root has no area.
fn area_of(path: &str) -> Option<String> {
    // A file in the repo root has no folder, so no area.
    let (top, rest) = path.split_once('/')?;
    let second = rest.split('/').next().unwrap_or(rest);
    let area = match top {
        CRATES_DIR => second,
        "plugins" => "plugin",
        ".github" => "ci",
        other => other,
    };
    Some(area.to_string())
}

/// The shared area of all files, or `None` when they differ.
fn common_area(changes: &[Change]) -> Option<String> {
    let mut areas = changes.iter().map(|c| area_of(&c.path));
    let first = areas.next()??;
    areas
        .all(|a| a.as_deref() == Some(first.as_str()))
        .then_some(first)
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// A conventional commit message for the staged changes. It is a guide, the
/// PR title and body carry the meaning.
pub fn draft(changes: &[Change]) -> String {
    let paths: Vec<&str> = changes.iter().map(|c| c.path.as_str()).collect();
    let area = common_area(changes);
    let kind = kind_of(&paths);
    let added: Vec<&Change> = changes.iter().filter(|c| c.status == 'A').collect();

    let (ty, scope, subject) = match kind {
        Some(Kind::Deps) => (
            "chore",
            Some(DEPS_SCOPE.to_string()),
            "update dependencies".to_string(),
        ),
        Some(Kind::Docs) => ("docs", None, subject_for(changes, &area)),
        Some(Kind::Ci) => ("ci", None, subject_for(changes, &area)),
        Some(Kind::Tests) => ("test", area.clone(), subject_for(changes, &area)),
        None => (
            if added.is_empty() { "fix" } else { "feat" },
            area.clone(),
            subject_for(changes, &area),
        ),
    };
    let head = match scope {
        Some(scope) => format!("{ty}({scope}): "),
        None => format!("{ty}: "),
    };
    let room = MAX_HEADER.saturating_sub(head.chars().count());
    let subject: String = subject.chars().take(room).collect();
    format!("{head}{subject}")
}

fn subject_for(changes: &[Change], area: &Option<String>) -> String {
    let mut added = changes.iter().filter(|c| c.status == 'A');
    if let (Some(only), None, 1) = (added.next(), added.next(), changes.len()) {
        return format!("add {}", file_name(&only.path));
    }
    match area {
        Some(area) => format!("update {area}"),
        None => "update files".to_string(),
    }
}

/// What the note says when the message came from file names.
const NOTE_PREFIX: &str = "message drafted from file names, no model: ";
/// The longest reason kept in a note.
const NOTE_REASON_CHARS: usize = 200;

/// The message for `changes`: the model's when `ai` is given and its reply is
/// valid, else the file name draft with a note that says why. `context` runs
/// only when a model is asked.
fn compose(
    ai: Option<&dyn MessageRunner>,
    context: impl FnOnce() -> model::Context,
    changes: &[Change],
) -> (String, Option<String>) {
    let Some(runner) = ai else {
        return (draft(changes), None);
    };
    let paths: Vec<&str> = changes.iter().map(|c| c.path.as_str()).collect();
    match model::draft(runner, &context(), &paths) {
        Ok(message) => (message, None),
        Err(reason) => {
            let reason: String = reason
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .take(NOTE_REASON_CHARS)
                .collect();
            (draft(changes), Some(format!("{NOTE_PREFIX}{reason}")))
        }
    }
}

/// Stage everything, commit, rebase when behind and push. `Ok` is the text to
/// print, `Err` the text for stderr. `ai` is the model that drafts the
/// message, or `None` for the file name draft.
pub fn run(
    dir: &Path,
    opts: Options,
    config_signoff: bool,
    ai: Option<&dyn MessageRunner>,
) -> Result<String, String> {
    run_noted(dir, opts, config_signoff, ai).map(|(text, _)| text)
}

/// Like [`run`], and also the note that says the message came from file names.
pub fn run_noted(
    dir: &Path,
    opts: Options,
    config_signoff: bool,
    ai: Option<&dyn MessageRunner>,
) -> Result<(String, Option<String>), String> {
    let staged = prepare::run(
        dir,
        prepare::Options {
            stage_all: opts.stage_all,
            stage_update: opts.stage_update,
            amend: opts.amend,
        },
    )?;
    if staged == NOTHING_STAGED {
        return Ok((staged, None));
    }
    let name_status = git_text(
        dir,
        &[
            "-c",
            "core.quotepath=off",
            "diff",
            "--staged",
            "--name-status",
        ],
    )
    .unwrap_or_default();
    let changes = parse_changes(&name_status);
    let kept = if opts.amend && opts.message.is_none() {
        git_text(dir, &["log", "-1", "--format=%B"]).filter(|m| !m.is_empty())
    } else {
        None
    };
    let drafted = opts.message.is_none() && kept.is_none();
    let mut note = None;
    let message = match opts.message.or(kept) {
        Some(message) => message,
        None => {
            let (message, why) = compose(
                ai,
                || model::gather(dir, &current_branch(dir), &name_status),
                &changes,
            );
            note = why;
            message
        }
    };

    let file = std::env::temp_dir().join(format!("playbook-commit-all-{}.txt", std::process::id()));
    std::fs::write(&file, format!("{message}\n"))
        .map_err(|e| format!("could not store the message: {e}"))?;
    let result = run::run(
        dir,
        &file,
        run::Options {
            amend: opts.amend,
            auto: opts.auto,
            no_signoff: opts.no_signoff,
            no_prose_check: drafted || opts.no_prose_check,
            no_type_check: opts.no_type_check,
        },
        config_signoff,
    );
    let _ = std::fs::remove_file(&file);
    let pushed = result?;

    let sha = git_text(dir, &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
    let note_line = note
        .as_ref()
        .map(|n| format!("\nNOTE={n}"))
        .unwrap_or_default();
    let text = format!(
        "{pushed}\nBRANCH={}{note_line}\nCOMMIT={sha}\nMESSAGE={}",
        current_branch(dir),
        message.lines().next().unwrap_or("")
    );
    Ok((text, note))
}

/// The `--json` form. Parses `text` (the command's arguments and the
/// worktree phrase), runs in the named root and reports in the shared
/// envelope. `base` carries the message and the check skips given on the
/// command line; staging, mode and amend come from `text`. It never fails the
/// process: the outcome is in `verdict`.
pub fn run_inline(
    text: &str,
    base: Options,
    config_signoff: bool,
    ai: Option<&dyn MessageRunner>,
) -> serde_json::Value {
    use crate::common::inline_args;
    use crate::json::envelope::wrap;
    use serde_json::json;

    let refuse = |verdict: &str, code: i32, why: String, mode: &str| {
        wrap(
            code,
            &why,
            json!({
                "verdict": verdict, "branch": null, "root": null, "commit": "",
                "message": "", "pushed": false, "rebased": false, "forced": false,
                "mode": mode, "warnings": [], "errors": [why],
            }),
        )
    };
    let parsed = match inline_args::parse(text) {
        Ok(p) => p,
        Err(e) => return refuse("REFUSED", 1, format!("{}: {}", e.code, e.message), "ask"),
    };
    let home = crate::common::home_dir();
    let slug = crate::common::repo_slug();
    let mode = parsed
        .mode_at(
            std::env::var("PLAYBOOK_MODE").ok().as_deref(),
            &home,
            (!slug.is_empty()).then_some(slug.as_str()),
        )
        .mode;
    let auto = mode == crate::common::mode::Mode::Auto;
    let mode_text = if auto { "auto" } else { "ask" };
    let dir = std::path::PathBuf::from(
        parsed
            .root
            .clone()
            .or(parsed.dir.clone())
            .unwrap_or_else(|| ".".into()),
    );
    if let (Some(root), Some(branch)) = (&parsed.root, &parsed.branch) {
        if let Err(why) = inline_args::guard(Path::new(root), branch) {
            return refuse("WRONG_DIRECTORY", 1, why, mode_text);
        }
    }
    let top = git_text(&dir, &["rev-parse", "--show-toplevel"]);
    let Some(top) = top else {
        return refuse(
            "REFUSED",
            1,
            format!("{} is not a git worktree", dir.display()),
            mode_text,
        );
    };
    let root = Path::new(&top);
    let opts = Options {
        message: base.message.or_else(|| parsed.message.clone()),
        auto,
        no_signoff: parsed.no_signoff,
        stage_all: parsed.stage_all,
        stage_update: parsed.tracked,
        amend: parsed.amend,
        no_type_check: base.no_type_check,
        no_prose_check: base.no_prose_check,
    };
    // Before the commit: afterwards nothing is staged to look at. A protected
    // branch is refused by `run` without touching the index.
    let on_protected = prepare::is_protected(&current_branch(root), &prepare::default_branch(root));
    let mut warnings = if on_protected {
        Vec::new()
    } else {
        if parsed.stage_all {
            let _ = git_text(root, &["add", "-A"]);
        } else if parsed.tracked {
            let _ = git_text(root, &["add", "-u"]);
        }
        crate::hooks::precommit_check::findings_in(Some(root))
    };
    let (result, note) = match run_noted(root, opts, config_signoff, ai) {
        Ok((text, note)) => (Ok(text), note),
        Err(text) => (Err(text), None),
    };
    warnings.extend(note);
    let branch = current_branch(root);
    let sha = git_text(root, &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
    let message = git_text(root, &["log", "-1", "--format=%B"]).unwrap_or_default();
    let (verdict, code, log, error) = match &result {
        Ok(text) if text.trim() == NOTHING_STAGED => {
            (NOTHING_STAGED, 0, String::new(), String::new())
        }
        // An aborted rebase still pushes the branch as it was, but the caller
        // must know it is not on the base yet.
        Ok(text) if text.contains("Rebase conflict") => (
            "CONFLICT",
            4,
            text.clone(),
            "rebase conflict: the branch was pushed without the rebase; run the git rebase it names by hand".to_string(),
        ),
        Ok(text) => ("PUSHED", 0, text.clone(), String::new()),
        Err(text) if text.contains("PARKED") => ("PARKED", 1, text.clone(), text.clone()),
        Err(text)
            if text.contains("never commits there")
                || text.contains("breaks the writing rules")
                || text.contains("commit type check") =>
        {
            ("REFUSED", 1, text.clone(), text.clone())
        }
        Err(text) => ("FAILED", 1, text.clone(), text.clone()),
    };
    let pushed = verdict == "PUSHED" || verdict == "CONFLICT";
    let rebased = log.contains("Rebasing") && !log.contains("Rebase conflict");
    let forced = pushed && !auto && (parsed.amend || rebased);
    // After a refusal the HEAD commit is not ours, so it is not reported.
    let (commit, message) = if pushed {
        (sha, message.lines().next().unwrap_or("").to_string())
    } else {
        (String::new(), String::new())
    };
    wrap(
        code,
        &error,
        json!({
            "verdict": verdict, "branch": branch, "root": top, "commit": commit,
            "message": message, "pushed": pushed, "rebased": rebased, "forced": forced,
            "mode": mode_text, "warnings": warnings,
            "errors": if error.is_empty() { vec![] } else { vec![error.clone()] },
            "log": log,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changes(lines: &str) -> Vec<Change> {
        parse_changes(lines)
    }

    #[test]
    fn a_rename_keeps_its_new_path() {
        let c = changes("R100\told.rs\tnew.rs\nM\ta.rs");
        assert_eq!(c[0].path, "new.rs");
        assert_eq!(c[1].status, 'M');
    }

    #[test]
    fn the_crate_is_the_area_of_a_crate_file() {
        assert_eq!(area_of("crates/cli/src/a.rs").as_deref(), Some("cli"));
        assert_eq!(area_of("plugins/playbook/a.md").as_deref(), Some("plugin"));
        assert_eq!(area_of(".github/workflows/x.yml").as_deref(), Some("ci"));
        assert_eq!(area_of("README.md"), None);
    }

    #[test]
    fn the_header_never_passes_the_limit() {
        let name = "x".repeat(200);
        let msg = draft(&changes(&format!("A\tcrates/cli/src/{name}.rs")));
        assert!(msg.chars().count() <= MAX_HEADER, "{msg}");
    }

    #[test]
    fn a_workflow_change_is_ci() {
        assert_eq!(
            draft(&changes("M\t.github/workflows/a.yml")),
            "ci: update ci"
        );
    }

    struct Fake {
        reply: Result<String, String>,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl Fake {
        fn new(reply: Result<String, String>) -> Self {
            Fake {
                reply,
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl MessageRunner for Fake {
        fn run(&self, _prompt: &str) -> Result<String, String> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.reply.clone()
        }
    }

    fn good_reply() -> String {
        serde_json::json!({
            "is_error": false,
            "structured_output": {
                "header": "fix(cli): stop the double send",
                "body": "",
            },
        })
        .to_string()
    }

    #[test]
    fn a_valid_model_reply_is_the_message_with_no_note() {
        // Arrange
        let runner = Fake::new(Ok(good_reply()));
        let staged = changes("M\tcrates/cli/src/a.rs");

        // Act
        let (message, note) = compose(Some(&runner), model::Context::default, &staged);

        // Assert
        assert_eq!(message, "fix(cli): stop the double send");
        assert_eq!(note, None);
        assert_eq!(runner.calls(), 1);
    }

    #[test]
    fn a_runner_error_falls_back_to_the_draft_with_a_note() {
        // Arrange
        let runner = Fake::new(Err("claude exited with 1".to_string()));
        let staged = changes("M\tcrates/cli/src/a.rs");

        // Act
        let (message, note) = compose(Some(&runner), model::Context::default, &staged);

        // Assert
        assert_eq!(message, draft(&staged));
        assert_eq!(
            note.as_deref(),
            Some("message drafted from file names, no model: claude exited with 1")
        );
    }

    #[test]
    fn invalid_json_and_is_error_fall_back_to_the_draft() {
        // Arrange
        let staged = changes("M\tcrates/cli/src/a.rs");
        let replies = [
            "not json".to_string(),
            serde_json::json!({"is_error": true}).to_string(),
        ];

        for raw in replies {
            let runner = Fake::new(Ok(raw));

            // Act
            let (message, note) = compose(Some(&runner), model::Context::default, &staged);

            // Assert
            assert_eq!(message, draft(&staged));
            assert!(note.is_some_and(|n| n.starts_with(NOTE_PREFIX)));
        }
    }

    #[test]
    fn a_rejected_reply_falls_back_to_the_draft() {
        // Arrange
        let raw = serde_json::json!({
            "is_error": false,
            "structured_output": {"header": "feat(docs): explain it", "body": ""},
        })
        .to_string();
        let runner = Fake::new(Ok(raw));
        let staged = changes("M\tdocs/a.md");

        // Act
        let (message, note) = compose(Some(&runner), model::Context::default, &staged);

        // Assert
        assert_eq!(message, draft(&staged));
        assert!(note.is_some_and(|n| n.contains("commit type check")));
    }

    #[test]
    fn with_the_model_off_the_heuristic_runs_and_nothing_is_gathered() {
        // Arrange
        let staged = changes("M\tcrates/cli/src/a.rs");

        // Act
        let (message, note) = compose(None, || unreachable!("no context without a model"), &staged);

        // Assert
        assert_eq!(message, draft(&staged));
        assert_eq!(note, None);
    }
}
