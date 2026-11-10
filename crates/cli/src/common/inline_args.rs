// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The text a plugin command passes to `--args-stdin`: the flags of
//! `/playbook:commit` and `/playbook:pr-create` plus the
//! phrase "Worktree root: <path>, branch: <name>" that callers add.
//!
//! The text is split into words with plain quote handling and no shell
//! evaluation, so nothing in it is ever run. Words that are not a known flag
//! are ignored.

use crate::common::mode::{self, Mode, Resolved, Source};
use std::path::Path;

/// Everything the two commands can take.
#[derive(Debug, Default, PartialEq, Eq, Clone)]
pub struct Parsed {
    /// `-A`: stage everything.
    pub stage_all: bool,
    /// `-u`: stage tracked files.
    pub tracked: bool,
    /// `-a`: amend the last commit.
    pub amend: bool,
    pub no_signoff: bool,
    pub auto: bool,
    pub ask: bool,
    pub ready: bool,
    pub message: Option<String>,
    pub base: Option<String>,
    pub ticket: Option<String>,
    pub issue: Option<String>,
    pub dir: Option<String>,
    /// The worktree root named by the caller.
    pub root: Option<String>,
    /// The branch the caller expects in that root.
    pub branch: Option<String>,
}

/// Why the text could not be used.
#[derive(Debug, PartialEq, Eq)]
pub struct ParseError {
    /// `conflict` or `missing-value`.
    pub code: &'static str,
    pub message: String,
}

fn error(code: &'static str, message: impl Into<String>) -> ParseError {
    ParseError {
        code,
        message: message.into(),
    }
}

/// Splits on whitespace; single and double quotes group words and are
/// dropped. A backslash is kept as is. Nothing is expanded.
fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    for c in text.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                started = true;
            }
            None if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            None => {
                cur.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(cur);
    }
    out
}

/// Removes "Worktree root: <path>, branch: <name>" from `text`.
fn take_worktree(text: &str) -> (String, Option<(String, String)>) {
    let Some(start) = text.find("Worktree root:") else {
        return (text.to_string(), None);
    };
    let rest = &text[start + "Worktree root:".len()..];
    let Some(comma) = rest.find(", branch:") else {
        return (text.to_string(), None);
    };
    let root = rest[..comma].trim().to_string();
    let after = &rest[comma + ", branch:".len()..];
    let after_trim = after.trim_start();
    let name_len = after_trim
        .find(char::is_whitespace)
        .unwrap_or(after_trim.len());
    let branch = after_trim[..name_len].trim_end_matches(',').to_string();
    let consumed = text.len() - after_trim.len() + name_len;
    let mut remaining = text[..start].to_string();
    remaining.push(' ');
    remaining.push_str(&text[consumed..]);
    if root.is_empty() || branch.is_empty() {
        return (remaining, None);
    }
    (remaining, Some((root, branch)))
}

