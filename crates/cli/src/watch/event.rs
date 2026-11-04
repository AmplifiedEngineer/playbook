// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The facts about one GitHub event that rules and the queue need. Text stays
//! out: only metadata, a hash of the body, and the first characters of a
//! comment for an exact prefix match.

use super::rule::On;

/// How many characters of a comment body are kept for a prefix rule.
pub const COMMENT_HEAD_CHARS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// `<source>:<object id>:<kind>`, unique per event.
    pub key: String,
    /// `owner/name`.
    pub repo: String,
    pub kind: On,
    pub author: String,
    /// GitHub's `author_association`, for example `OWNER` or `NONE`.
    pub association: String,
    /// The issue or pull request number, or the comment id for a comment.
    pub object: u64,
    pub updated_at: i64,
    pub body_hash: [u8; 32],
    pub labels: Vec<String>,
    pub comment_head: String,
}
