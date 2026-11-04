// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The interactive Agent Budget Dashboard (`playbook usage`). `data` is the snapshot it
//! draws, `worker` keeps it fresh in the background, `app` is the state and
//! keys, `view` draws a frame, and `run` is the terminal loop around them.

pub mod app;
pub mod arrange;
pub mod budget;
pub mod burnup;
pub mod chart;
pub mod data;
pub mod editor;
pub mod editor_view;
pub mod fmt;
pub mod icon;
pub mod keys;
pub mod layout;
pub mod notice;
pub mod panels;
#[cfg(test)]
mod parity;
pub mod projection;
pub mod scale;
pub mod sessions_view;
#[cfg(test)]
mod snapshots;
pub mod sort;
pub mod theme;
pub mod ui_state;
pub mod view;
pub mod worker;

use super::query::Range;
use super::run::Paths;
use app::{App, Effect};
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use sessions_view::SessionsView;
use std::time::{Duration, Instant};
use theme::{ColorDepth, Theme};
use ui_state::{Autosave, Flushed};
use worker::{Update, Worker};

/// How long the loop waits for a key before it checks for new data.
const TICK: Duration = Duration::from_millis(200);

/// The theme to start with: `NO_COLOR` forces `mono`, else the `usage.theme`
/// config key, which is `default` unless set.
fn start_theme(no_color: bool) -> Theme {
    if no_color {
        return Theme::Mono;
    }
    crate::host::config_value_now("usage.theme")
        .and_then(|value| value.as_str().and_then(Theme::parse))
        .unwrap_or_default()
}

/// The range to open on: the `--range` flag, else the stored one, else this
/// week.
fn start_range(
    flag: Option<Range>,
    saved: &ui_state::UiState,
    week: crate::query::WeekStart,
) -> Range {
    flag.or_else(|| {
        saved
            .range
            .as_deref()
            .and_then(|r| Range::parse_with(Some(r), week))
    })
    .unwrap_or(Range::ThisWeek(week))
}

/// The state to store. A range that only came from `--range` is not stored
/// until the user changes it, so a one-off flag does not stick.
fn state_to_save(app: &App, flag: Option<Range>, saved: &ui_state::UiState) -> ui_state::UiState {
    let mut state = app.ui_state();
    if flag == Some(app.range) {
        state.range = saved.range.clone();
    }
    state
}

