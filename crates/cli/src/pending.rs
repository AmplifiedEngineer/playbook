// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The list of what still needs the user after an upgrade, and the one
//! renderer the session hook, `playbook migrate`, `migrate --status`,
//! `doctor` and `doctor --fix` share (ADR 0034, 2026-10-19 amendment).
//!
//! Each item is one bullet: what it is, why it matters, and the exact
//! command that fixes it. The session hook shows an item in full the first
//! time, then at most once a day in one short line, and never again once
//! `playbook migrate` printed a report item. The marks live in the `state`
//! table under `notices/items/<slug>`.

use crate::init::migrate::{self, OpenItem};
use crate::update::resolve::compare;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The `state` key prefix of the per item marks.
const MARK_PREFIX: &str = "notices/items/";

/// How long the hook waits before it shows an item it already showed.
pub const REMIND_AFTER_SECS: u64 = 24 * 60 * 60;

/// The slug of the item for a plugin that needs a newer binary.
pub const PLUGIN_NEEDS_BINARY: &str = "plugin-needs-newer-binary";
/// The slug of the item for a session that loaded an old plugin folder.
pub const PLUGIN_STALE_SESSION: &str = "plugin-stale-session";
/// The slug the plugin range check shares with its migration.
pub const PLUGIN_RANGE: &str = "plugin-version-range";

/// What kind of item it is, which says when it stops showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A change to a file the user owns. It closes when setup applies it or
    /// the user says no.
    Consent,
    /// A finding playbook cannot fix by itself. `playbook migrate` printing
    /// it counts as seen.
    Report,
}

/// One thing that needs the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub slug: String,
    pub kind: Kind,
    /// What it is, in plain words.
    pub what: String,
    /// Why it matters.
    pub why: &'static str,
    /// The exact command, or the plain action, that fixes it.
    pub fix: String,
}

/// The reason shown when a slug has no entry in [`reason`].
const UNKNOWN_REASON: &str = "playbook found it while checking this machine after the upgrade";

/// Why an item matters, by slug.
fn why(slug: &str) -> &'static str {
    reason(slug).unwrap_or(UNKNOWN_REASON)
}

/// The reason for each slug that has one. A test checks that every
/// `Consent` and `Report` migration has an entry.
fn reason(slug: &str) -> Option<&'static str> {
    Some(match slug {
        migrate::STATUSLINE_MIGRATION => {
            "the status line still runs a script that newer playbook no longer ships"
        }
        "shell-init-rc-line" | migrate::RC_LINE_MIGRATION => {
            "every new shell loads a playbook launcher that is retired"
        }
        migrate::DEAD_HOOKS_MIGRATION => {
            "Claude Code runs these hooks on every event and they no longer do anything useful"
        }
        migrate::NO_SLOP_GUARD_MIGRATION => {
            "Claude Code runs a hook entry for a hook playbook no longer has, on every tool call"
        }
        migrate::PROMPT_DEFAULT_MIGRATION => {
            "pb cc sessions start without the playbook system prompt until it is installed or turned off"
        }
        "other-playbook-plugins" => "hooks and skills run twice in every session",
        PLUGIN_RANGE => "skills and commands may call binary features that work differently in this version",
        PLUGIN_NEEDS_BINARY => "skills from this plugin call binary features this version does not have",
        PLUGIN_STALE_SESSION => "this session runs the old skills and commands until Claude Code starts again",
        _ => return None,
    })
}

/// `message` split at its last `Run: ` into the text and the command.
fn split_command(message: &str) -> (String, Option<String>) {
    match message.rsplit_once("Run: ") {
        Some((text, command)) if !command.trim().is_empty() => (
            text.trim_end().trim_end_matches('.').to_string(),
            Some(command.trim().to_string()),
        ),
        _ => (message.trim_end_matches('.').to_string(), None),
    }
}

/// `what` without a trailing `, so <reason>` clause, since the bullet gives
/// the reason on its own line. A list after a colon in the clause stays.
fn without_reason(what: String) -> String {
    match what.rsplit_once(", so ") {
        Some((text, reason)) => match reason.split_once(": ") {
            Some((_, list)) => format!("{text}: {list}"),
            None => text.to_string(),
        },
        None => what,
    }
}

