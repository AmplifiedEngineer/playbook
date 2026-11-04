// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! What an action's failure means for the item: skipped, sent back to a
//! proposal, rolled back, retried or dead. Also the diff caps a code writing
//! action is held to.

use super::rule::Limits;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    Files,
    Lines,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionError {
    /// The world changed: the issue closed, or the body changed.
    Precondition(String),
    /// A diff cap was passed.
    Limit(LimitKind),
    /// The kill switch tripped.
    Killed,
    /// This platform cannot run the action.
    Unsupported,
    Failed {
        retryable: bool,
        why: String,
    },
}

/// What happens to the item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    /// Done, with outcome `skipped` and this reason. Never retried.
    Skipped(String),
    /// Back to a proposal, with this reason.
    Proposed(&'static str),
    /// The journal is rolled back and the worktree removed.
    RolledBack,
    /// Another try after a backoff.
    Retry,
    /// On the dead list at once, with this reason.
    Dead(String),
}

pub fn disposition(error: &ActionError) -> Disposition {
    match error {
        ActionError::Precondition(why) => Disposition::Skipped(why.clone()),
        ActionError::Limit(_) => Disposition::Proposed("limit"),
        ActionError::Killed => Disposition::RolledBack,
        ActionError::Unsupported => Disposition::Proposed("unsupported"),
        ActionError::Failed {
            retryable: true, ..
        } => Disposition::Retry,
        ActionError::Failed {
            retryable: false,
            why,
        } => Disposition::Dead(why.clone()),
    }
}

/// The most files and lines a change may touch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffCaps {
    pub files: i64,
    pub lines: i64,
}

/// A rule may lower `fix.maxFiles` and `fix.maxLines` and never raise them.
pub fn diff_caps(limits: &Limits, fix_max_files: i64, fix_max_lines: i64) -> DiffCaps {
    DiffCaps {
        files: limits.files.map_or(fix_max_files, |n| n.min(fix_max_files)),
        lines: limits.lines.map_or(fix_max_lines, |n| n.min(fix_max_lines)),
    }
}

/// Whether a change of this size stays inside the caps.
pub fn check_diff(files: i64, lines: i64, caps: &DiffCaps) -> Result<(), ActionError> {
    if files > caps.files {
        Err(ActionError::Limit(LimitKind::Files))
    } else if lines > caps.lines {
        Err(ActionError::Limit(LimitKind::Lines))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_error_has_one_disposition() {
        let rows = [
            (
                ActionError::Precondition("closed".into()),
                Disposition::Skipped("closed".into()),
            ),
            (
                ActionError::Limit(LimitKind::Files),
                Disposition::Proposed("limit"),
            ),
            (ActionError::Killed, Disposition::RolledBack),
            (
                ActionError::Unsupported,
                Disposition::Proposed("unsupported"),
            ),
            (
                ActionError::Failed {
                    retryable: true,
                    why: "502".into(),
                },
                Disposition::Retry,
            ),
            (
                ActionError::Failed {
                    retryable: false,
                    why: "no such branch".into(),
                },
                Disposition::Dead("no such branch".into()),
            ),
        ];
        for (error, want) in rows {
            assert_eq!(disposition(&error), want, "{error:?}");
        }
    }

    fn limits(files: Option<i64>, lines: Option<i64>) -> Limits {
        Limits {
            files,
            lines,
            ..Limits::default()
        }
    }

    #[test]
    fn a_rule_can_lower_the_caps_and_never_raise_them() {
        assert_eq!(
            diff_caps(&limits(Some(3), Some(100)), 10, 500),
            DiffCaps {
                files: 3,
                lines: 100
            }
        );
        assert_eq!(
            diff_caps(&limits(Some(99), Some(9_999)), 10, 500),
            DiffCaps {
                files: 10,
                lines: 500
            }
        );
    }

    #[test]
    fn a_rule_with_no_entry_gets_the_fix_defaults() {
        let caps = diff_caps(&limits(None, None), 10, 500);
        assert_eq!(
            caps,
            DiffCaps {
                files: 10,
                lines: 500
            }
        );
        assert!(check_diff(10, 500, &caps).is_ok());
        assert_eq!(
            check_diff(11, 1, &caps),
            Err(ActionError::Limit(LimitKind::Files))
        );
    }

    #[test]
    fn the_caps_allow_the_limit_minus_one_and_the_limit_and_stop_one_past() {
        // (files, lines, ok): the files row, then the lines row.
        let caps = DiffCaps {
            files: 4,
            lines: 40,
        };
        let rows = [
            (3, 1, true),
            (4, 1, true),
            (5, 1, false),
            (1, 39, true),
            (1, 40, true),
            (1, 41, false),
        ];
        for (files, lines, ok) in rows {
            assert_eq!(
                check_diff(files, lines, &caps).is_ok(),
                ok,
                "{files} files {lines} lines"
            );
        }
    }
}
