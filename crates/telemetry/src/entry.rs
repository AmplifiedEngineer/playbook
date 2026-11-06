// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook usage` and its subcommands: picks the terminal view or the text or
//! JSON output.

use super::command::UsageCommand;
use super::daemon::{self, Stopped};
use super::period::Period;
use super::query::{self, Range, WeekStart};
use super::run::{ingest_new, run_summary, Paths};
use super::{api, db, tui};
use rusqlite::Connection;
use std::path::Path;
use std::time::Duration;

/// What the removed `usage ingest` says to anyone who still calls it.
const INGEST_REMOVED: &str =
    "`usage ingest` was removed: use `playbook usage --summary`, the terminal view warms itself";

/// How long `usage stop` waits for the collector before it may kill it.
const STOP_BUDGET: Duration = Duration::from_secs(3);

/// The flags of `playbook usage` itself.
pub struct Opts {
    pub json: bool,
    pub summary: bool,
    /// The `--range` value, or `None` when the flag was not given.
    pub range: Option<String>,
}

/// Whether the terminal view can run: it needs a terminal on both ends.
pub fn interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// The `--json` document: the totals and groups, the limits and burn rate
/// under `limits`, plus
/// the live view under `live`, and for today, week and month the comparison
/// with the previous period under `comparison`.
pub fn json_report(
    conn: &Connection,
    paths: &Paths,
    range: Range,
    now: i64,
) -> Result<String, String> {
    let usage = query::load_window(conn, range, now)?;
    let tools = db::load_tool_events(conn)?;
    let mut data = api::data_json(&usage, &tools, now, range);
    let i = query::load_live_inputs(conn, now)?;
    let live = api::live_json(&i.window, &i.sessions, &i.latest, now);
    let since = super::limits_source::events_since(now, configured_week_start());
    let limit_events = db::load_usage_events_since(conn, since)?;
    let limits = super::run::limits_json(paths, conn, &limit_events, now);
    if let Some(map) = data.as_object_mut() {
        map.insert("live".to_string(), live);
        map.insert("limits".to_string(), limits);
        if let Some(period) = Period::of_range(range) {
            let events = db::load_usage_events_since(conn, period.windows(now).previous.start)?;
            map.insert(
                "comparison".to_string(),
                api::comparison_json(&events, now, period),
            );
        }
    }
    serde_json::to_string_pretty(&data).map_err(|e| format!("failed to encode usage data: {e}"))
}

/// Runs the command. The returned text, when not empty, is printed on stdout.
pub fn run(
    sub: Option<UsageCommand>,
    opts: &Opts,
    paths: &Paths,
    interactive: bool,
) -> Result<String, String> {
    // Every command starts from a data file that exists (created owner-only,
    // with older data moved in first), so no command errors on a fresh home.
    // This runs before the terminal view takes over the screen.
    super::tz::ensure_init();
    // A rehearsal on a copy of the data file touches nothing else.
    if let Some(UsageCommand::Account {
        command,
        db: Some(db),
    }) = &sub
    {
        if !db.is_file() {
            return Err(format!("no usage database at {}", db.display()));
        }
        let mut copy = paths.clone();
        copy.db = db.clone();
        return super::account_cmd::run(&copy, command);
    }
    // The doctor only reads files, so it runs before the data file is created.
    if let Some(UsageCommand::Doctor { raw }) = sub {
        let usage_dir = paths
            .db
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let budgets = super::limits_source::manual_budgets(&super::host::home_dir());
        return Ok(super::plan::report(
            &paths.claude_json,
            usage_dir,
            &budgets,
            crate::util::time::now_secs(),
            raw,
        ));
    }
    // Stopping needs no data file, so it creates none.
    if let Some(UsageCommand::Stop) = sub {
        return stop_text(
            daemon::stop(paths, STOP_BUDGET, true),
            &daemon::lock_path(paths),
        );
    }
    super::db::open_db(&paths.db).map(drop)?;
    match sub {
        // Answered above, before the data file is created.
        Some(UsageCommand::Doctor { .. } | UsageCommand::Stop) => Ok(String::new()),
        Some(UsageCommand::Ingest) => Err(INGEST_REMOVED.to_string()),
        Some(UsageCommand::Warm) => super::warm::run_warm(paths).map(|()| String::new()),
        Some(UsageCommand::Collect { port, idle_seconds }) => {
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            super::collector::run_collect(paths, port, idle_seconds, &stop)
        }
        Some(UsageCommand::Account { command, .. }) => super::account_cmd::run(paths, &command),
        Some(UsageCommand::Prune { dry_run }) => super::collector::run_prune(paths, dry_run),
        Some(UsageCommand::Status { json }) => {
            let status = super::collector::status(paths, super::collector::configured_retention())?;
            if json {
                serde_json::to_string_pretty(&status)
                    .map_err(|e| format!("failed to encode the collector status: {e}"))
            } else {
                Ok(status.text(crate::util::time::now_secs()))
            }
        }
        None if opts.summary => {
            let range = opts.range.as_deref().map(parse_range).transpose()?;
            run_summary(paths, range, crate::util::time::now_secs())
        }
        None if opts.json => {
            let range = parse_range(opts.range.as_deref().unwrap_or(DEFAULT_RANGE))?;
            let (conn, _) = ingest_new(paths)?;
            json_report(&conn, paths, range, crate::util::time::now_secs())
        }
        None if interactive => {
            tui::run(paths, opts.range.as_deref().map(parse_range).transpose()?)?;
            Ok(String::new())
        }
        None => {
            let range = opts.range.as_deref().map(parse_range).transpose()?;
            run_summary(paths, range, crate::util::time::now_secs())
        }
    }
}

