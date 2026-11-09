// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook migrate`, the one front door to the migration runner (ADR 0034).
//! A plain run is the same heal `doctor --fix` runs. `--status` and
//! `--dry-run` read only.

use crate::heal::{self, Outcome, Record, Trigger};
use crate::init::migrate::{self, Ctx, Planned, State};
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::path::Path;

/// What `playbook migrate` was asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Run,
    Status,
    DryRun,
}

/// Runs `playbook migrate` for `home`, prints the result, and returns the
/// exit code: 1 when the record could not be read or written, a migration
/// failed, or the lock was busy, else 0.
/// A run also updates the playbook plugin when `plugin` is true (ADR 0034,
/// 2026-10-19 amendment, decision 6); `--dry-run` and `--status` never do.
pub fn run(home: &Path, mode: Mode, json: bool, plugin: bool) -> i32 {
    use std::io::IsTerminal;
    // The list counts as seen only when a person reads it: not when a
    // caller such as `playbook upgrade` or the automatic update captures it.
    let seen = std::io::stdout().is_terminal() && !crate::common::headless::is_headless();
    run_with(home, mode, json, seen, plugin)
}

/// [`run`], where `seen` says whether a person reads the output, so the
/// session hook counts the printed items as seen.
pub fn run_with(home: &Path, mode: Mode, json: bool, seen: bool, plugin: bool) -> i32 {
    let ctx = Ctx::for_home(home);
    match mode {
        Mode::Run => {
            let now = crate::common::time::now_epoch_secs();
            let out = heal::run(&ctx, Trigger::Fix, now);
            eprint!("{}", out.notices.render());
            // Before the list, so an updated plugin drops off it.
            let plugin_outcome = plugin
                .then(|| crate::plugin_update::run_for_command(home, false))
                .flatten();
            let plugin_line = plugin_outcome.as_ref().and_then(|o| o.line());
            let skills_lines = if plugin {
                crate::skills_sync::refresh(home, false)
            } else {
                Vec::new()
            };
            let mut items = out.items(home);
            // A refreshed marketplace without a newer plugin leaves the manual
            // fix with nothing to do, and the plugin line already says why.
            if plugin_outcome
                .as_ref()
                .is_some_and(crate::plugin_update::Outcome::awaits_release)
            {
                items.retain(|i| !i.fix.starts_with(crate::plugin_update::COMMAND_PREFIX));
            }
            if json {
                for line in plugin_line.iter().chain(&skills_lines) {
                    eprintln!("{line}");
                }
                println!("{}", run_json(&out, &items));
            } else {
                for line in plugin_line.iter().chain(&skills_lines) {
                    println!("{line}");
                }
                print!("{}", heal::render_fix(&out, &items));
                // Printed to a person, so the session hook counts them as
                // seen (ADR 0034, 2026-10-19 amendment, decision 3). JSON
                // goes to a script, so it does not count.
                if seen && out.skipped != Some(heal::Skip::RealHomeUnderTest) {
                    let root = crate::common::paths::playbook_root_from(home);
                    crate::pending::acknowledge(&root, &items, now);
                }
            }
            out.exit_code()
        }
        Mode::Status | Mode::DryRun => {
            let root = crate::common::paths::playbook_root_from(home);
            let (Ok(record), Some(plan)) = (Record::read_only(&root), migrate::plan(&ctx)) else {
                eprintln!("playbook migrate: the upgrade record could not be read");
                return 1;
            };
            let items = items_of(home, &plan);
            // Planned only: no claim, no writes.
            let skills_lines = if plugin && mode == Mode::DryRun {
                crate::skills_sync::refresh(home, true)
            } else {
                Vec::new()
            };
            let text = match (mode, json) {
                (Mode::Status, true) => format!("{}\n", status_json(&record, &plan, &items)),
                (Mode::Status, false) => render_status(&record, &plan, &items),
                (_, true) => {
                    let mut value = dry_run_json(&record, &plan, &items);
                    value["skills"] = json!(skills_lines);
                    format!("{value}\n")
                }
                (_, false) => {
                    let lines: String = skills_lines.iter().map(|l| format!("{l}\n")).collect();
                    format!("{lines}{}", render_dry_run(&record, &plan, &items))
                }
            };
            print!("{text}");
            0
        }
    }
}

