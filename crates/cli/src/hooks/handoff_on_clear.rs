// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! SessionEnd hook for reason `clear`: writes the handoff the session did not.
//!
//! No hook runs before `/clear`, but `SessionEnd` does, and `/clear` waits for
//! it. The hook resumes the ended session as a fork, with no tools, asks for
//! the handoff template, and saves the answer tagged with the session id. The
//! existing clear marker and the SessionStart loader then hand it to the new
//! session. It is off unless `handoff.onClear` is true, skips a session that
//! saved a handoff a few minutes ago or has a very large transcript, and never
//! fails the clear: every problem leaves the session with no handoff.

use crate::common::payload::Payload;
use crate::common::proc::run_with_input;
use crate::handoff::compose;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

/// Set on the forked call so a hook it triggers never starts another one.
const CHILD_ENV: &str = "PLAYBOOK_HANDOFF_ON_CLEAR";

/// A handoff this session saved within this window counts as already done.
const RECENT_SAVE: Duration = Duration::from_secs(600);

/// Largest transcript worth resuming. Resuming re-reads the whole file, so a
/// larger one can outlast the hook and cost real money on a cold cache.
const MAX_TRANSCRIPT_BYTES: u64 = 8 * 1024 * 1024;

/// How long the forked call may run. The wired hook timeout is longer, so the
/// call ends first and the hook returns on its own.
const CALL_TIMEOUT: Duration = Duration::from_secs(50);

const MODEL: &str = "haiku";

const PROMPT: &str = "Write the session handoff for this conversation now. \
Reply with the markdown only: no preamble, no closing offer, no tool calls. \
Use exactly these sections in this order: a first line `# Session Handoff - <specific one-line topic>`, \
then `## Where we are now`, `## Decisions made`, `## Next steps`, and optionally `## Open questions`. \
Write facts and decisions for a reader who has not seen the conversation. \
Give every decision a reason and every next step a reason it is next. \
At a clean stopping point, Next steps is the single line `Clean stop. No pending work.` \
Use short SHAs, path:line and exact versions. No emojis, no em or en dashes.";

/// What one run did, for tests.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Saved(PathBuf),
    /// The session saved a handoff of its own a few minutes ago.
    AlreadySaved,
    TooLarge,
    NoTranscript,
    /// The resumed call failed, timed out, or returned a handoff that does not
    /// follow the template.
    NoHandoff,
}

/// Resumes the ended session and returns the model's reply.
trait Resumer {
    fn handoff_text(&self, session_id: &str) -> Option<String>;
}

struct ClaudeResumer;

