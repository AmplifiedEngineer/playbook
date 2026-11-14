// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The Sessions management view (ADR-0024, stage 2): a full screen table of
//! recent sessions over the same records as `playbook sessions`. It is read
//! only. A suggestion is shown for the selected session and nothing is ever
//! done to a session, a worktree or a pane from here.
//!
//! The view is kept apart from `App` so the usage panels stay untouched: the
//! event loop owns an `Option<SessionsView>`, routes keys to it while it is
//! open, and draws it instead of the panels.

use super::fmt::fit;
use super::theme::Palette;
use crate::query::{clock, short_id};
use crate::session_view::{age, link_label, tokens, worktree_label, Record, State};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Paragraph, Row, Table, TableState};
use ratatui::Frame;

/// Seconds between re-reads while the view is open. Each read asks `gh`, so
/// it is slower than the usage refresh.
pub const REFRESH_SECONDS: u64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// Newest activity first.
    Activity,
    /// Grouped by repo, newest first inside a repo.
    Repo,
    /// Needs attention first, then done, stale, idle, working.
    State,
}

impl Order {
    fn next(self) -> Order {
        match self {
            Order::Activity => Order::Repo,
            Order::Repo => Order::State,
            Order::State => Order::Activity,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Order::Activity => "activity",
            Order::Repo => "repo",
            Order::State => "state",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    /// Go back to the usage panels.
    Close,
    Quit,
    /// Read the sessions again now.
    Refresh,
}

#[derive(Debug)]
pub struct SessionsView {
    records: Vec<Record>,
    pub order: Order,
    pub selected: usize,
    /// When the records were read, epoch seconds. `None` before the first read.
    pub read_at: Option<i64>,
    pub error: Option<String>,
}

impl Default for SessionsView {
    fn default() -> Self {
        SessionsView {
            records: Vec::new(),
            order: Order::Activity,
            selected: 0,
            read_at: None,
            error: None,
        }
    }
}

fn state_rank(r: &Record) -> u8 {
    if r.needs_attention {
        return 0;
    }
    match r.state {
        // `Crashed` always needs attention, so it leaves through the early
        // return above; its rank here only completes the match.
        State::Crashed => 1,
        State::Done => 2,
        State::Waiting => 3,
        State::Stale => 4,
        State::Idle => 5,
        State::Working => 6,
        State::Closed => 7,
    }
}

impl SessionsView {
    /// Swaps in a fresh read. The selection follows the session it was on, and
    /// stays on the same row number when that session is gone.
    pub fn refresh(&mut self, records: Vec<Record>, now: i64) {
        let keep = self.current().map(|r| r.id.clone());
        self.records = records;
        self.read_at = Some(now);
        self.error = None;
        self.sort();
        if let Some(id) = keep {
            if let Some(at) = self.records.iter().position(|r| r.id == id) {
                self.selected = at;
            }
        }
        self.selected = self.selected.min(self.records.len().saturating_sub(1));
    }

    pub fn fail(&mut self, message: String) {
        self.error = Some(message);
    }

    pub fn records(&self) -> &[Record] {
        &self.records
    }

    fn current(&self) -> Option<&Record> {
        self.records.get(self.selected)
    }

    fn sort(&mut self) {
        match self.order {
            Order::Activity => self
                .records
                .sort_by(|a, b| b.last_activity.cmp(&a.last_activity).then(a.id.cmp(&b.id))),
            Order::Repo => self.records.sort_by(|a, b| {
                a.repo
                    .cmp(&b.repo)
                    .then(b.last_activity.cmp(&a.last_activity))
                    .then(a.id.cmp(&b.id))
            }),
            Order::State => self.records.sort_by(|a, b| {
                state_rank(a)
                    .cmp(&state_rank(b))
                    .then(b.last_activity.cmp(&a.last_activity))
                    .then(a.id.cmp(&b.id))
            }),
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> Action {
        let last = self.records.len().saturating_sub(1);
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Action::Quit,
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Esc | KeyCode::Char('p') => Action::Close,
            KeyCode::Char('r') => Action::Refresh,
            KeyCode::Char('v') | KeyCode::Char('s') => {
                let keep = self.current().map(|r| r.id.clone());
                self.order = self.order.next();
                self.sort();
                if let Some(at) = keep.and_then(|id| self.records.iter().position(|r| r.id == id)) {
                    self.selected = at;
                }
                Action::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(last);
                Action::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                Action::None
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.selected = 0;
                Action::None
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.selected = last;
                Action::None
            }
            _ => Action::None,
        }
    }
}

fn state_ink(pal: &Palette, r: &Record) -> ratatui::style::Color {
    if r.needs_attention {
        return pal.color(pal.error);
    }
    match r.state {
        State::Working => pal.color(pal.live),
        State::Idle | State::Closed => pal.color(pal.fg),
        State::Waiting | State::Stale | State::Done => pal.color(pal.warn),
        State::Crashed => pal.color(pal.error),
    }
}

/// Draws the whole screen: a table, the suggestion for the selected session,
/// and the footer with the keys.
pub fn render(
    frame: &mut Frame,
    view: &SessionsView,
    pal: &Palette,
    now: i64,
    icon: super::icon::Icon,
) {
    let area = frame.area();
    let base = Style::default().fg(pal.color(pal.fg)).bg(pal.color(pal.bg));
    frame.buffer_mut().set_style(area, base);
    let [body, hint, footer] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(2),
        Constraint::Length(1),
    ])
    .areas(area);
    let plain = Style::default().fg(pal.color(pal.fg));
    let box_ink = Style::default().fg(pal.color(pal.box_proc));
    let title = Line::from(vec![
        Span::styled("\u{256d}\u{2500}\u{2524}", box_ink),
        Span::styled(icon.prefix(), box_ink),
        Span::styled(
            "Sessions",
            Style::default()
                .fg(pal.color(pal.title))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("\u{251c}", box_ink),
    ]);
    let note = Line::styled(
        format!("order: {}  {}", view.order.label(), crate::tz::short(now)),
        plain,
    )
    .right_aligned();
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(box_ink)
        .title(title)
        .title(note);
    let inner_width = usize::from(body.width.saturating_sub(2));
    if view.records.is_empty() {
        let text = match (&view.error, view.read_at) {
            (Some(e), _) => format!("Could not read sessions: {e}"),
            (None, None) => "Reading sessions...".to_string(),
            (None, Some(_)) => "No recent sessions.".to_string(),
        };
        frame.render_widget(Paragraph::new(text).block(block), body);
    } else {
        let wide = inner_width >= 100;
        let repo_w = if wide { 16 } else { 12 };
        let branch_w = if wide { 28 } else { 18 };
        let wt_w = if wide { 22 } else { 14 };
        let link_w = 18;
        let head = [
            "state", "session", "repo", "branch", "worktree", "link", "last", "ctx", "cost",
        ];
        let header = Row::new(head.map(|h| {
            Cell::from(h).style(
                Style::default()
                    .fg(pal.color(pal.title))
                    .add_modifier(Modifier::BOLD),
            )
        }));
        let rows: Vec<Row> = view
            .records
            .iter()
            .map(|r| {
                Row::new([
                    Cell::from(r.state.label()).style(Style::default().fg(state_ink(pal, r))),
                    Cell::from(short_id(&r.id)),
                    Cell::from(fit(&r.repo, repo_w)),
                    Cell::from(fit(&r.branch, branch_w)),
                    Cell::from(fit(&worktree_label(r), wt_w)),
                    Cell::from(fit(&link_label(r), link_w)),
                    Cell::from(age(r.last_activity, now)),
                    Cell::from(tokens(r.context_tokens)),
                    Cell::from(format!("${:.2}", r.cost_usd)),
                ])
            })
            .collect();
        let widths = [
            Constraint::Length(7),
            Constraint::Length(8),
            Constraint::Length(repo_w as u16),
            Constraint::Length(branch_w as u16),
            Constraint::Length(wt_w as u16),
            Constraint::Length(link_w as u16),
            Constraint::Length(5),
            Constraint::Length(6),
            Constraint::Length(9),
        ];
        let table = Table::new(rows, widths)
            .header(header)
            .block(block)
            .column_spacing(1)
            .row_highlight_style(
                Style::default()
                    .fg(pal.color(pal.sel_fg))
                    .bg(pal.color(pal.sel_bg)),
            );
        let mut state = TableState::default().with_selected(Some(view.selected));
        frame.render_stateful_widget(table, body, &mut state);
    }
    let suggestion = match view.current() {
        Some(r) if !r.hint.is_empty() => {
            let mark = if r.needs_attention { "! " } else { "" };
            format!(" {mark}{}", r.hint)
        }
        Some(_) => " Nothing to suggest for this session.".to_string(),
        None => String::new(),
    };
    let style = match view.current() {
        Some(r) if r.needs_attention => Style::default().fg(pal.color(pal.error)),
        _ => plain,
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(fit(&suggestion, usize::from(hint.width)), style),
            Line::styled(
                " Suggestions only: nothing here closes, removes or stops anything.",
                plain,
            ),
        ]),
        hint,
    );
    frame.render_widget(Paragraph::new(footer_line(view, pal, now)), footer);
}

