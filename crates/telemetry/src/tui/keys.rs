// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Every key the view handles, in one table. The key handler looks keys up
//! here, the footer takes its hints from here and the help window is built
//! from here, so none of them can drift from the keys that really work.

use ratatui::crossterm::event::KeyCode;

/// What a key does. `App::run` has one arm for each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    Quit,
    Theme,
    Today,
    Week,
    LastWeek,
    Month,
    LastMonth,
    All,
    CycleRange,
    Filter,
    ClearFilter,
    FocusPanel,
    NextPanel,
    PrevPanel,
    NextTab,
    PrevTab,
    GoTab,
    CursorLeft,
    CursorRight,
    First,
    Last,
    Down,
    Up,
    PageDown,
    PageUp,
    NextView,
    Sort,
    SortFlip,
    Pick,
    Sessions,
    EditLayout,
    Help,
    Refresh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Navigation,
    Periods,
    Views,
    Sorting,
    Filtering,
    Refresh,
    Quit,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Group::Navigation => "Navigation",
            Group::Periods => "Periods",
            Group::Views => "Views",
            Group::Sorting => "Sorting",
            Group::Filtering => "Filtering",
            Group::Refresh => "Refresh",
            Group::Quit => "Quit",
        }
    }
}

/// A short hint for the footer. `order` places it from the left. `drop` is
/// the order hints are dropped in on a narrow screen, 1 first; 0 is never
/// dropped.
#[derive(Debug, Clone, Copy)]
pub struct Hint {
    pub key: &'static str,
    pub verb: &'static str,
    pub order: u8,
    pub drop: u8,
}

pub struct Binding {
    pub cmd: Cmd,
    pub keys: &'static [KeyCode],
    /// With Ctrl held.
    pub ctrl: bool,
    /// How the keys read in the help window.
    pub shown: &'static str,
    pub group: Group,
    /// Which panels the key acts on.
    pub panels: &'static str,
    pub text: &'static str,
    pub hint: Option<Hint>,
}

const fn c(ch: char) -> KeyCode {
    KeyCode::Char(ch)
}

const fn hint(key: &'static str, verb: &'static str, order: u8, drop: u8) -> Option<Hint> {
    Some(Hint {
        key,
        verb,
        order,
        drop,
    })
}

const ALL_PANELS: &str = "all panels";

