// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Where events come from. The engine sees only this trait, so a webhook
//! source can replace polling later without changing it.

use std::fmt;

use super::event::Event;
use super::store::Cursor;

/// The result of one poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Poll {
    /// Nothing new. Wait at least `next_in` seconds.
    Unchanged { next_in: i64 },
    /// New events are in the caller's buffer. Store `next` with them.
    Events { next: Cursor, next_in: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceError {
    /// No request before `retry_at` (epoch seconds).
    RateLimited { retry_at: i64 },
    /// The token was refused. Not retried.
    Unauthorized,
    /// Network or server trouble. The caller backs off.
    Transient(String),
    /// A reply that could not be read. The cursor must not move.
    Malformed(String),
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RateLimited { retry_at } => write!(f, "rate limited until {retry_at}"),
            Self::Unauthorized => f.write_str("GitHub refused the token"),
            Self::Transient(why) => write!(f, "GitHub request failed: {why}"),
            Self::Malformed(why) => write!(f, "GitHub reply could not be read: {why}"),
        }
    }
}

impl std::error::Error for SourceError {}

pub trait EventSource {
    fn name(&self) -> &str;

    /// Reads what is new since `cursor`. `out` is cleared first and belongs to
    /// the caller, so its capacity is reused across polls.
    fn poll(&mut self, cursor: &Cursor, out: &mut Vec<Event>) -> Result<Poll, SourceError>;
}