/// The open items of `plan` with the plugin items, as `pending` lists them.
fn items_of(home: &Path, plan: &[Planned]) -> Vec<crate::pending::Item> {
    crate::pending::from_plan(home, plan)
}

/// The item list, or the line that says nothing needs the user.
fn render_items(items: &[crate::pending::Item]) -> String {
    if items.is_empty() {
        "nothing needs you\n".to_string()
    } else {
        crate::pending::render(items)
    }
}

/// Whether a newer playbook wrote the record, so a run would do nothing.
fn downgraded(record: &Record) -> bool {
    record.compare_with(env!("CARGO_PKG_VERSION")) == Ordering::Greater
}

fn recorded_line(record: &Record) -> String {
    match &record.binary {
        Some(b) => format!("recorded: playbook {b}"),
        None => "recorded: nothing yet".to_string(),
    }
}

fn state_text(state: &State) -> String {
    match state {
        State::Applied => "applied".into(),
        State::Pending => "pending".into(),
        State::EveryRun => "runs on every run".into(),
        State::Clear => "nothing to do".into(),
        State::Open { message, command } => heal::needs_you(message, *command).trim_end().into(),
        State::Unknown => "not checked: old state files are not imported yet".into(),
        State::Unreleased => "waits for the next release".into(),
    }
}

fn render_status(record: &Record, plan: &[Planned], items: &[crate::pending::Item]) -> String {
    let mut text = format!(
        "running: playbook {}\n{}\n",
        env!("CARGO_PKG_VERSION"),
        recorded_line(record)
    );
    let width = plan.iter().map(|p| p.id.len()).max().unwrap_or(0);
    let since = plan
        .iter()
        .map(|p| p.since.as_str().len())
        .max()
        .unwrap_or(0);
    for p in plan {
        text.push_str(&format!(
            "{:<since$}  {:<width$}  {:<10}  {:<7}  {}\n",
            p.since.as_str(),
            p.id,
            p.kind,
            p.scope,
            state_text(&p.state)
        ));
    }
    text.push('\n');
    text.push_str(&render_items(items));
    text
}

/// The migrations a run would start now, in order: none after a downgrade.
fn would_run(record: &Record, plan: &[Planned]) -> Vec<&'static str> {
    if downgraded(record) {
        return Vec::new();
    }
    plan.iter().filter(|p| p.runs).map(|p| p.id).collect()
}

/// The open items of `plan`: `Owned` ones are notes, the rest need the user.
fn open_items(plan: &[Planned]) -> impl Iterator<Item = (&Planned, &String, Option<&'static str>)> {
    plan.iter().filter_map(|p| match &p.state {
        State::Open { message, command } => Some((p, message, *command)),
        _ => None,
    })
}

fn render_dry_run(record: &Record, plan: &[Planned], items: &[crate::pending::Item]) -> String {
    let run = would_run(record, plan);
    let mut text = if downgraded(record) {
        format!("nothing would run: {}\n", heal::Skip::Downgrade.reason())
    } else if run.is_empty() {
        "nothing would run\n".to_string()
    } else {
        format!("would run: {}\n", run.join(", "))
    };
    for (p, message, _) in open_items(plan) {
        if p.scope == "owned" {
            text.push_str(&format!("note: {}: {message}\n", p.id));
        }
    }
    text.push_str(&render_items(items));
    text.push_str("dry run: nothing was written\n");
    text
}

fn record_json(record: &Record) -> Value {
    json!({
        "binary": record.binary,
        "schema": record.schema,
        "plugin": record.plugin,
        "pending": record.pending,
        "failed_at": record.failed_at,
    })
}

/// The items as JSON, one object per bullet.
fn items_json(items: &[crate::pending::Item]) -> Value {
    json!(items
        .iter()
        .map(|i| json!({
            "slug": i.slug,
            "kind": match i.kind {
                crate::pending::Kind::Consent => "consent",
                crate::pending::Kind::Report => "report",
            },
            "what": i.what,
            "why": i.why,
            "fix": i.fix,
        }))
        .collect::<Vec<_>>())
}

fn status_json(record: &Record, plan: &[Planned], items: &[crate::pending::Item]) -> Value {
    let migrations: Vec<Value> = plan
        .iter()
        .map(|p| {
            let (message, command) = match &p.state {
                State::Open { message, command } => (Some(message.as_str()), *command),
                _ => (None, None),
            };
            json!({
                "id": p.id,
                "since": p.since.as_str(),
                "kind": p.kind,
                "scope": p.scope,
                "state": p.state.code(),
                "runs": p.runs,
                "message": message,
                "command": command,
            })
        })
        .collect();
    json!({
        "running": {"binary": env!("CARGO_PKG_VERSION"), "schema": heal::LEGACY_SCHEMA},
        "recorded": record_json(record),
        "migrations": migrations,
        "items": items_json(items),
    })
}