pub const BINDINGS: &[Binding] = &[
    Binding {
        cmd: Cmd::FocusPanel,
        keys: &[c('1'), c('2'), c('3'), c('4'), c('5'), c('6')],
        ctrl: false,
        shown: "1 to 6",
        group: Group::Navigation,
        panels: ALL_PANELS,
        text: "Focus a panel by its number",
        hint: hint("1-6", "panel", 1, 5),
    },
    Binding {
        cmd: Cmd::NextTab,
        keys: &[KeyCode::Tab],
        ctrl: false,
        shown: "Tab",
        group: Group::Navigation,
        panels: ALL_PANELS,
        text: "Show the next tab",
        hint: hint("Tab", "tab", 0, 0),
    },
    Binding {
        cmd: Cmd::PrevTab,
        keys: &[KeyCode::BackTab],
        ctrl: false,
        shown: "Shift-Tab",
        group: Group::Navigation,
        panels: ALL_PANELS,
        text: "Show the previous tab",
        hint: None,
    },
    Binding {
        cmd: Cmd::GoTab,
        keys: &[
            KeyCode::F(1),
            KeyCode::F(2),
            KeyCode::F(3),
            KeyCode::F(4),
            KeyCode::F(5),
            KeyCode::F(6),
            KeyCode::F(7),
            KeyCode::F(8),
            KeyCode::F(9),
            KeyCode::F(10),
            KeyCode::F(11),
            KeyCode::F(12),
        ],
        ctrl: false,
        shown: "F1 to F12",
        group: Group::Navigation,
        panels: ALL_PANELS,
        text: "Show a tab by its number",
        hint: None,
    },
    Binding {
        cmd: Cmd::NextPanel,
        keys: &[c(']')],
        ctrl: false,
        shown: "]",
        group: Group::Navigation,
        panels: ALL_PANELS,
        text: "Focus the next panel of the tab",
        hint: None,
    },
    Binding {
        cmd: Cmd::PrevPanel,
        keys: &[c('[')],
        ctrl: false,
        shown: "[",
        group: Group::Navigation,
        panels: ALL_PANELS,
        text: "Focus the previous panel of the tab",
        hint: None,
    },
    Binding {
        cmd: Cmd::Down,
        keys: &[KeyCode::Down, c('j')],
        ctrl: false,
        shown: "Down, j",
        group: Group::Navigation,
        panels: "tables",
        text: "Select the next row",
        hint: None,
    },
    Binding {
        cmd: Cmd::Up,
        keys: &[KeyCode::Up, c('k')],
        ctrl: false,
        shown: "Up, k",
        group: Group::Navigation,
        panels: "tables",
        text: "Select the previous row",
        hint: None,
    },
    Binding {
        cmd: Cmd::PageDown,
        keys: &[KeyCode::PageDown],
        ctrl: false,
        shown: "PgDn",
        group: Group::Navigation,
        panels: "tables",
        text: "Move five rows down",
        hint: None,
    },
    Binding {
        cmd: Cmd::PageUp,
        keys: &[KeyCode::PageUp],
        ctrl: false,
        shown: "PgUp",
        group: Group::Navigation,
        panels: "tables",
        text: "Move five rows up",
        hint: None,
    },
    Binding {
        cmd: Cmd::First,
        keys: &[KeyCode::Home, c('g')],
        ctrl: false,
        shown: "Home, g",
        group: Group::Navigation,
        panels: "tables, charts",
        text: "First row, or first day of a chart",
        hint: None,
    },
    Binding {
        cmd: Cmd::Last,
        keys: &[KeyCode::End, c('G')],
        ctrl: false,
        shown: "End, G",
        group: Group::Navigation,
        panels: "tables, charts",
        text: "Last row, or last day of a chart",
        hint: None,
    },
    Binding {
        cmd: Cmd::CursorLeft,
        keys: &[KeyCode::Left],
        ctrl: false,
        shown: "Left",
        group: Group::Navigation,
        panels: "charts",
        text: "Move the chart cursor one bar back",
        hint: hint("\u{2190}/\u{2192}", "day", 6, 8),
    },
    Binding {
        cmd: Cmd::CursorRight,
        keys: &[KeyCode::Right],
        ctrl: false,
        shown: "Right",
        group: Group::Navigation,
        panels: "charts",
        text: "Move the chart cursor one bar forward",
        hint: None,
    },
    Binding {
        cmd: Cmd::Today,
        keys: &[c('d')],
        ctrl: false,
        shown: "d",
        group: Group::Periods,
        panels: ALL_PANELS,
        text: "Show today",
        hint: hint("d", "day", 2, 7),
    },
    Binding {
        cmd: Cmd::Week,
        keys: &[c('w')],
        ctrl: false,
        shown: "w",
        group: Group::Periods,
        panels: ALL_PANELS,
        text: "Show this week",
        hint: hint("w", "week", 3, 10),
    },
    Binding {
        cmd: Cmd::LastWeek,
        keys: &[c('W')],
        ctrl: false,
        shown: "W",
        group: Group::Periods,
        panels: ALL_PANELS,
        text: "Show last week",
        hint: None,
    },
    Binding {
        cmd: Cmd::Month,
        keys: &[c('m')],
        ctrl: false,
        shown: "m",
        group: Group::Periods,
        panels: ALL_PANELS,
        text: "Show this month",
        hint: hint("m", "month", 4, 11),
    },
    Binding {
        cmd: Cmd::LastMonth,
        keys: &[c('M')],
        ctrl: false,
        shown: "M",
        group: Group::Periods,
        panels: ALL_PANELS,
        text: "Show last month",
        hint: None,
    },
    Binding {
        cmd: Cmd::All,
        keys: &[c('a')],
        ctrl: false,
        shown: "a",
        group: Group::Periods,
        panels: ALL_PANELS,
        text: "Show all time",
        hint: hint("a", "all", 5, 6),
    },
    Binding {
        cmd: Cmd::CycleRange,
        keys: &[c('r')],
        ctrl: false,
        shown: "r",
        group: Group::Periods,
        panels: ALL_PANELS,
        text: "Switch between today and this week",
        hint: hint("r", "range", 7, 9),
    },
    Binding {
        cmd: Cmd::NextView,
        keys: &[c('v')],
        ctrl: false,
        shown: "v",
        group: Group::Views,
        panels: "Spend, Projects",
        text: "Next view of the focused panel",
        hint: hint("v", "view", 8, 0),
    },
    Binding {
        cmd: Cmd::Theme,
        keys: &[c('t')],
        ctrl: false,
        shown: "t",
        group: Group::Views,
        panels: ALL_PANELS,
        text: "Next color theme (off with NO_COLOR)",
        hint: hint("t", "theme", 12, 2),
    },
    Binding {
        cmd: Cmd::Sessions,
        keys: &[c('p')],
        ctrl: false,
        shown: "p",
        group: Group::Views,
        panels: ALL_PANELS,
        text: "Open the full screen Sessions list",
        hint: hint("p", "sessions", 11, 1),
    },
    Binding {
        cmd: Cmd::EditLayout,
        keys: &[c('e')],
        ctrl: false,
        shown: "e",
        group: Group::Views,
        panels: ALL_PANELS,
        text: "Edit the tabs and the panels on each",
        hint: None,
    },
    Binding {
        cmd: Cmd::Help,
        keys: &[c('h'), c('?')],
        ctrl: false,
        shown: "h, ?",
        group: Group::Views,
        panels: ALL_PANELS,
        text: "Show this help",
        hint: hint("h", "help", 13, 0),
    },
    Binding {
        cmd: Cmd::Sort,
        keys: &[c('s')],
        ctrl: false,
        shown: "s",
        group: Group::Sorting,
        panels: "tables",
        text: "Sort by the next column",
        hint: hint("s/S", "sort", 10, 4),
    },
    Binding {
        cmd: Cmd::SortFlip,
        keys: &[c('S')],
        ctrl: false,
        shown: "S",
        group: Group::Sorting,
        panels: "tables",
        text: "Reverse the sort order",
        hint: None,
    },
    Binding {
        cmd: Cmd::Filter,
        keys: &[c('/')],
        ctrl: false,
        shown: "/",
        group: Group::Filtering,
        panels: ALL_PANELS,
        text: "Type a filter on any field",
        hint: hint("/", "filter", 9, 0),
    },
    Binding {
        cmd: Cmd::Pick,
        keys: &[KeyCode::Enter],
        ctrl: false,
        shown: "Enter",
        group: Group::Filtering,
        panels: "tables 3-6",
        text: "Filter to selected row; again to undo",
        hint: hint("enter", "select", 14, 3),
    },
    Binding {
        cmd: Cmd::ClearFilter,
        keys: &[c('c')],
        ctrl: false,
        shown: "c",
        group: Group::Filtering,
        panels: ALL_PANELS,
        text: "Clear the filter",
        hint: None,
    },
    Binding {
        cmd: Cmd::Refresh,
        keys: &[c('R')],
        ctrl: false,
        shown: "R",
        group: Group::Refresh,
        panels: ALL_PANELS,
        text: "Read new usage now (also automatic)",
        hint: None,
    },
    Binding {
        cmd: Cmd::Quit,
        keys: &[c('q'), KeyCode::Esc],
        ctrl: false,
        shown: "q, Esc",
        group: Group::Quit,
        panels: ALL_PANELS,
        text: "Quit",
        hint: hint("q", "quit", 15, 0),
    },
    Binding {
        cmd: Cmd::Quit,
        keys: &[c('c')],
        ctrl: true,
        shown: "Ctrl-C",
        group: Group::Quit,
        panels: ALL_PANELS,
        text: "Quit, also while typing a filter",
        hint: None,
    },
];