/// Items for the open `Consent` and `Report` migrations.
pub fn from_open(open: &[OpenItem]) -> Vec<Item> {
    open.iter()
        .map(|o| {
            let (what, run) = split_command(&o.message);
            let (kind, fix) = match o.command {
                Some(command) => (Kind::Consent, command.to_string()),
                None => (
                    Kind::Report,
                    run.unwrap_or_else(|| "playbook migrate --status".to_string()),
                ),
            };
            Item {
                slug: o.id.to_string(),
                kind,
                what: without_reason(what),
                why: why(o.id),
                fix,
            }
        })
        .collect()
}

/// What the plugin checks read, passed in so tests never touch the real home.
#[derive(Debug, Clone)]
pub struct PluginProbe {
    pub claude_home: PathBuf,
    /// The plugin folder this session runs from (`CLAUDE_PLUGIN_ROOT`).
    pub session_root: Option<PathBuf>,
    pub binary: String,
    pub homebrew: bool,
}

impl PluginProbe {
    /// The probe for this process and `home`.
    pub fn for_home(home: &Path) -> PluginProbe {
        let exe = std::env::current_exe().unwrap_or_default();
        // A Homebrew binary is often reached through a symlink in /usr/local/bin.
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
        PluginProbe {
            claude_home: home.join(".claude"),
            session_root: std::env::var_os("CLAUDE_PLUGIN_ROOT")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            binary: env!("CARGO_PKG_VERSION").to_string(),
            homebrew: crate::update::is_homebrew(&exe),
        }
    }
}

/// The plugin items: a plugin that needs a newer binary, a plugin outside
/// the minor line of the binary, and a session that loaded an older plugin
/// folder than the one Claude Code installed. A version problem counts only
/// when both the installed plugin and the folder this session runs from have
/// it, so it goes away once either one is current. With nothing installed,
/// the session folder alone is checked.
pub fn plugin_items(probe: &PluginProbe) -> Vec<Item> {
    use crate::plugin_root::{
        binary_too_old, find, is_plain_version, is_plugin_dir, plugin_out_of_range, plugin_version,
    };
    let session = probe.session_root.clone().filter(|r| is_plugin_dir(r));
    let Some(installed) = find(None, &probe.claude_home).or_else(|| session.clone()) else {
        return Vec::new();
    };
    let session = session.filter(|r| *r != installed);
    let roots: Vec<&Path> = std::iter::once(installed.as_path())
        .chain(session.as_deref())
        .collect();
    // Only a plain version reaches the message, so a manifest cannot put
    // other text in front of the user.
    let version = |root: &Path| plugin_version(root).filter(|v| is_plain_version(v));
    let too_old: Vec<Option<String>> = roots
        .iter()
        .map(|r| binary_too_old(r, &probe.binary, probe.homebrew))
        .collect();
    let plugin_key = crate::common::plugin_state::installed_key(&probe.claude_home);
    let range: Vec<Option<String>> = roots
        .iter()
        .map(|r| {
            version(r)
                .and_then(|v| plugin_out_of_range(&v, &probe.binary, &plugin_key, probe.homebrew))
        })
        .collect();
    let mut items = Vec::new();
    if let Some(message) = on_every_root(&too_old) {
        items.push(report(PLUGIN_NEEDS_BINARY, message));
    } else if let Some(message) = on_every_root(&range) {
        items.push(report(PLUGIN_RANGE, message));
    }
    if let (Some(new), Some(old)) = (version(&installed), session.as_deref().and_then(version)) {
        if compare(&old, &new) == Ordering::Less {
            items.push(Item {
                slug: PLUGIN_STALE_SESSION.to_string(),
                kind: Kind::Report,
                what: format!(
                    "This session loaded the playbook plugin {old} from an old folder, and Claude Code has {new} installed"
                ),
                why: why(PLUGIN_STALE_SESSION),
                fix: "exit this session and start Claude Code again".to_string(),
            });
        }
    }
    items
}

/// The first message when every root has one, else `None`.
fn on_every_root(messages: &[Option<String>]) -> Option<&str> {
    if messages.iter().all(Option::is_some) {
        messages.first()?.as_deref()
    } else {
        None
    }
}

