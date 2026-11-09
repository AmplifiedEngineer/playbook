// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The update report as a terminal sees it. A pipe keeps the plain lines.

use super::Outcome;
use crate::ui::render::{self, Status};
use crate::ui::Caps;

fn status_of(line: &str) -> Status {
    if line.starts_with("updated") || line.starts_with("verified") {
        Status::Done
    } else if line.starts_with("update available") {
        Status::Warn
    } else if line.contains("is already installed")
        || line.contains("is up to date")
        || line.contains("nothing to do")
    {
        Status::Unchanged
    } else {
        Status::Info
    }
}

/// One marked line per output line, then each warning.
pub fn render(caps: &Caps, outcome: &Outcome) -> String {
    let mut out = String::new();
    for line in &outcome.out {
        out.push_str(&render::progress_line(caps, status_of(line), "", line));
        out.push('\n');
    }
    for w in &outcome.warnings {
        out.push_str(&render::progress_line(caps, Status::Warn, "", w));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_get_marks_and_warnings_follow() {
        let o = Outcome {
            out: vec![
                "verified the build attestation for a.tgz".into(),
                "updated playbook 1.0.0 -> 1.1.0 at /bin/playbook".into(),
                "kept the previous binary as /bin/playbook.bak".into(),
            ],
            warnings: vec!["another playbook comes first on PATH".into()],
            ..Outcome::default()
        };
        let got = render(&Caps::plain(80), &o);
        assert!(got.starts_with("[ok] verified"), "{got}");
        assert!(got.contains("[ok] updated playbook 1.0.0 -> 1.1.0"));
        assert!(got.contains("[info] kept the previous binary"));
        assert!(got.contains("[warn] another playbook"));
    }

    #[test]
    fn up_to_date_is_unchanged_and_available_is_a_warning() {
        assert_eq!(
            status_of("playbook 1 is up to date (latest is 1)"),
            Status::Unchanged
        );
        assert_eq!(
            status_of("update available: 1 -> 2 (run `playbook upgrade`)"),
            Status::Warn
        );
    }
}