impl Resumer for ClaudeResumer {
    fn handoff_text(&self, session_id: &str) -> Option<String> {
        let mut command = Command::new("claude");
        command
            .args([
                "-p",
                "--resume",
                session_id,
                "--fork-session",
                "--model",
                MODEL,
                "--tools",
                "",
                "--setting-sources",
                "",
                "--strict-mcp-config",
            ])
            .env(CHILD_ENV, "1");
        let out = run_with_input(&mut command, PROMPT.as_bytes(), CALL_TIMEOUT)?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

/// Whether `handoff.onClear` is on. Off when unset or unreadable.
fn enabled() -> bool {
    let slug = crate::common::repo::repo_slug();
    let slug = (!slug.is_empty()).then_some(slug.as_str());
    crate::config::resolve("handoff.onClear", &crate::common::home_dir(), slug)
        .ok()
        .and_then(|(value, _)| value.as_bool())
        .unwrap_or(false)
}

pub fn run(payload: &Payload) {
    if payload.field(".reason") != "clear" || std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let session_id = payload.field(".session_id");
    if session_id.is_empty() || !enabled() {
        return;
    }
    let slug = crate::handoff::current_slug();
    if slug.is_empty() {
        return;
    }
    let transcript = payload.field(".transcript_path");
    let _ = capture(
        &crate::handoff::root(),
        &slug,
        &session_id,
        Path::new(&transcript),
        SystemTime::now(),
        &ClaudeResumer,
    );
}

fn capture(
    dir: &Path,
    slug: &str,
    session_id: &str,
    transcript: &Path,
    now: SystemTime,
    resumer: &dyn Resumer,
) -> Outcome {
    let Some(size) = std::fs::metadata(transcript)
        .ok()
        .filter(|meta| meta.is_file())
        .map(|meta| meta.len())
    else {
        return Outcome::NoTranscript;
    };
    if size > MAX_TRANSCRIPT_BYTES {
        return Outcome::TooLarge;
    }
    let since = now
        .checked_sub(RECENT_SAVE)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    if crate::handoff::saved_since(dir, slug, session_id, since) {
        return Outcome::AlreadySaved;
    }
    let Some(text) = resumer
        .handoff_text(session_id)
        .filter(|text| compose::check(text).is_empty())
    else {
        return Outcome::NoHandoff;
    };
    match crate::handoff::save_for_session(dir, slug, &text, session_id) {
        Ok(path) => Outcome::Saved(path),
        Err(_) => Outcome::NoHandoff,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;
    use std::cell::Cell;
    use std::fs;

    const SESSION: &str = "c6e91d16-99c2-4305-b9c5-2abc835f7f83";
    const SLUG: &str = "-Users-me-repo_0a1b2c3d";
    const GOOD: &str = "# Session Handoff - topic\n\n## Where we are now\n- a\n\n## Decisions made\n- b. Why: c.\n\n## Next steps\n1. d. Why now: e.\n";

    struct Fake {
        reply: Option<String>,
        calls: Cell<u32>,
    }

    impl Fake {
        fn saying(text: &str) -> Self {
            Fake {
                reply: Some(text.to_string()),
                calls: Cell::new(0),
            }
        }
    }

    impl Resumer for Fake {
        fn handoff_text(&self, _: &str) -> Option<String> {
            self.calls.set(self.calls.get() + 1);
            self.reply.clone()
        }
    }

    fn transcript_of(dir: &Path, bytes: u64) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join("t.jsonl");
        let file = fs::File::create(&path).unwrap();
        file.set_len(bytes).unwrap();
        path
    }

    #[test]
    fn a_good_reply_is_saved_for_the_session_and_loads_after_the_clear() {
        let dir = scratch_dir("hoc-saved");
        let transcript = transcript_of(&dir, 10);
        let handoffs = dir.join("handoff");

        let outcome = capture(
            &handoffs,
            SLUG,
            SESSION,
            &transcript,
            SystemTime::now(),
            &Fake::saying(GOOD),
        );

        let Outcome::Saved(path) = outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(fs::read_to_string(&path).unwrap(), GOOD);
        assert!(crate::handoff::saved_since(
            &handoffs,
            SLUG,
            SESSION,
            SystemTime::UNIX_EPOCH
        ));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_session_that_saved_a_handoff_just_now_is_not_resumed() {
        let dir = scratch_dir("hoc-recent");
        let transcript = transcript_of(&dir, 10);
        let handoffs = dir.join("handoff");
        crate::handoff::save_for_session(&handoffs, SLUG, "# mine", SESSION).unwrap();
        let fake = Fake::saying(GOOD);

        let outcome = capture(
            &handoffs,
            SLUG,
            SESSION,
            &transcript,
            SystemTime::now(),
            &fake,
        );

        assert_eq!(outcome, Outcome::AlreadySaved);
        assert_eq!(fake.calls.get(), 0, "no model call, no cost");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_handoff_of_another_session_does_not_count() {
        let dir = scratch_dir("hoc-other");
        let transcript = transcript_of(&dir, 10);
        let handoffs = dir.join("handoff");
        crate::handoff::save_for_session(&handoffs, SLUG, "# theirs", "0a1b2c3d-0000").unwrap();

        let outcome = capture(
            &handoffs,
            SLUG,
            SESSION,
            &transcript,
            SystemTime::now(),
            &Fake::saying(GOOD),
        );

        assert!(matches!(outcome, Outcome::Saved(_)), "{outcome:?}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_very_large_transcript_is_skipped_without_a_model_call() {
        let dir = scratch_dir("hoc-large");
        let transcript = transcript_of(&dir, MAX_TRANSCRIPT_BYTES + 1);
        let fake = Fake::saying(GOOD);

        let outcome = capture(
            &dir.join("h"),
            SLUG,
            SESSION,
            &transcript,
            SystemTime::now(),
            &fake,
        );

        assert_eq!(outcome, Outcome::TooLarge);
        assert_eq!(fake.calls.get(), 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_transcript_is_skipped() {
        let dir = scratch_dir("hoc-missing");
        let fake = Fake::saying(GOOD);

        let outcome = capture(
            &dir.join("h"),
            SLUG,
            SESSION,
            &dir.join("none.jsonl"),
            SystemTime::now(),
            &fake,
        );

        assert_eq!(outcome, Outcome::NoTranscript);
        assert_eq!(fake.calls.get(), 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_call_or_a_reply_off_the_template_saves_nothing() {
        let dir = scratch_dir("hoc-bad");
        let transcript = transcript_of(&dir, 10);
        let handoffs = dir.join("handoff");
        let failed = Fake {
            reply: None,
            calls: Cell::new(0),
        };

        for fake in [
            failed,
            Fake::saying("Sure, here is a summary."),
            Fake::saying(""),
        ] {
            let outcome = capture(
                &handoffs,
                SLUG,
                SESSION,
                &transcript,
                SystemTime::now(),
                &fake,
            );

            assert_eq!(outcome, Outcome::NoHandoff);
        }
        assert!(!handoffs.exists() || fs::read_dir(&handoffs).unwrap().next().is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_reason_other_than_clear_does_nothing() {
        let payload = Payload::parse(&format!(
            r#"{{"session_id":"{SESSION}","reason":"logout","transcript_path":"/none"}}"#
        ));

        run(&payload);
    }
}