fn report(slug: &'static str, message: &str) -> Item {
    let (what, run) = split_command(message);
    Item {
        slug: slug.to_string(),
        kind: Kind::Report,
        what,
        why: why(slug),
        fix: run.unwrap_or_else(|| "playbook doctor".to_string()),
    }
}

/// The migration items and the plugin items as one list, each slug once.
/// The plugin checks read the plugin folders now, so they replace the
/// migration's copy of the range check.
pub fn merge(open: &[OpenItem], plugin: Vec<Item>) -> Vec<Item> {
    let mut items: Vec<Item> = from_open(open)
        .into_iter()
        .filter(|i| i.slug != PLUGIN_RANGE)
        .collect();
    items.extend(plugin);
    items
}

/// The open items of `plan` with the plugin items for `home`.
pub fn from_plan(home: &Path, plan: &[migrate::Planned]) -> Vec<Item> {
    let open: Vec<OpenItem> = plan
        .iter()
        .filter(|p| p.scope != "owned")
        .filter_map(|p| match &p.state {
            migrate::State::Open { message, command } => Some(OpenItem {
                id: p.id,
                message: message.clone(),
                command: *command,
            }),
            _ => None,
        })
        .collect();
    merge(&open, plugin_items(&PluginProbe::for_home(home)))
}

/// The open migration items for `home`, read without writing, without the
/// plugin items. `None` when the migration record cannot be read.
pub fn migration_items_read_only(home: &Path) -> Option<Vec<Item>> {
    let ctx = migrate::Ctx::for_home(home);
    let plan = migrate::plan(&ctx)?;
    Some(
        from_plan(home, &plan)
            .into_iter()
            .filter(|i| !is_plugin_slug(&i.slug))
            .collect(),
    )
}

/// Whether `slug` names a plugin item, which the doctor's version row covers.
pub fn is_plugin_slug(slug: &str) -> bool {
    [PLUGIN_NEEDS_BINARY, PLUGIN_RANGE, PLUGIN_STALE_SESSION].contains(&slug)
}

/// `what` with its first letter in upper case, unless the first word is a
/// key, a path or a name that keeps its case.
fn sentence(what: &str) -> String {
    let first = what.split(' ').next().unwrap_or_default();
    if first.is_empty() || !first.bytes().all(|b| b.is_ascii_lowercase()) {
        return what.to_string();
    }
    let mut chars = what.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// One bullet per item: what, why, and the fix.
pub fn bullets(items: &[Item]) -> String {
    let mut text = String::new();
    for item in items {
        text.push_str(&format!(
            "  - {}.\n    Why: {}.\n    Fix: {}\n",
            sentence(&item.what),
            item.why,
            item.fix
        ));
    }
    text
}

/// The one command for the last line: `playbook setup` when a setup change
/// is open, since it asks about every one; `playbook migrate` when the
/// plugin needs `claude plugin update`, which a run does for you; else the
/// command that shows the list again.
pub fn closing_command(items: &[Item]) -> &'static str {
    if items.iter().any(|i| i.kind == Kind::Consent) {
        "playbook setup"
    } else if items
        .iter()
        .any(|i| i.fix.starts_with(crate::plugin_update::COMMAND_PREFIX))
    {
        "playbook migrate"
    } else {
        "playbook migrate --status"
    }
}

/// The last line, naming [`closing_command`].
pub fn final_line(items: &[Item]) -> String {
    match closing_command(items) {
        "playbook setup" => {
            "To go through every setup change at once, run: playbook setup".to_string()
        }
        "playbook migrate" => {
            "To update the plugin and see what is left, run: playbook migrate".to_string()
        }
        command => format!("To see this list again, run: {command}"),
    }
}

/// How many items need the user, in words.
pub fn count(n: usize) -> String {
    count_still(n, false)
}

/// [`count`], with `still` for a reminder.
fn count_still(n: usize, still: bool) -> String {
    let still = if still { " still" } else { "" };
    if n == 1 {
        format!("1 item{still} needs you")
    } else {
        format!("{n} items{still} need you")
    }
}