/// Parses the text of one invocation.
pub fn parse(text: &str) -> Result<Parsed, ParseError> {
    let (text, worktree) = take_worktree(text);
    let mut p = Parsed::default();
    if let Some((root, branch)) = worktree {
        p.root = Some(root);
        p.branch = Some(branch);
    }
    let list = words(&text);
    let mut i = 0;
    while i < list.len() {
        let word = list[i].as_str();
        let mut value = |flag: &str| -> Result<String, ParseError> {
            i += 1;
            list.get(i)
                .filter(|v| !v.starts_with("--"))
                .cloned()
                .ok_or_else(|| error("missing-value", format!("{flag} needs a value")))
        };
        match word {
            "--auto" => p.auto = true,
            "--ask" => p.ask = true,
            "--no-signoff" => p.no_signoff = true,
            "--all" => p.stage_all = true,
            "--update" => p.tracked = true,
            "--amend" => p.amend = true,
            "--ready" => p.ready = true,
            "-m" | "--message" => p.message = Some(value(word)?),
            "--base" => p.base = Some(value(word)?),
            "--ticket" => p.ticket = Some(value(word)?),
            "--issue" => p.issue = Some(value(word)?),
            "--dir" => p.dir = Some(value(word)?),
            w if w.len() > 1
                && w.starts_with('-')
                && !w.starts_with("--")
                && w[1..].chars().all(|c| matches!(c, 'A' | 'u' | 'a')) =>
            {
                for c in w[1..].chars() {
                    match c {
                        'A' => p.stage_all = true,
                        'u' => p.tracked = true,
                        _ => p.amend = true,
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    if p.auto && p.ask {
        return Err(error("conflict", "--auto and --ask conflict; pass one"));
    }
    Ok(p)
}

impl Parsed {
    /// The run mode: a flag, else the environment, else config.
    pub fn mode_at(&self, env: Option<&str>, home: &Path, slug: Option<&str>) -> Resolved {
        let flag = if self.auto {
            Some(Mode::Auto)
        } else if self.ask {
            Some(Mode::Ask)
        } else {
            None
        };
        let mut resolved = mode::resolve_for_hook_at(env, home, slug);
        if let Some(mode) = flag {
            resolved.mode = mode;
            resolved.source = Source::Flag;
        }
        resolved
    }
}

/// Checks that `root` is on `branch`. `Err` carries the sentence for a
/// `WRONG_DIRECTORY` verdict.
pub fn guard(root: &Path, branch: &str) -> Result<(), String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["branch", "--show-current"])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("could not run git in {}: {e}", root.display()))?;
    if !out.status.success() {
        return Err(format!("{} is not a git worktree", root.display()));
    }
    let actual = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if actual == branch {
        Ok(())
    } else {
        Err(format!(
            "{} is on '{actual}', not '{branch}'; run from the right worktree",
            root.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_short_flags_and_auto() {
        let p = parse("-Au --auto").unwrap();
        assert!(p.stage_all && p.tracked && p.auto && !p.amend);
    }

    #[test]
    fn the_long_forms_of_the_stage_and_amend_flags_are_parsed() {
        let p = parse("--all --update --amend").unwrap();
        assert!(p.stage_all && p.tracked && p.amend);
    }

    #[test]
    fn amend_and_no_signoff() {
        let p = parse("-a --no-signoff").unwrap();
        assert!(p.amend && p.no_signoff);
    }

    #[test]
    fn auto_and_ask_conflict() {
        assert_eq!(parse("--auto --ask").unwrap_err().code, "conflict");
    }

    #[test]
    fn a_message_keeps_its_quote() {
        let p = parse(r#"-m "fix(x): it's done""#).unwrap();
        assert_eq!(p.message.as_deref(), Some("fix(x): it's done"));
    }

    #[test]
    fn shell_text_is_never_run() {
        let marker = std::env::temp_dir().join(format!("pb-sentinel-{}", std::process::id()));
        let text = format!("$(touch {}) ; ls", marker.display());

        let p = parse(&text).unwrap();

        assert_eq!(p, Parsed::default());
        assert!(!marker.exists());
    }

    #[test]
    fn empty_text_defaults_to_ask_with_no_config() {
        let home = std::env::temp_dir().join(format!("pb-inline-home-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();

        let p = parse("").unwrap();
        let mode = p.mode_at(None, &home, None);

        assert_eq!(p, Parsed::default());
        assert_eq!(mode.mode, Mode::Ask);
        assert_eq!(mode.source, Source::Default);
    }

    #[test]
    fn a_flag_without_its_value_is_an_error() {
        assert_eq!(parse("-m").unwrap_err().code, "missing-value");
        assert_eq!(parse("--base --auto").unwrap_err().code, "missing-value");
    }

    #[test]
    fn the_worktree_phrase_is_taken_out() {
        let p = parse("-A Worktree root: /w, branch: feat/b --auto").unwrap();
        assert_eq!(p.root.as_deref(), Some("/w"));
        assert_eq!(p.branch.as_deref(), Some("feat/b"));
        assert!(p.stage_all && p.auto);
    }

    #[test]
    fn parity_worktree_guard_a_root_on_another_branch_is_wrong_directory() {
        let dir = std::env::temp_dir().join(format!("pb-inline-wt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap()
        };
        run(&["init", "-q", "-b", "c"]);

        let err = guard(&dir, "b").unwrap_err();

        assert!(err.contains("not 'b'"), "{err}");
        assert!(guard(&dir, "c").is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn base_ticket_issue_and_dir_are_parsed() {
        let p = parse("--base main --ticket PROJ-1 --issue 4 --dir /w").unwrap();
        assert_eq!(p.base.as_deref(), Some("main"));
        assert_eq!(p.ticket.as_deref(), Some("PROJ-1"));
        assert_eq!(p.issue.as_deref(), Some("4"));
        assert_eq!(p.dir.as_deref(), Some("/w"));
    }
}