fn footer_line(view: &SessionsView, pal: &Palette, now: i64) -> Line<'static> {
    let plain = Style::default().fg(pal.color(pal.fg));
    let keys = Style::default()
        .fg(pal.color(pal.hi))
        .add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::raw(" ")];
    for (key, label) in [
        ("j/k", "move"),
        ("v", "order"),
        ("r", "refresh"),
        ("p/esc", "back"),
        ("q", "quit"),
    ] {
        spans.push(Span::styled(key, keys));
        spans.push(Span::styled(format!(" {label} "), plain));
    }
    let status = match (&view.error, view.read_at) {
        (Some(e), _) => format!(" refresh failed: {e}"),
        (None, Some(at)) => format!(
            " updated {} {}, every {REFRESH_SECONDS}s, {} ago",
            clock(at),
            crate::tz::describe(at),
            age(at, now)
        ),
        (None, None) => String::new(),
    };
    let style = if view.error.is_some() {
        Style::default().fg(pal.color(pal.error))
    } else {
        plain
    };
    spans.push(Span::styled(status, style));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_view::{Place, Pr, PrKind};
    use crate::tui::icon::Icon;
    use crate::tui::theme::{ColorDepth, Theme};
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyEventKind;
    use ratatui::Terminal;

    const NOW: i64 = 1_788_400_000;

    fn record(id: &str, repo: &str, state: State, ago: i64, dirty: bool) -> Record {
        let pr = (state == State::Done).then_some(Pr {
            number: 540,
            kind: PrKind::Merged,
        });
        let (needs_attention, hint) = match (state, dirty) {
            (State::Done, true) => (true, "uncommitted changes, nothing is removed".to_string()),
            (State::Done, false) => (false, "work landed: close this session".to_string()),
            _ => (false, String::new()),
        };
        Record {
            id: id.into(),
            agent: "claude-code".into(),
            repo: repo.into(),
            branch: "feat/536-x".into(),
            place: Place {
                cwd: Some("/w/r".into()),
                worktree: Some("/w/r/536".into()),
                convention: "cc",
                dirty: Some(dirty),
                ..Place::default()
            },
            issue: Some(536),
            pr,
            state,
            last_activity: NOW - ago,
            context_tokens: 182_000,
            cost_usd: 1.5,
            unpriced: 0,
            messages: 3,
            needs_attention,
            hint,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        let mut k = KeyEvent::new(code, KeyModifiers::NONE);
        k.kind = KeyEventKind::Press;
        k
    }

    fn screen(view: &SessionsView, width: u16, height: u16) -> String {
        screen_with(view, width, height, Icon::Off)
    }

    fn screen_with(view: &SessionsView, width: u16, height: u16, icon: Icon) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let pal = Theme::default().palette(ColorDepth::detect(None, None));
        terminal.draw(|f| render(f, view, &pal, NOW, icon)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn loaded() -> SessionsView {
        let mut view = SessionsView::default();
        view.refresh(
            vec![
                record("aaaaaaaa11", "playbook", State::Working, 30, false),
                record("bbbbbbbb22", "playbook", State::Done, 7200, true),
                record("cccccccc33", "other", State::Done, 9000, false),
            ],
            NOW,
        );
        view
    }

    #[test]
    fn before_the_first_read_the_view_says_so_and_an_empty_read_says_none() {
        let mut view = SessionsView::default();
        assert!(screen(&view, 100, 20).contains("Reading sessions..."));
        view.refresh(Vec::new(), NOW);
        assert!(screen(&view, 100, 20).contains("No recent sessions."));
    }

    #[test]
    fn every_column_the_suggestion_and_the_footer_are_drawn() {
        let text = screen(&loaded(), 130, 20);
        for want in [
            "Sessions",
            "order: activity",
            "working",
            "aaaaaaaa",
            "playbook",
            "feat/536-x",
            "536 (cc)",
            "PR #540 merged",
            "182k",
            "$1.50",
            "Suggestions only",
            "updated 01:46:40 UTC",
            "j/k",
            "refresh",
        ] {
            assert!(text.contains(want), "missing {want:?} in\n{text}");
        }
    }

    #[test]
    fn the_selected_session_shows_its_suggestion_and_attention_leads_in_state_order() {
        let mut view = loaded();
        view.key(key(KeyCode::Char('v')));
        view.key(key(KeyCode::Char('v')));
        assert_eq!(view.order, Order::State);
        assert_eq!(view.records()[0].id, "bbbbbbbb22");
        view.selected = 0;
        assert!(screen(&view, 130, 20).contains("! uncommitted changes, nothing is removed"));
    }

    #[test]
    fn the_state_order_ranks_the_seven_states() {
        let mut view = SessionsView::default();
        let states = [
            State::Closed,
            State::Working,
            State::Idle,
            State::Stale,
            State::Waiting,
            State::Done,
        ];
        let records = states
            .iter()
            .enumerate()
            .map(|(i, state)| record(&format!("s{i}"), "playbook", *state, 10, false))
            .collect();
        view.refresh(records, NOW);
        view.order = Order::State;
        view.sort();
        let got: Vec<State> = view.records().iter().map(|r| r.state).collect();
        assert_eq!(
            got,
            [
                State::Done,
                State::Waiting,
                State::Stale,
                State::Idle,
                State::Working,
                State::Closed,
            ]
        );
    }

    #[test]
    fn every_state_draws_its_label_in_the_row() {
        let rows = [
            (State::Working, "│working aaaaaaaa playbook         feat/536-x                   536 (cc)               issue #536?        now   182k   $1.50     │"),
            (State::Waiting, "│waiting aaaaaaaa playbook         feat/536-x                   536 (cc)               issue #536?        now   182k   $1.50     │"),
            (State::Idle, "│idle    aaaaaaaa playbook         feat/536-x                   536 (cc)               issue #536?        now   182k   $1.50     │"),
            (State::Stale, "│stale   aaaaaaaa playbook         feat/536-x                   536 (cc)               issue #536?        now   182k   $1.50     │"),
            (State::Done, "│done    aaaaaaaa playbook         feat/536-x                   536 (cc)               PR #540 merged     now   182k   $1.50     │"),
            (State::Crashed, "│crashed aaaaaaaa playbook         feat/536-x                   536 (cc)               issue #536?        now   182k   $1.50     │"),
            (State::Closed, "│closed  aaaaaaaa playbook         feat/536-x                   536 (cc)               issue #536?        now   182k   $1.50     │"),
        ];
        for (state, want) in rows {
            // Arrange
            let mut view = SessionsView::default();
            view.refresh(
                vec![record("aaaaaaaa11", "playbook", state, 30, false)],
                NOW,
            );
            // Act
            let text = screen(&view, 130, 20);
            let row = text.lines().find(|l| l.contains("aaaaaaaa")).unwrap_or("");
            // Assert
            assert_eq!(row, want, "{state:?}");
        }
    }

    #[test]
    fn the_repo_order_groups_repos_together() {
        let mut view = loaded();
        view.key(key(KeyCode::Char('v')));
        assert_eq!(view.order, Order::Repo);
        let repos: Vec<&str> = view.records().iter().map(|r| r.repo.as_str()).collect();
        assert_eq!(repos, ["other", "playbook", "playbook"]);
    }

    #[test]
    fn keys_move_the_selection_inside_the_list_and_report_what_they_ask_for() {
        let mut view = loaded();
        assert_eq!(view.key(key(KeyCode::Char('j'))), Action::None);
        assert_eq!(view.selected, 1);
        view.key(key(KeyCode::Char('G')));
        assert_eq!(view.selected, 2);
        view.key(key(KeyCode::Down));
        assert_eq!(view.selected, 2);
        view.key(key(KeyCode::Char('g')));
        assert_eq!(view.selected, 0);
        assert_eq!(view.key(key(KeyCode::Char('r'))), Action::Refresh);
        assert_eq!(view.key(key(KeyCode::Char('p'))), Action::Close);
        assert_eq!(view.key(key(KeyCode::Esc)), Action::Close);
        assert_eq!(view.key(key(KeyCode::Char('q'))), Action::Quit);
    }

    #[test]
    fn a_refresh_keeps_the_selected_session_when_the_order_changes() {
        let mut view = loaded();
        view.selected = 1;
        let id = view.records()[1].id.clone();
        let mut next = view.records().to_vec();
        next.insert(
            0,
            record("dddddddd44", "playbook", State::Working, 5, false),
        );
        view.refresh(next, NOW + 30);
        assert_eq!(view.records()[view.selected].id, id);
    }

    #[test]
    fn a_failed_refresh_is_named_in_the_footer_and_the_rows_stay() {
        let mut view = loaded();
        view.fail("database is locked".into());
        let text = screen(&view, 130, 20);
        assert!(text.contains("refresh failed: database is locked"));
        assert!(text.contains("aaaaaaaa"));
    }

    #[test]
    fn a_narrow_screen_still_draws_without_panicking() {
        for width in [40, 80, 100] {
            screen(&loaded(), width, 12);
        }
    }

    #[test]
    fn the_sessions_title_carries_the_icon_unless_it_is_off() {
        let first = |icon| {
            screen_with(&loaded(), 80, 12, icon)
                .lines()
                .next()
                .unwrap_or_default()
                .to_string()
        };
        assert!(first(Icon::Wide).contains("\u{2524}\u{1F4FA}  Sessions"));
        assert!(first(Icon::Narrow).contains("\u{2524}\u{1F4FA}   Sessions"));
        let off = first(Icon::Off);
        assert!(off.contains("\u{2524}Sessions") && !off.contains('\u{1F4FA}'));
    }
}