/// What `usage stop` prints for each outcome.
fn stop_text(stopped: Stopped, lock: &Path) -> Result<String, String> {
    match stopped {
        Stopped::Stopped | Stopped::Killed => Ok("stopped".to_string()),
        Stopped::NotRunning => Ok("not running".to_string()),
        Stopped::StillRunning => Err(format!(
            "the collector did not stop within {} seconds; see who holds {}",
            STOP_BUDGET.as_secs(),
            lock.display()
        )),
        Stopped::Unavailable => Err(format!("cannot read the lock {}", lock.display())),
    }
}

/// The range of `--json` when `--range` is not given.
const DEFAULT_RANGE: &str = "30d";

/// The first day of the week, from the `usage.weekStart` config key. Monday
/// when the key is unset or unreadable.
pub fn configured_week_start() -> WeekStart {
    super::host::config_value_now("usage.weekStart")
        .and_then(|value| value.as_str().and_then(WeekStart::parse))
        .unwrap_or_default()
}

/// Seconds between refreshes of the terminal view, from the
/// `usage.refreshSeconds` config key.
pub fn configured_refresh_seconds() -> u64 {
    super::host::config_value_now("usage.refreshSeconds")
        .and_then(|value| value.as_u64())
        .filter(|n| *n > 0)
        .unwrap_or(tui::worker::DEFAULT_REFRESH_SECONDS)
}