/// The full list under a heading, or an empty string for no items.
pub fn render(items: &[Item]) -> String {
    if items.is_empty() {
        return String::new();
    }
    format!(
        "{}:\n{}{}\n",
        count(items.len()),
        bullets(items),
        final_line(items)
    )
}

/// The one short line for items the user already saw.
pub fn compact(items: &[&Item]) -> String {
    let slugs: Vec<&str> = items.iter().map(|i| i.slug.as_str()).collect();
    format!(
        "playbook: {} ({}). To see the list, run: playbook migrate --status",
        count_still(items.len(), true),
        slugs.join(", ")
    )
}

/// What the hook did with an item the last time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Mark {
    /// A hash of what the bullet shows, so a changed item shows in full again.
    fp: String,
    /// When it last showed, in unix seconds.
    shown: u64,
    /// Whether `playbook migrate` printed this report item.
    #[serde(default)]
    acked: bool,
}

/// A stable FNV-1a hash of everything the bullet shows, as hex.
fn fingerprint(item: &Item) -> String {
    let kind = match item.kind {
        Kind::Consent => "consent",
        Kind::Report => "report",
    };
    let mut bytes = Vec::new();
    for field in [item.slug.as_str(), kind, &item.what, &item.fix] {
        bytes.extend_from_slice(field.as_bytes());
        bytes.push(0);
    }
    crate::gate::hash::hash_hex(&bytes)
}

/// The marks in `rows`, by slug. A row that does not parse is left out.
fn parse_marks(rows: Vec<(String, String)>) -> BTreeMap<String, Mark> {
    rows.into_iter()
        .filter_map(|(key, value)| {
            let slug = key.strip_prefix(MARK_PREFIX)?.to_string();
            Some((slug, serde_json::from_str(&value).ok()?))
        })
        .collect()
}

/// Every mark, read without writing. An unreadable store reads as no marks.
fn read_marks(root: &Path) -> BTreeMap<String, Mark> {
    parse_marks(crate::state::list_read_only(root, MARK_PREFIX).unwrap_or_default())
}

/// How the hook shows one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Show {
    /// New or changed: the full bullet.
    Full,
    /// Seen, not settled, and last shown a day ago or more: the short line.
    Compact,
    /// Shown less than a day ago, or a report item `migrate` printed.
    Hide,
}

/// How the hook shows `item` at `now`, given its mark. A mark from the
/// future, after the clock moved back, counts as due.
fn decide(item: &Item, mark: Option<&Mark>, now: u64) -> Show {
    match mark {
        Some(m) if m.fp == fingerprint(item) => {
            if m.acked && item.kind == Kind::Report {
                Show::Hide
            } else if now.saturating_sub(m.shown) >= REMIND_AFTER_SECS || m.shown > now {
                Show::Compact
            } else {
                Show::Hide
            }
        }
        _ => Show::Full,
    }
}

/// Which marks a write may drop because their item is not in its list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prune {
    /// The list holds every open migration item: the heal computed it.
    pub migrations: bool,
    /// The list holds the plugin items for the folder this session runs
    /// from, so a missing stale session item has cleared.
    pub session_plugin: bool,
}

impl Prune {
    /// Drops nothing.
    pub const NONE: Prune = Prune {
        migrations: false,
        session_plugin: false,
    };

    fn allows(self, slug: &str) -> bool {
        match slug {
            PLUGIN_STALE_SESSION => self.session_plugin,
            PLUGIN_NEEDS_BINARY | PLUGIN_RANGE => true,
            _ => self.migrations,
        }
    }
}

