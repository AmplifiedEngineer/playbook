// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The segments of line 4: where a usage cap stands against its pace. Each
//! segment is plain text; colour and fitting to the terminal width are added
//! by [`super::lines::line_4`]. The words and the leading `!` carry the
//! meaning, so nothing relies on colour alone.

use crate::usage::limits::{brief, fmt_span, LimitPeriod, State};
use crate::usage::limits_source::{manual_budgets, Snapshot};
use crate::usage::monthly_use::{detect, monthly_use, MonthlyUse, Source};
use crate::usage::plan::{self, Account};
use std::path::Path;

/// Placed before the words of a segment that is an alarm.
pub const ALARM_MARK: &str = "! ";

/// Segments with a lower number are kept longer on a narrow terminal.
pub const PRIORITY_SEVEN_DAY: u8 = 0;
/// Kept after the 7 day segment on a narrow terminal.
pub const PRIORITY_FIVE_HOUR: u8 = 2;

/// Kept after the 7 day segment and before the 5 hour one.
pub const PRIORITY_MONTHLY: u8 = 1;

/// The longest Claude config file the statusline reads. A larger file is
/// skipped, so the line stays cheap whatever Claude Code keeps in there.
pub const MAX_CONFIG_BYTES: u64 = 512 * 1024;

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

/// The segments for the statusline input at `now`. `claude_json` is Claude
/// Code's config file, read only when stdin carries no monthly dollar figure,
/// and `home` is where the manual budget is kept. Empty when nothing is worth
/// saying, which is the usual case.
pub fn build(input: &str, now: i64, claude_json: &Path, home: &Path) -> Vec<Segment> {
    let snapshot = Snapshot::from_statusline(input, now);
    let mut out = snapshot.as_ref().map_or_else(Vec::new, |s| windows(s, now));
    out.extend(monthly(snapshot.as_ref(), now, claude_json, home));
    out
}

/// The monthly dollar segment: the amount Claude reports (stdin, else the
/// config file cache) against the lower of its limit and the manual budget.
fn monthly(
    snapshot: Option<&Snapshot>,
    now: i64,
    claude_json: &Path,
    home: &Path,
) -> Option<Segment> {
    let mut account = Account::default();
    if detect(snapshot, &account, now).is_none() {
        account = plan::read_limited(claude_json, MAX_CONFIG_BYTES)?;
    }
    let figure = monthly_use(snapshot, &account, &manual_budgets(home), now)?;
    monthly_segment(&figure, now)
}

/// The words for a monthly figure. A stale cache projects nothing: it only
/// says how old it is, and that the cap was hit if the amount already passed it.
fn monthly_segment(m: &MonthlyUse, now: i64) -> Option<Segment> {
    // A cache holds the amount at its fetch time, so the pace is worked out
    // there: hours since the fetch must not dilute it.
    let seen = match m.source {
        Source::Cache { age, .. } => now - age,
        Source::Statusline => now,
    };
    let headroom = m.limit.headroom(seen)?;
    let label = if m.own_budget { "mo budget" } else { "mo" };
    let figure = format!("${:.0}/${:.0} {label}", m.limit.used, m.limit.limit);
    let exceeded = headroom.state == State::Exceeded;
    let (words, alarm, short_ok) = match (m.source, headroom.state) {
        (Source::Cache { age, fresh: false }, _) => {
            let old = format!("({} old)", fmt_span(age));
            let words = if exceeded {
                format!(" CAP HIT {old}")
            } else {
                format!(" {old}")
            };
            (words, exceeded, false)
        }
        (_, State::Exceeded) => (" CAP HIT".to_string(), true, true),
        (_, State::WillExceed { at }) => (
            format!(", cap in {}", fmt_span((at - now).max(0))),
            true,
            true,
        ),
        (_, State::Under) => (", on pace".to_string(), false, true),
        (_, State::OnTrack) => (", near cap".to_string(), false, true),
        (_, State::Unknown) => (String::new(), false, true),
    };
    let mark = if alarm { ALARM_MARK } else { "" };
    let short = (short_ok && !words.is_empty()).then(|| format!("{mark}{figure}"));
    Some(Segment {
        text: format!("{mark}{figure}{words}"),
        short,
        priority: PRIORITY_MONTHLY,
        alarm,
    })
}

/// The 7 day and 5 hour segments of the statusline reading.
fn windows(snapshot: &Snapshot, now: i64) -> Vec<Segment> {
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