/// Parses a range name with the configured week start.
pub fn parse_range(value: &str) -> Result<Range, String> {
    Range::parse_with(Some(value), configured_week_start()).ok_or_else(|| {
        format!(
            "unknown range `{value}`; use today, week, last-week, month, last-month, 30d, 60d, 90d or all"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::AccountCommand;

    fn paths(name: &str) -> Paths {
        let dir = crate::util::scratch_dir(name);
        let mut p = Paths::from_home(&dir);
        p.db = dir.join("usage").join("usage.db");
        p
    }

    fn no_opts() -> Opts {
        Opts {
            json: false,
            summary: false,
            range: None,
        }
    }

    #[test]
    fn stop_with_nothing_running_says_so_and_creates_no_data_file() {
        let p = paths("entry-stop-none");

        let out = run(Some(UsageCommand::Stop), &no_opts(), &p, false);

        assert_eq!(out, Ok("not running".to_string()));
        assert!(!p.db.exists());
    }

    #[cfg(unix)]
    #[test]
    fn stop_ends_a_running_collector_and_frees_the_lock() {
        let p = paths("entry-stop-running");
        let _collector = crate::collector::tests::Running::start(&p);

        let out = run(Some(UsageCommand::Stop), &no_opts(), &p, false);

        assert_eq!(out, Ok("stopped".to_string()));
        assert_eq!(
            crate::daemon::lock_state(&p),
            crate::daemon::LockState::Free
        );
    }

    #[test]
    fn a_collector_that_stays_is_an_error() {
        let err = stop_text(Stopped::StillRunning, Path::new("/x/collector.lock")).unwrap_err();
        assert!(err.contains("did not stop"), "{err}");
        let err = stop_text(Stopped::Unavailable, Path::new("/x/collector.lock")).unwrap_err();
        assert!(err.contains("/x/collector.lock"), "{err}");
        assert_eq!(
            stop_text(Stopped::Killed, Path::new("/x")),
            Ok("stopped".into())
        );
    }

    fn account(p: &Paths, args: AccountCommand) -> Result<String, String> {
        run(
            Some(UsageCommand::Account {
                command: args,
                db: None,
            }),
            &no_opts(),
            p,
            false,
        )
    }

    #[test]
    fn the_account_commands_name_a_key_and_list_it_through_the_entry_point() {
        let p = paths("entry-account");
        let conn = crate::db::open_db(&p.db).unwrap();
        conn.execute(
            "INSERT INTO usage_events (event_id, timestamp, session_id, account, model, effort,
                repo, branch, input_tokens, output_tokens, cache_creation_tokens,
                cache_read_tokens, cost_usd) VALUES ('e', 86400, 's', 'a@b.c', 'm', '', 'r', 'b', 1, 1, 0, 0, 2.5)",
            [],
        )
        .unwrap();
        let name = |name: &str| AccountCommand::Name {
            key: "a@b.c".into(),
            name: name.into(),
        };

        assert!(account(&p, AccountCommand::List)
            .unwrap()
            .starts_with("a@b.c\n  1 events, $2.50"));
        assert_eq!(account(&p, name("Home")).unwrap(), "a@b.c is shown as Home");
        assert!(account(&p, AccountCommand::List)
            .unwrap()
            .starts_with("Home (a@b.c)"));
        assert_eq!(account(&p, name("")).unwrap(), "cleared the name of a@b.c");
    }

    #[test]
    fn assign_dry_runs_then_applies_and_undoes_through_the_entry_point() {
        crate::tz::set(crate::tz::Zone::Utc);
        let p = paths("entry-assign");
        let conn = crate::db::open_db(&p.db).unwrap();
        conn.execute(
            "INSERT INTO usage_events (event_id, timestamp, session_id, account, model, effort,
                repo, branch, input_tokens, output_tokens, cache_creation_tokens,
                cache_read_tokens, cost_usd) VALUES ('e', 86400, 's', 'a@b.c', 'm', '', 'r', 'b', 1, 1, 0, 0, 2.5)",
            [],
        )
        .unwrap();
        let assign = |confirm| AccountCommand::Assign {
            from: Some("1970-01-02".into()),
            from_session: None,
            before_session: None,
            through_session: None,
            to: Some("1970-01-03".into()),
            target: "personal".into(),
            was: None,
            confirm,
        };

        let dry = account(&p, assign(false)).unwrap();
        assert!(
            dry.contains("1 events ($2.50)") && dry.contains("--confirm"),
            "{dry}"
        );
        assert!(account(&p, assign(true)).unwrap().contains("move 1:"));
        assert!(account(&p, assign(true))
            .unwrap()
            .starts_with("nothing to move"));
        let undone = account(&p, AccountCommand::Undo { id: None }).unwrap();
        assert!(undone.contains("restored 1"), "{undone}");
    }

    #[test]
    fn the_db_option_runs_an_account_command_on_a_copy_and_leaves_the_default_alone() {
        let p = paths("entry-account-db");
        let copy = crate::util::scratch_dir("entry-account-copy").join("copy.db");
        crate::db::open_db(&copy).unwrap();
        let on = |db: &Path| {
            run(
                Some(UsageCommand::Account {
                    command: AccountCommand::List,
                    db: Some(db.to_path_buf()),
                }),
                &no_opts(),
                &p,
                false,
            )
        };

        assert_eq!(on(&copy), Ok("No usage recorded yet.".to_string()));
        assert!(!p.db.exists(), "the default database was not created");
        let err = on(&copy.with_file_name("missing.db")).unwrap_err();
        assert!(err.contains("no usage database at"), "{err}");
    }

    #[test]
    fn a_range_names_one_of_the_nine_values() {
        for ok in [
            "today",
            "week",
            "last-week",
            "month",
            "last-month",
            "30d",
            "60d",
            "90d",
            "all",
        ] {
            assert!(parse_range(ok).is_ok(), "{ok}");
        }
        let err = parse_range("7d").unwrap_err();
        assert!(err.contains("unknown range `7d`"), "{err}");
    }
}
