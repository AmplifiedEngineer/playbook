// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The segments of line 4: where a usage cap stands against its pace. Each
//! segment is plain text; colour and fitting to the terminal width are added
//! by [`super::lines::line_4`]. The words and the leading `!` carry the
//! meaning, so nothing relies on colour alone.

use crate::usage::limits::{brief, LimitPeriod};
use crate::usage::limits_source::Snapshot;

/// Placed before the words of a segment that is an alarm.
pub const ALARM_MARK: &str = "! ";

/// Segments with a lower number are kept longer on a narrow terminal.
pub const PRIORITY_SEVEN_DAY: u8 = 0;

/// One thing line 4 says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// The full plain text.
    pub text: String,
    /// A shorter plain text for a narrow terminal, when one still makes sense.
    pub short: Option<String>,
    /// Lower is more important.
    pub priority: u8,
    /// Shown red; the text already starts with [`ALARM_MARK`].
    pub alarm: bool,
}

/// The segments for the statusline input at `now`. Empty when nothing is worth
/// saying, which is the usual case.
pub fn build(input: &str, now: i64) -> Vec<Segment> {
    let Some(snapshot) = Snapshot::from_statusline(input, now) else {
        return Vec::new();
    };
    snapshot
        .limits(now)
        .iter()
        .filter(|limit| limit.period == LimitPeriod::SevenDay)
        .filter_map(|limit| {
            let headroom = limit.headroom(now)?;
            let brief = brief(limit, &headroom, now)?;
            let mark = if brief.alarm { ALARM_MARK } else { "" };
            Some(Segment {
                text: format!("{mark}{}", brief.text),
                short: None,
                priority: PRIORITY_SEVEN_DAY,
                alarm: brief.alarm,
            })
        })
        .collect()
}