/// The command a key runs. A Ctrl key with no Ctrl binding falls back to the
/// plain binding, as the handler always has.
pub fn lookup(code: KeyCode, ctrl: bool) -> Option<&'static Binding> {
    let find = |ctrl: bool| {
        BINDINGS
            .iter()
            .find(|b| b.ctrl == ctrl && b.keys.contains(&code))
    };
    find(ctrl).or_else(|| if ctrl { find(false) } else { None })
}

/// One line of the help window.
#[derive(Debug, PartialEq, Eq)]
pub enum HelpLine {
    Heading(&'static str),
    Key {
        shown: &'static str,
        text: &'static str,
        panels: &'static str,
    },
    Blank,
}

const GROUPS: [Group; 7] = [
    Group::Navigation,
    Group::Periods,
    Group::Views,
    Group::Sorting,
    Group::Filtering,
    Group::Refresh,
    Group::Quit,
];

/// The help window's lines, grouped, built from `BINDINGS`, then the marks
/// the tables use (no key triggers a mark).
pub fn help_lines() -> Vec<HelpLine> {
    let mut out = Vec::new();
    for group in GROUPS {
        if !out.is_empty() {
            out.push(HelpLine::Blank);
        }
        out.push(HelpLine::Heading(group.title()));
        for b in BINDINGS.iter().filter(|b| b.group == group) {
            out.push(HelpLine::Key {
                shown: b.shown,
                text: b.text,
                panels: b.panels,
            });
        }
    }
    out.push(HelpLine::Blank);
    out.push(HelpLine::Heading("Marks"));
    out.push(HelpLine::Key {
        shown: "~",
        text: "Tools: count only, from transcripts",
        panels: "",
    });
    out
}

/// The footer hints in the order they are shown.
pub fn footer_hints() -> Vec<Hint> {
    let mut hints: Vec<Hint> = BINDINGS.iter().filter_map(|b| b.hint).collect();
    hints.sort_by_key(|h| h.order);
    hints
}
