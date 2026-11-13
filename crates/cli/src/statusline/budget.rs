// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The segments of line 4: where a usage cap stands against its pace. Each
//! segment is plain text; colour and fitting to the terminal width are added
//! by [`super::lines::line_4`]. The words and the leading `!` carry the
//! meaning, so nothing relies on colour alone.

use crate::usage::data::usage_db;
use crate::usage::limits::{brief, fmt_span, LimitPeriod, State};
use crate::usage::limits_source::{manual_budgets, Snapshot};
use crate::usage::local_total::LocalTotal;
use crate::usage::monthly_use::{detect, monthly_use, MonthlyUse, Source};
use crate::usage::plan::{self, Account};
use crate::usage::tui::fmt::{money, money_whole};
use std::path::Path;

/// The file under the statusline cache folder that holds the month's local
/// total between renders.
const LOCAL_TOTAL_FILE: &str = "monthly-local-total";

/// Placed before the words of a segment that is an alarm.
pub const ALARM_MARK: &str = "! ";

/// Segments with a lower number are kept longer on a narrow terminal.
pub const PRIORITY_SEVEN_DAY: u8 = 0;
/// Kept after the 7 day segment on a narrow terminal.
pub const PRIORITY_FIVE_HOUR: u8 = 2;

/// Kept after the 7 day segment and before the 5 hour one.
pub const PRIORITY_MONTHLY: u8 = 1;

/// This chat's cost goes first on a narrow terminal: line 3 already shows it.
pub const PRIORITY_CHAT: u8 = 3;

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

/// The segments for the statusline reading at `now`, in the order they show:
/// the 7 day window, this chat's cost, the monthly dollars, the 5 hour window.
/// `chat_usd` is this chat's cost, shown only beside an account figure that
/// Claude reports. `claude_json` is Claude Code's config file, read only when
/// stdin carries no monthly dollar figure, and `home` is where the manual
/// budget is kept. Empty when nothing is worth saying, which is the usual case.
pub fn build(
    snapshot: Option<&Snapshot>,
    chat_usd: f64,
    now: i64,
    claude_json: &Path,
    home: &Path,
    cache_dir: &Path,
) -> Vec<Segment> {
    let window = |period| snapshot.and_then(|s| window(s, period, now));
    let monthly = monthly(snapshot, now, claude_json, home, cache_dir);
    let figure = monthly.as_ref().and_then(|m| monthly_segment(m, now));
    let by_claude = monthly.is_some_and(|m| m.source != Source::Local);
    let chat = figure
        .as_ref()
        .filter(|_| by_claude)
        .and_then(|_| chat(chat_usd));
    [
        window(LimitPeriod::SevenDay),
        chat,
        figure,
        window(LimitPeriod::FiveHour),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// `chat $1.20`, this chat's cost, or `None` when it has cost nothing yet.
fn chat(usd: f64) -> Option<Segment> {
    (usd.is_finite() && usd > 0.0).then(|| Segment {
        text: format!("chat {}", money(usd)),
        short: None,
        priority: PRIORITY_CHAT,
        alarm: false,
    })
}

/// The monthly dollar figure: the amount Claude reports (stdin, else the
/// config file cache) against the lower of its limit and the manual budget,
/// or with only a manual budget, this machine's cost for the month.
fn monthly(
    snapshot: Option<&Snapshot>,
    now: i64,
    claude_json: &Path,
    home: &Path,
    cache_dir: &Path,
) -> Option<MonthlyUse> {
    let mut account = Account::default();
    if detect(snapshot, &account, now).is_none() {
        account = plan::read_limited(claude_json, MAX_CONFIG_BYTES).unwrap_or_default();
    }
    let local = LocalTotal {
        db: usage_db(home),
        cache: Some(cache_dir.join(LOCAL_TOTAL_FILE)),
    };
    monthly_use(snapshot, &account, &manual_budgets(home), now, || {
        local.month_used(now)
    })
}

/// The words for a monthly figure. A stale cache projects nothing: it only
/// says how old it is, and that the cap was hit if the amount already passed it.
fn monthly_segment(m: &MonthlyUse, now: i64) -> Option<Segment> {
    // A cache holds the amount at its fetch time, so the pace is worked out
    // there: hours since the fetch must not dilute it.
    let seen = match m.source {
        Source::Cache { age, .. } => now - age,
        Source::Statusline | Source::Local => now,
    };
    let headroom = m.limit.headroom(seen)?;
    let label = match (m.source, m.own_budget) {
        (Source::Local, _) => "mo budget (local)",
        (_, true) => "mo budget",
        (_, false) => "mo",
    };
    let figure = match m.source {
        Source::Local => format!("${:.0}/${:.0} {label}", m.limit.used, m.limit.limit),
        Source::Statusline | Source::Cache { .. } => format!(
            "acct {} / {} {label}",
            money_whole(m.limit.used),
            money_whole(m.limit.limit)
        ),
    };
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

/// The 7 day or 5 hour segment of the statusline reading, when that window is
/// reached or on pace to be.
fn window(snapshot: &Snapshot, period: LimitPeriod, now: i64) -> Option<Segment> {
    let priority = match period {
        LimitPeriod::SevenDay => PRIORITY_SEVEN_DAY,
        LimitPeriod::FiveHour => PRIORITY_FIVE_HOUR,
        _ => return None,
    };
    let limit = snapshot
        .limits(now)
        .into_iter()
        .find(|l| l.period == period)?;
    let headroom = limit.headroom(now)?;
    let brief = brief(&limit, &headroom, now)?;
    // A brief is only given for a cap that is hit or on pace to be.
    Some(Segment {
        text: format!("{ALARM_MARK}{}", brief.text),
        short: None,
        priority,
        alarm: true,
    })
}