fn dry_run_json(record: &Record, plan: &[Planned], items: &[crate::pending::Item]) -> Value {
    let (notes, open): (Vec<_>, Vec<_>) =
        open_items(plan).partition(|(p, _, _)| p.scope == "owned");
    json!({
        "would_run": would_run(record, plan),
        "skipped": downgraded(record).then(|| heal::Skip::Downgrade.code()),
        "notes": notes
            .iter()
            .map(|(p, message, _)| format!("{}: {message}", p.id))
            .collect::<Vec<_>>(),
        "open": open
            .iter()
            .map(|(p, message, command)| json!({"id": p.id, "message": message, "command": command}))
            .collect::<Vec<_>>(),
        "items": items_json(items),
        "written": false,
    })
}

fn run_json(out: &Outcome, items: &[crate::pending::Item]) -> Value {
    let steps = |list: &[crate::init::run::StepReport]| {
        list.iter()
            .map(|s| json!({"name": s.name, "detail": s.detail}))
            .collect::<Vec<_>>()
    };
    json!({
        "skipped": out.skipped.map(heal::Skip::code),
        "reason": out.skipped.map(heal::Skip::reason),
        "changed": steps(&out.changed),
        "failed": steps(&out.failed),
        "notes": out.warnings,
        "open": out
            .open
            .iter()
            .map(|i| json!({"id": i.id, "message": i.message, "command": i.command}))
            .collect::<Vec<_>>(),
        "items": items_json(items),
        "recorded": out.recorded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(binary: Option<&str>, schema: u32) -> Record {
        Record {
            binary: binary.map(str::to_string),
            schema,
            ..Record::default()
        }
    }

    fn planned(id: &'static str, scope: &'static str, state: State, runs: bool) -> Planned {
        Planned {
            id,
            since: migrate::Since::At("0.36.0"),
            kind: "idempotent",
            scope,
            state,
            runs,
        }
    }

    fn planned_next() -> Planned {
        Planned {
            since: migrate::Since::Next,
            ..planned("label-e", "owned", State::Unreleased, false)
        }
    }

    fn sample() -> Vec<Planned> {
        vec![
            planned("move-a", "owned", State::EveryRun, true),
            planned("budget-b", "owned", State::Applied, false),
            planned(
                "command-c",
                "consent",
                State::Open {
                    message: "old command".into(),
                    command: Some("playbook setup --statusline"),
                },
                false,
            ),
            planned(
                "caches-d",
                "owned",
                State::Open {
                    message: "old folders".into(),
                    command: None,
                },
                false,
            ),
        ]
    }

    #[test]
    fn a_run_a_person_reads_counts_its_report_items_as_seen() {
        // Arrange: a plugin one line behind, so one report item is open.
        let home = crate::common::test_support::scratch_dir("migrate-seen");
        let dir = home.join(".claude/plugins/cache/amplifiedengineer/playbook/0.0.1");
        std::fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(dir.join("skills")).unwrap();
        std::fs::write(
            dir.join(".claude-plugin/plugin.json"),
            r#"{"name":"playbook","version":"0.0.1"}"#,
        )
        .unwrap();
        std::fs::write(
            home.join(".claude/plugins/installed_plugins.json"),
            format!(
                r#"{{"plugins":{{"playbook@amplifiedengineer":[{{"scope":"user","installPath":"{}"}}]}}}}"#,
                dir.display()
            ),
        )
        .unwrap();
        let root = crate::common::paths::playbook_root_from(&home);
        let session = || {
            let items = crate::pending::plugin_items(&crate::pending::PluginProbe {
                claude_home: home.join(".claude"),
                session_root: None,
                binary: env!("CARGO_PKG_VERSION").into(),
                homebrew: false,
            });
            crate::pending::session_notice(
                &root,
                &items,
                u64::MAX / 2,
                true,
                crate::pending::Prune::NONE,
            )
        };

        // Act: a captured run, then a run a person reads.
        run_with(&home, Mode::Run, false, false, false);
        let after_captured = session();
        run_with(&home, Mode::Run, false, true, false);
        let after_seen = crate::pending::session_notice(
            &root,
            &crate::pending::plugin_items(&crate::pending::PluginProbe {
                claude_home: home.join(".claude"),
                session_root: None,
                binary: env!("CARGO_PKG_VERSION").into(),
                homebrew: false,
            }),
            u64::MAX / 2 + 10 * crate::pending::REMIND_AFTER_SECS,
            true,
            crate::pending::Prune::NONE,
        );

        // Assert
        assert!(after_captured.is_some_and(|t| t.contains("Fix: claude plugin update")));
        assert_eq!(
            after_seen, None,
            "migrate printed it, so it does not come back"
        );
    }

    #[test]
    fn a_dry_run_names_what_a_run_starts_unless_a_newer_playbook_ran() {
        // Arrange
        let plan = sample();

        // Act
        let items = items_of(
            &crate::common::test_support::scratch_dir("migrate-dry"),
            &plan,
        );
        let old = render_dry_run(&record(Some("0.25.0"), 1), &plan, &items);
        let same = render_dry_run(
            &record(Some(env!("CARGO_PKG_VERSION")), heal::LEGACY_SCHEMA),
            &plan,
            &items,
        );
        let newer = render_dry_run(&record(Some("999.0.0"), 999), &plan, &items);

        // Assert
        assert!(old.starts_with("would run: move-a\n"), "{old}");
        assert!(
            old.contains("  - Old command.\n    Why: ")
                && old.contains("    Fix: playbook setup --statusline\n"),
            "{old}"
        );
        assert!(old.contains("note: caches-d: old folders\n"), "{old}");
        assert!(old.ends_with("dry run: nothing was written\n"));
        assert!(same.starts_with("would run: move-a\n"), "{same}");
        assert!(
            newer.starts_with("nothing would run: a newer playbook"),
            "{newer}"
        );
        let value = dry_run_json(&record(Some("999.0.0"), 999), &plan, &items);
        assert_eq!(value["would_run"], json!([]));
        assert_eq!(value["skipped"], "downgrade");
        assert_eq!(value["notes"], json!(["caches-d: old folders"]));
        assert_eq!(value["open"][0]["command"], "playbook setup --statusline");
        assert_eq!(value["items"][0]["fix"], "playbook setup --statusline");
    }

    #[test]
    fn status_shows_each_migration_with_its_scope_and_state() {
        // Arrange
        let plan = sample();

        // Act
        let items = items_of(
            &crate::common::test_support::scratch_dir("migrate-status"),
            &plan,
        );
        let text = render_status(&record(None, 0), &plan, &items);
        let value = status_json(&record(None, 0), &plan, &items);

        // Assert
        assert!(text.contains("recorded: nothing yet"), "{text}");
        assert!(
            text.contains("0.36.0  move-a     idempotent  owned    runs on every run"),
            "{text}"
        );
        assert!(
            text.contains("0.36.0  budget-b   idempotent  owned    applied"),
            "{text}"
        );
        assert!(text.starts_with(&format!(
            "running: playbook {}\nrecorded: nothing yet\n",
            env!("CARGO_PKG_VERSION")
        )));
        assert!(!text.contains("migrations up to"), "{text}");
        assert_eq!(value["migrations"][0]["state"], "every_run");
        assert_eq!(value["migrations"][0]["since"], "0.36.0");
        assert_eq!(value["migrations"][0]["id"], "move-a");
        let next_text = render_status(&record(None, 0), &[planned_next()], &[]);
        let next_value = status_json(&record(None, 0), &[planned_next()], &[]);
        assert!(
            next_text.contains("next  label-e  idempotent  owned    waits for the next release"),
            "{next_text}"
        );
        assert_eq!(next_value["migrations"][0]["since"], "next");
        assert_eq!(next_value["migrations"][0]["state"], "unreleased");
        assert_eq!(next_value["migrations"][0]["runs"], false);
        assert_eq!(value["migrations"][2]["state"], "open");
        assert_eq!(value["running"]["binary"], env!("CARGO_PKG_VERSION"));
        assert!(
            text.ends_with("To go through every setup change at once, run: playbook setup\n"),
            "{text}"
        );
        assert_eq!(value["items"][0]["fix"], "playbook setup --statusline");
        assert!(next_text.ends_with("nothing needs you\n"), "{next_text}");
    }
}