/// The session notice for `items` at `now`: the full list of new items and
/// one short line for the ones due again. With `shown` false (a headless
/// session) nothing is marked, so nobody misses an item. Marks of items that
/// cleared are removed as `prune` allows, so an item that comes back shows
/// in full.
pub fn session_notice(
    root: &Path,
    items: &[Item],
    now: u64,
    shown: bool,
    prune: Prune,
) -> Option<String> {
    let marks = read_marks(root);
    let mut full = Vec::new();
    let mut due = Vec::new();
    for item in items {
        match decide(item, marks.get(&item.slug), now) {
            Show::Full => full.push(item),
            Show::Compact => due.push(item),
            Show::Hide => {}
        }
    }
    if shown {
        let showing: Vec<&Item> = full.iter().chain(due.iter()).copied().collect();
        write_marks(root, items, &showing, now, false, prune);
    }
    let mut parts = Vec::new();
    if !full.is_empty() {
        let full: Vec<Item> = full.into_iter().cloned().collect();
        parts.push(format!(
            "playbook {}: {}",
            env!("CARGO_PKG_VERSION"),
            render(&full).trim_end()
        ));
    }
    if !due.is_empty() {
        parts.push(compact(&due));
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

/// Records that `playbook migrate` printed `items` to a person at `now`:
/// report items stop showing in the hook, and setup items wait a day. It
/// drops no mark, since its list may lack items only a session can see.
pub fn acknowledge(root: &Path, items: &[Item], now: u64) {
    let all: Vec<&Item> = items.iter().collect();
    write_marks(root, items, &all, now, true, Prune::NONE);
}

/// Writes the marks of `showing` and drops the marks `prune` allows for
/// items no longer in `items`, in one transaction that reads the marks
/// again, so a mark another process wrote meanwhile is not lost. Best
/// effort: a failed write only means an item may show again.
fn write_marks(root: &Path, items: &[Item], showing: &[&Item], now: u64, ack: bool, prune: Prune) {
    let _ = crate::state::transaction(root, |tx| {
        let marks = parse_marks(tx.list(MARK_PREFIX)?);
        for slug in marks.keys() {
            if prune.allows(slug) && !items.iter().any(|i| &i.slug == slug) {
                tx.delete(&format!("{MARK_PREFIX}{slug}"))?;
            }
        }
        for item in showing {
            let fp = fingerprint(item);
            let kept = marks.get(&item.slug).is_some_and(|m| m.fp == fp && m.acked);
            let mark = Mark {
                fp,
                shown: now,
                acked: kept || (ack && item.kind == Kind::Report),
            };
            let text = serde_json::to_string(&mark).unwrap_or_default();
            tx.set(&format!("{MARK_PREFIX}{}", item.slug), &text)?;
        }
        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::scratch_dir;
    use crate::testing::scratch_playbook_root as root;
    use std::fs;

    fn item(slug: &str, kind: Kind, what: &str) -> Item {
        Item {
            slug: slug.into(),
            kind,
            what: what.into(),
            why: why(slug),
            fix: "playbook setup --only x".into(),
        }
    }

    const ALL: Prune = Prune {
        migrations: true,
        session_plugin: true,
    };

    fn session_notice_all(root: &Path, items: &[Item], now: u64, shown: bool) -> Option<String> {
        session_notice(root, items, now, shown, ALL)
    }

    #[test]
    fn a_setup_item_migrate_printed_waits_a_day_then_comes_back_short() {
        let root = root("pending-ack-consent");
        let items = vec![item("x", Kind::Consent, "a setup change")];
        acknowledge(&root, &items, 1_000);
        assert_eq!(session_notice_all(&root, &items, 1_000 + 3_600, true), None);
        assert!(
            session_notice_all(&root, &items, 1_000 + REMIND_AFTER_SECS, true)
                .is_some_and(|t| t.starts_with("playbook: 1 item still needs you (x)"))
        );
    }

    #[test]
    fn a_mark_from_the_future_counts_as_due() {
        let it = item("x", Kind::Consent, "a");
        let mark = Mark {
            fp: fingerprint(&it),
            shown: 5_000,
            acked: false,
        };
        assert_eq!(decide(&it, Some(&mark), 1_000), Show::Compact);
    }

    #[test]
    fn a_new_fix_command_shows_a_report_in_full_again() {
        let root = root("pending-new-fix");
        let mut it = item("x", Kind::Report, "a");
        acknowledge(&root, std::slice::from_ref(&it), 1_000);
        it.fix = "another command".into();
        assert!(
            session_notice_all(&root, std::slice::from_ref(&it), 2_000, true)
                .is_some_and(|t| t.contains("Fix: another command"))
        );
    }

    #[test]
    fn a_partial_list_keeps_the_marks_it_cannot_judge() {
        // Arrange: a setup item and a stale session item were shown.
        let root = root("pending-partial");
        let setup = item("x", Kind::Consent, "a");
        let stale = item(PLUGIN_STALE_SESSION, Kind::Report, "b");
        session_notice_all(&root, &[setup.clone(), stale.clone()], 1_000, true);

        // Act: migrate prints nothing open, and a skipped heal lists nothing.
        acknowledge(&root, &[], 1_100);
        session_notice(&root, &[], 1_200, true, Prune::NONE);

        // Assert: both marks stay, so neither shows in full again.
        assert_eq!(
            session_notice_all(&root, &[setup, stale], 1_300, true),
            None
        );
    }

    fn plugin(home: &Path, version: &str, min: &str) -> PathBuf {
        let dir = home.join(format!(
            ".claude/plugins/cache/amplifiedengineer/playbook/{version}"
        ));
        fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        fs::create_dir_all(dir.join("skills")).unwrap();
        fs::write(
            dir.join(".claude-plugin/plugin.json"),
            format!(r#"{{"name":"playbook","version":"{version}","metadata":{{"minBinaryVersion":"{min}"}}}}"#),
        )
        .unwrap();
        dir
    }

    fn install(home: &Path, dir: &Path, version: &str) {
        fs::write(
            home.join(".claude/plugins/installed_plugins.json"),
            format!(
                r#"{{"version":2,"plugins":{{"playbook@amplifiedengineer":[{{"scope":"user","installPath":"{}","version":"{version}"}}]}}}}"#,
                dir.display()
            ),
        )
        .unwrap();
    }

    fn probe(home: &Path, session: Option<PathBuf>, binary: &str) -> PluginProbe {
        PluginProbe {
            claude_home: home.join(".claude"),
            session_root: session,
            binary: binary.into(),
            homebrew: false,
        }
    }

    #[test]
    fn each_item_is_one_bullet_with_what_why_and_the_fix() {
        // Arrange
        let open = [
            OpenItem {
                id: migrate::STATUSLINE_MIGRATION,
                message: "statusLine.command in settings.json still runs the old script, so the status line breaks".into(),
                command: Some("playbook setup --statusline"),
            },
            OpenItem {
                id: "other-playbook-plugins",
                message: "installed next to playbook@amplifiedengineer, so hooks or skills run twice: playbook-hooks@pragmatic-engineer. Run: claude plugin uninstall playbook-hooks@pragmatic-engineer".into(),
                command: None,
            },
        ];

        // Act
        let text = render(&from_open(&open));

        // Assert
        assert_eq!(
            text,
            "2 items need you:\n\
             \x20 - statusLine.command in settings.json still runs the old script.\n\
             \x20   Why: the status line still runs a script that newer playbook no longer ships.\n\
             \x20   Fix: playbook setup --statusline\n\
             \x20 - Installed next to playbook@amplifiedengineer: playbook-hooks@pragmatic-engineer.\n\
             \x20   Why: hooks and skills run twice in every session.\n\
             \x20   Fix: claude plugin uninstall playbook-hooks@pragmatic-engineer\n\
             To go through every setup change at once, run: playbook setup\n"
        );
    }

    #[test]
    fn the_final_line_names_setup_then_migrate_then_the_status() {
        let report = item("x", Kind::Report, "a");
        let consent = item("y", Kind::Consent, "b");
        assert!(
            final_line(std::slice::from_ref(&report)).ends_with("run: playbook migrate --status")
        );
        assert!(final_line(&[report.clone(), consent]).ends_with("run: playbook setup"));
        let mut plugin = report;
        plugin.fix = "claude plugin update playbook@amplifiedengineer".into();
        assert!(final_line(&[plugin]).ends_with("run: playbook migrate"));
    }

    #[test]
    fn every_consent_and_report_migration_says_why_it_matters() {
        for m in migrate::registry() {
            if matches!(m.scope, migrate::Scope::Owned) {
                continue;
            }
            assert!(
                reason(m.id).is_some(),
                "{} is a Consent or Report migration: add why it matters to pending::reason",
                m.id
            );
        }
    }

    #[test]
    fn a_problem_only_the_installed_plugin_has_is_not_reported_when_the_session_folder_is_current()
    {
        // Arrange: the installed plugin needs a newer binary, or is a line
        // behind, while this session runs from a current folder.
        let home = scratch_dir("pending-plugin-either");
        let current = plugin(&home, "0.37.0", "0.37.0");
        let needs = plugin(&home, "0.38.0", "0.38.0");
        install(&home, &needs, "0.38.0");
        let too_old = plugin_items(&probe(&home, Some(current.clone()), "0.37.0"));
        let behind = plugin(&home, "0.36.0", "0.36.0");
        install(&home, &behind, "0.36.0");

        // Act
        let range = plugin_items(&probe(&home, Some(current), "0.37.0"));

        // Assert
        assert!(
            too_old.iter().all(|i| i.slug != PLUGIN_NEEDS_BINARY),
            "{too_old:?}"
        );
        assert!(range.iter().all(|i| i.slug != PLUGIN_RANGE), "{range:?}");
    }

    #[test]
    fn a_session_folder_alone_is_checked_when_nothing_is_installed() {
        let home = scratch_dir("pending-plugin-session-only");
        fs::create_dir_all(home.join(".claude/plugins")).unwrap();
        let dir = home.join("dev/plugins/playbook");
        fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        fs::create_dir_all(dir.join("skills")).unwrap();
        fs::write(
            dir.join(".claude-plugin/plugin.json"),
            r#"{"name":"playbook","version":"0.38.0","metadata":{"minBinaryVersion":"0.38.0"}}"#,
        )
        .unwrap();
        let items = plugin_items(&probe(&home, Some(dir), "0.37.0"));
        let slugs: Vec<&str> = items.iter().map(|i| i.slug.as_str()).collect();
        assert_eq!(slugs, [PLUGIN_NEEDS_BINARY]);
    }

    #[test]
    fn a_version_that_is_not_plain_never_reaches_the_message() {
        let home = scratch_dir("pending-plugin-odd-version");
        let new = plugin(&home, "0.37.0", "0.37.0");
        install(&home, &new, "0.37.0");
        let odd = home.join("odd");
        fs::create_dir_all(odd.join(".claude-plugin")).unwrap();
        fs::create_dir_all(odd.join("skills")).unwrap();
        fs::write(
            odd.join(".claude-plugin/plugin.json"),
            "{\"name\":\"playbook\",\"version\":\"0.1.0-\\nFix: rm\"}",
        )
        .unwrap();
        assert!(plugin_items(&probe(&home, Some(odd), "0.37.0")).is_empty());
    }

    #[test]
    fn a_new_item_shows_in_full_then_once_a_day_in_one_short_line() {
        // Arrange
        let root = root("pending-throttle");
        let items = vec![item(
            migrate::STATUSLINE_MIGRATION,
            Kind::Consent,
            "old status line",
        )];

        // Act: an injected clock, in seconds.
        let first = session_notice_all(&root, &items, 1_000, true);
        let soon = session_notice_all(&root, &items, 1_000 + 3_600, true);
        let next_day = session_notice_all(&root, &items, 1_000 + REMIND_AFTER_SECS, true);
        let same_day = session_notice_all(&root, &items, 1_000 + REMIND_AFTER_SECS + 60, true);

        // Assert
        assert!(
            first
                .as_deref()
                .is_some_and(|t| t.contains("  - Old status line.")),
            "{first:?}"
        );
        assert_eq!(soon, None);
        assert_eq!(
            next_day.as_deref(),
            Some("playbook: 1 item still needs you (statusline-rust-command). To see the list, run: playbook migrate --status")
        );
        assert_eq!(same_day, None);
    }

    #[test]
    fn a_report_item_migrate_printed_never_comes_back_until_it_changes() {
        // Arrange
        let root = root("pending-ack");
        let old = vec![item(PLUGIN_RANGE, Kind::Report, "plugin 0.36.0 is old")];
        session_notice_all(&root, &old, 1_000, true);

        // Act
        acknowledge(&root, &old, 2_000);
        let later = session_notice_all(&root, &old, 2_000 + 10 * REMIND_AFTER_SECS, true);
        let changed = vec![item(PLUGIN_RANGE, Kind::Report, "plugin 0.35.0 is old")];
        let new_text = session_notice_all(&root, &changed, 2_000 + 10 * REMIND_AFTER_SECS, true);

        // Assert
        assert_eq!(later, None);
        assert!(new_text.is_some_and(|t| t.contains("Plugin 0.35.0 is old")));
    }

    #[test]
    fn a_cleared_item_disappears_at_once_and_shows_in_full_if_it_comes_back() {
        // Arrange
        let root = root("pending-clear");
        let items = vec![item(
            migrate::STATUSLINE_MIGRATION,
            Kind::Consent,
            "old status line",
        )];
        session_notice_all(&root, &items, 1_000, true);

        // Act
        let cleared = session_notice_all(&root, &[], 1_060, true);
        let back = session_notice_all(&root, &items, 1_120, true);

        // Assert
        assert_eq!(cleared, None);
        assert!(back.is_some_and(|t| t.contains("  - Old status line.")));
    }

    #[test]
    fn a_headless_session_marks_nothing() {
        let root = root("pending-headless");
        let items = vec![item("x", Kind::Consent, "a")];
        assert!(session_notice_all(&root, &items, 1_000, false).is_some());
        assert!(
            session_notice_all(&root, &items, 1_001, true).is_some_and(|t| t.contains("  - A."))
        );
    }

    #[test]
    fn an_old_plugin_is_one_item_with_the_plugin_update_command_and_clears_once_current() {
        // Arrange
        let home = scratch_dir("pending-plugin-old");
        let old = plugin(&home, "0.36.0", "0.36.0");
        install(&home, &old, "0.36.0");

        // Act
        let before = plugin_items(&probe(&home, None, "0.37.0"));
        let new = plugin(&home, "0.37.0", "0.37.0");
        install(&home, &new, "0.37.0");
        let after = plugin_items(&probe(&home, None, "0.37.0"));

        // Assert
        assert_eq!(before.len(), 1, "{before:?}");
        assert_eq!(before[0].slug, PLUGIN_RANGE);
        assert_eq!(
            before[0].fix,
            "claude plugin update playbook@amplifiedengineer"
        );
        assert!(after.is_empty(), "{after:?}");
    }

    #[test]
    fn a_plugin_that_needs_a_newer_binary_is_one_item_with_the_update_command() {
        // Arrange
        let home = scratch_dir("pending-plugin-new");
        let dir = plugin(&home, "0.38.0", "0.38.0");
        install(&home, &dir, "0.38.0");

        // Act
        let items = plugin_items(&probe(&home, None, "0.37.0"));

        // Assert
        let slugs: Vec<&str> = items.iter().map(|i| i.slug.as_str()).collect();
        assert_eq!(slugs, [PLUGIN_NEEDS_BINARY]);
        assert_eq!(items[0].fix, "playbook update");
    }

    #[test]
    fn a_session_from_a_stale_plugin_folder_is_recognised() {
        // Arrange: Claude Code installed 0.37.0, the session still runs 0.36.0.
        let home = scratch_dir("pending-plugin-stale");
        let old = plugin(&home, "0.36.0", "0.36.0");
        let new = plugin(&home, "0.37.0", "0.37.0");
        install(&home, &new, "0.37.0");

        // Act
        let items = plugin_items(&probe(&home, Some(old), "0.37.0"));
        let current = plugin_items(&probe(&home, Some(new), "0.37.0"));

        // Assert
        let slugs: Vec<&str> = items.iter().map(|i| i.slug.as_str()).collect();
        assert_eq!(
            slugs,
            [PLUGIN_STALE_SESSION],
            "the installed plugin is current"
        );
        assert!(items[0].what.contains("0.36.0") && items[0].what.contains("0.37.0"));
        assert!(current.is_empty());
    }

    #[test]
    fn the_plugin_check_replaces_the_migration_copy_of_the_range_item() {
        let open = [OpenItem {
            id: PLUGIN_RANGE,
            message: "old. Run: x".into(),
            command: None,
        }];
        assert!(merge(&open, Vec::new()).is_empty());
    }
}
