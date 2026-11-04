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
/// Kept after the 7 day segment on a narrow terminal.
pub const PRIORITY_FIVE_HOUR: u8 = 2;

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
        .filter_map(|limit| {
            let priority = match limit.period {
                LimitPeriod::SevenDay => PRIORITY_SEVEN_DAY,
                LimitPeriod::FiveHour => PRIORITY_FIVE_HOUR,
                _ => return None,
            };
            let headroom = limit.headroom(now)?;
            let brief = brief(limit, &headroom, now)?;
            // A brief is only given for a cap that is hit or on pace to be.
            Some(Segment {
                text: format!("{ALARM_MARK}{}", brief.text),
                short: None,
                priority,
                alarm: true,
            })
        })
        .collect()
}