/// Runs the view until the user quits. Restores the terminal on every exit,
/// including a panic (ratatui installs the hook).
pub fn run(paths: &Paths, range: Option<Range>) -> Result<(), String> {
    // Nothing may write to stderr while the screen is drawn.
    crate::say::hold();
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, paths, range);
    ratatui::restore();
    crate::say::release();
    result.map_err(|e| format!("terminal error: {e}"))
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    paths: &Paths,
    range: Option<Range>,
) -> std::io::Result<()> {
    let home = crate::host::home_dir();
    let saved = ui_state::load(&home);
    let week = super::entry::configured_week_start();
    let flag = range;
    let range = start_range(flag, &saved, week);
    let mut app = App::new(range);
    let (layout, layout_warning) = ui_state::load_layout(&home);
    app.layout = layout;
    app.apply_ui_state(&saved);
    app.note = layout_warning;
    let mut last_saved = state_to_save(&app, flag, &saved);
    let mut autosave = Autosave::default();
    app.sources = paths.claude_projects.display().to_string();
    app.week_start = super::entry::configured_week_start();
    // NO_COLOR counts when it is set to anything but empty.
    app.no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    app.theme = start_theme(app.no_color);
    let env = |k: &str| std::env::var(k).ok();
    app.icon = icon::Icon::detect(
        app.no_color,
        env("TERM").as_deref(),
        [
            env("LC_ALL").as_deref(),
            env("LC_CTYPE").as_deref(),
            env("LANG").as_deref(),
        ],
        env("PBTOP_ICON_WIDTH").as_deref(),
    );
    app.depth = ColorDepth::detect(
        std::env::var("COLORTERM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
    );
    let every = Duration::from_secs(super::entry::configured_refresh_seconds());
    app.refresh_every = every;
    app.telemetry = Some(super::telemetry_status::detect_here(paths));
    let worker = Worker::spawn(paths.clone(), app.range, app.filter.clone(), every);
    let mut sessions = SessionsLoop::new(paths.clone());
    while !app.quit {
        app.area = terminal.size().map(Into::into)?;
        sessions.poll();
        terminal.draw(|frame| match &sessions.view {
            Some(v) => sessions_view::render(
                frame,
                v,
                &app.theme.palette(app.depth),
                crate::util::time::now_secs(),
                app.icon,
            ),
            None => view::render(frame, &app),
        })?;
        if event::poll(TICK)? {
            // A resize needs no handling: the next draw reads the new size.
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press && sessions.view.is_some() => {
                    if sessions.key(key) == sessions_view::Action::Quit {
                        app.quit = true;
                    }
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => match app.key(key) {
                    Effect::Requery => worker.ask(app.range, &app.filter),
                    Effect::Sessions => sessions.open(),
                    Effect::SaveLayout(text) => autosave.push(text),
                    Effect::None => {}
                },
                _ => {}
            }
        }
        // Pending layout changes go out every 200 ms while the editor is open,
        // and at once when it closes or the program quits.
        let force = app.editor.is_none() || app.quit;
        match autosave.flush(Instant::now(), force, |text| {
            ui_state::save_layout(&home, text)
        }) {
            Flushed::Idle | Flushed::Saved => {}
            Flushed::Notice(text) | Flushed::Failed(text) => app.note = Some(text),
        }
        let now_state = state_to_save(&app, flag, &saved);
        if now_state != last_saved {
            ui_state::save(&home, &now_state);
            last_saved = now_state;
        }
        while let Ok(update) = worker.updates.try_recv() {
            match update {
                Update::Data(data) => {
                    // A reply for an earlier range may still be in flight.
                    if data.range == app.range && data.filter == app.filter {
                        app.refresh(*data);
                        app.telemetry = Some(super::telemetry_status::detect_here(paths));
                    }
                }
                Update::Error(e) => app.error = Some(e),
                Update::Indexing(progress) => app.indexing = progress,
            }
        }
    }
    if let Some(why) = autosave.finish(|text| ui_state::save_layout(&home, text)) {
        crate::say::notice(&why);
    }
    Ok(())
}

type Loaded = Result<(Vec<crate::session_view::Record>, i64), String>;

/// The Sessions management view and the thread that reads it. A read asks
/// `gh` for each branch, so it runs off the draw loop and the screen never
/// waits on it. At most one read is in flight.
struct SessionsLoop {
    paths: Paths,
    view: Option<SessionsView>,
    reply: Option<std::sync::mpsc::Receiver<Loaded>>,
    last_read: Option<std::time::Instant>,
}

impl SessionsLoop {
    fn new(paths: Paths) -> Self {
        SessionsLoop {
            paths,
            view: None,
            reply: None,
            last_read: None,
        }
    }

    fn open(&mut self) {
        self.view = Some(SessionsView::default());
        self.start();
    }

    fn start(&mut self) {
        if self.reply.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let paths = self.paths.clone();
        std::thread::spawn(move || {
            let home = crate::host::home_dir();
            let _ = tx.send(crate::host::load_sessions(&paths, &home));
        });
        self.reply = Some(rx);
        self.last_read = Some(std::time::Instant::now());
    }

    fn key(&mut self, key: ratatui::crossterm::event::KeyEvent) -> sessions_view::Action {
        let action = match self.view.as_mut() {
            Some(v) => v.key(key),
            None => sessions_view::Action::None,
        };
        match action {
            sessions_view::Action::Close => self.view = None,
            sessions_view::Action::Refresh => self.start(),
            _ => {}
        }
        action
    }

    /// Takes a finished read and starts the next one when the view is open and
    /// the last one is old.
    fn poll(&mut self) {
        let Some(view) = self.view.as_mut() else {
            self.reply = None;
            return;
        };
        if let Some(rx) = &self.reply {
            if let Ok(result) = rx.try_recv() {
                match result {
                    Ok((records, now)) => view.refresh(records, now),
                    Err(e) => view.fail(e),
                }
                self.reply = None;
            }
        }
        let due = self
            .last_read
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(sessions_view::REFRESH_SECONDS));
        if due {
            self.start();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::WeekStart;

    fn saved(range: Option<&str>) -> ui_state::UiState {
        ui_state::UiState {
            range: range.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn the_range_flag_beats_the_stored_range() {
        let got = start_range(Some(Range::Today), &saved(Some("30d")), WeekStart::Monday);

        assert_eq!(got, Range::Today);
    }

    #[test]
    fn the_stored_range_is_used_when_there_is_no_flag() {
        let got = start_range(None, &saved(Some("30d")), WeekStart::Monday);

        assert_eq!(got, Range::LastDays(30));
    }

    #[test]
    fn a_missing_or_invalid_stored_range_opens_on_this_week() {
        let week = Range::ThisWeek(WeekStart::Monday);

        assert_eq!(start_range(None, &saved(None), WeekStart::Monday), week);
        assert_eq!(
            start_range(None, &saved(Some("nonsense")), WeekStart::Monday),
            week
        );
    }

    #[test]
    fn a_flag_range_is_not_stored_until_the_user_changes_it() {
        let mut app = App::new(Range::Today);
        let stored = saved(Some("30d"));

        let untouched = state_to_save(&app, Some(Range::Today), &stored);
        app.range = Range::All;
        let changed = state_to_save(&app, Some(Range::Today), &stored);

        assert_eq!(untouched.range.as_deref(), Some("30d"));
        assert_eq!(changed.range.as_deref(), Some("all"));
    }
}
