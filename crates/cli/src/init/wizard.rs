// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The guided `playbook setup` in a terminal: one screen per asked part with
//! what happens on yes and on no, then a summary and a confirm. The screens
//! are built from the part catalog, so a new part shows up here without a
//! change to this file. Nothing is written until the confirm.

use crate::init::catalog::{self, Part};
use crate::init::choices::Flags;
use crate::init::run::{StepReport, StepStatus};
use crate::init::wire::{wired_hooks, Group};
use crate::ui::render::{self, Choice, Counts, Status};
use crate::ui::wizard::{Item, MAX_ITEMS};
use crate::ui::Caps;

/// What a flag already decided for a part, by catalog id.
pub fn flag_for(flags: &Flags, id: &str) -> Option<bool> {
    let pair = crate::init::choices::part_pair(flags, id)?;
    match pair {
        (true, _) => Some(true),
        (_, true) => Some(false),
        _ => None,
    }
}

fn hook_summary() -> String {
    let mut parts = Vec::new();
    for g in Group::ALL {
        let n = wired_hooks().filter(|h| h.group == g).count();
        if n > 0 {
            parts.push(format!("{n} {}", g.label().to_lowercase()));
        }
    }
    parts.join(", ")
}

/// Exact text the part adds, beyond its description.
fn extra_lines(id: &str) -> Vec<String> {
    match id {
        "hooks" => vec![format!(
            "Hooks added: {}. Run `playbook hooks list` to read each one.",
            hook_summary()
        )],
        "statusline" => vec![
            "Sets statusLine.command to `playbook statusline` (model, git branch, PR and cost)."
                .to_string(),
        ],
        "path" => vec![
            "Adds a block that starts with `# playbook binary` and one `export PATH=...` line."
                .to_string(),
        ],
        "telemetry" => {
            crate::init::telemetry::plan_lines(&crate::common::home_dir().join(".claude"))
        }
        "short-name" => vec![
            "Adds the same link install.sh makes: pb -> playbook, in the playbook install folder."
                .to_string(),
        ],
        "completions" => vec![
            "Each loader file starts with `# playbook completions (written by playbook setup)`."
                .to_string(),
        ],
        "policy-guard" => vec![
            "Turn it off later with `playbook config set --global hooks.policyGuard.enabled false`."
                .to_string(),
        ],
        "attribution" => {
            vec!["Claude Code reads the key in every session it starts after this.".to_string()]
        }
        "codex-hooks" => vec![
            "Open /hooks in Codex afterwards and trust the two entries, or Codex skips them."
                .to_string(),
        ],
        _ => Vec::new(),
    }
}

/// What happens when the part is declined. `/playbook:setup` shows the same
/// line, and a test keeps the two in step.
pub fn decline_line(id: &str) -> &'static str {
    match id {
        "hooks" => "No hooks are added. The safety guards, session helpers and usage warm-up do not run.",
        "statusline" => "Your status line stays as it is.",
        "path" => "Your shell files are not touched. Hooks may not find playbook unless it is already on PATH.",
        "system-prompt" => "No file is copied, and `systemPrompt.enabled` is set to false, so `pb cc` starts with Claude Code's own prompt.",
        "telemetry" => "No variables are written. `playbook usage` reads transcripts instead.",
        "short-name" => "No `pb` link is added, and an existing one stays. Use `playbook` in full where `pb` is missing.",
        "completions" => "No completion files are written, and your shell files are not touched. Tab does not complete `playbook` or `pb` until you run `playbook setup --only completions`.",
        "policy-guard" => "The policy guard setting is not changed. It is off by default, and its hook entry allows every call while off. A guard you turned on stays on until `hooks.policyGuard.enabled` is set to false. Turn it on later with `playbook config set --global hooks.policyGuard.enabled true`.",
        "attribution" => "Attribution is OFF, the default: playbook adds the missing `attribution` keys to ~/.claude/settings.json and keeps your own values, so Claude Code adds no trailer, generated-by line or session link.",
        "codex-hooks" => "~/.codex/hooks.json is not touched. Entries from an earlier run stay until `playbook uninstall`, and without them the playbook guards do not run in Codex. Register them later with `playbook setup --only codex-hooks`.",
        _ => "Nothing changes for this part.",
    }
}

fn title_of(p: &Part) -> String {
    let mut chars = p.id.replace('-', " ");
    if let Some(first) = chars.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    chars
}

/// The screen for one part.
pub fn choice_for(p: &Part) -> Choice {
    let mut accept = vec![p.does.to_string()];
    accept.extend(extra_lines(p.id));
    accept.push(format!("Edits: {}", p.changes));
    accept.push(format!("Undo: {}", p.undo));
    Choice {
        id: p.id.to_string(),
        title: title_of(p),
        explain: p.question.to_string(),
        accept,
        decline: vec![decline_line(p.id).to_string()],
        recommended: p.default_on,
        question: "Do this?".to_string(),
    }
}

/// One item per asked part, in catalog order. `security_on` is whether the
/// security part will run anyway, from its flag or `security.defaults`. It is
/// never asked about, so it appears as a fixed row in the summary.
pub fn items(flags: &Flags, security_on: bool) -> Vec<Item> {
    let mut list: Vec<Item> = catalog::PARTS
        .iter()
        .filter(|p| p.asked)
        .take(MAX_ITEMS)
        .map(|p| Item {
            choice: choice_for(p),
            default: p.default_on,
            fixed: flag_for(flags, p.id),
        })
        .collect();
    if let (true, Some(p)) = (security_on, catalog::part("security")) {
        list.push(Item {
            choice: choice_for(p),
            default: true,
            fixed: Some(true),
        });
    }
    list
}

/// [`items`] without the questions of the parts in `skip`, each a part id
/// whose question setup replaced with one line. The parts in `off` start as
/// no, because the user said no before, and the parts in `on` start as yes,
/// because the user chose it before.
pub fn items_for(
    flags: &Flags,
    security_on: bool,
    skip: &[&str],
    off: &[&str],
    on: &[&str],
) -> Vec<Item> {
    let mut list = items(flags, security_on);
    list.retain(|i| !skip.contains(&i.choice.id.as_str()));
    for item in &mut list {
        let id = item.choice.id.as_str();
        if off.contains(&id) {
            item.default = false;
        } else if on.contains(&id) {
            item.default = true;
        }
    }
    list
}

/// The wizard's answer for the part a question belongs to, else `default`.
pub fn guided_answer(guided: &[(&str, bool)], question: &str, default: bool) -> bool {
    catalog::PARTS
        .iter()
        .find(|p| p.question == question)
        .and_then(|p| guided.iter().find(|(id, _)| *id == p.id))
        .map_or(default, |(_, on)| *on)
}

/// The words after the on/off mark in the summary.
pub fn summary_detail(item: &Item, on: bool) -> String {
    let base = match (item.choice.id == crate::init::attribution::PART, on) {
        (true, true) => crate::init::attribution::ON_LABEL,
        (true, false) => crate::init::attribution::OFF_LABEL,
        (false, true) => "will be applied",
        (false, false) => "left as it is",
    };
    if item.fixed.is_some() {
        format!("{base} (set by a flag)")
    } else {
        base.to_string()
    }
}

/// The text that opens the guided run, also shown by `/playbook:setup`.
pub const WELCOME: &str = "This walks through each change playbook can make to your machine. Nothing is written until you confirm at the end. playbook never changes Claude Code's memory, and it edits settings.json only for the parts you accept.";

/// What opens the guided run.
pub fn welcome(caps: &Caps) -> String {
    let mut out = render::heading(caps, "playbook setup");
    out.push_str(&render::wrap(WELCOME, caps.width, 0));
    out.push_str("\n\nPress Enter to take the answer in capitals. Keys: y, n, b (back), s (default), q (quit).\n");
    out
}

/// The status a step report maps to in the progress phase.
pub fn status_of(step: &StepReport) -> Status {
    match step.status {
        StepStatus::Wired => Status::Done,
        StepStatus::AlreadyCorrect => Status::Unchanged,
        StepStatus::Skipped => Status::Skipped,
        StepStatus::Failed => Status::Failed,
    }
}

/// The progress phase: one line per step, the totals and what to do next.
pub fn progress(caps: &Caps, steps: &[StepReport]) -> String {
    let mut out = render::heading(caps, "Applying");
    let mut statuses = Vec::new();
    for s in steps {
        let st = status_of(s);
        statuses.push(st);
        out.push_str(&render::progress_line(caps, st, s.name, &s.detail));
        out.push('\n');
    }
    let counts = Counts::tally(&statuses);
    let mut next: Vec<&str> = vec![
        "Open a new terminal so a PATH change loads.",
        "Run `playbook doctor check` to check the result.",
        "Run `playbook uninstall --dry-run` to see how to undo everything.",
    ];
    if counts.failed > 0 {
        next.insert(
            0,
            "Fix the failed lines above, then run `playbook setup` again. It is safe to repeat.",
        );
    }
    out.push_str(&render::next_steps(caps, counts, &next));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_asked_part_has_a_flag_and_a_screen() {
        for p in catalog::PARTS.iter().filter(|p| p.asked) {
            let flags = Flags::default();
            assert!(flag_for(&flags, p.id).is_none());
            let c = choice_for(p);
            assert!(c.accept.len() >= 3, "{}", p.id);
            assert!(!c.decline[0].is_empty());
        }
        // Each asked part reacts to its own on flag.
        let on = Flags {
            hooks: true,
            statusline: true,
            path: true,
            system_prompt: true,
            telemetry: true,
            short_name: true,
            completions: true,
            policy_guard: true,
            attribution: true,
            codex_hooks: true,
            ..Flags::default()
        };
        for p in catalog::PARTS.iter().filter(|p| p.asked) {
            assert_eq!(flag_for(&on, p.id), Some(true), "{}", p.id);
        }
    }

    #[test]
    fn an_off_flag_fixes_the_item_to_no() {
        let flags = Flags {
            no_short_name: true,
            ..Flags::default()
        };
        let list = items(&flags, false);
        let a = list.iter().find(|i| i.choice.id == "short-name");
        assert_eq!(a.map(|i| i.fixed), Some(Some(false)));
        assert_eq!(
            a.map(|i| summary_detail(i, false)),
            Some("left as it is (set by a flag)".to_string())
        );
    }

    #[test]
    fn the_hooks_screen_counts_hooks_by_group() {
        let list = items(&Flags::default(), false);
        let h = list.iter().find(|i| i.choice.id == "hooks");
        let shown = h.map(|i| i.choice.accept.join("\n")).unwrap_or_default();
        assert!(shown.contains("safety guards"), "{shown}");
    }

    #[test]
    fn progress_maps_every_status_and_counts() {
        let steps = vec![
            StepReport::wired("hooks", "29 entries added"),
            StepReport::already_correct("path", "already set"),
            StepReport::skipped("short-name", "you chose off"),
            StepReport::failed("trust", "cannot write"),
        ];
        let got = progress(&Caps::plain(80), &steps);
        assert!(got.contains("[ok] hooks  29 entries added\n"));
        assert!(got.contains("[=] path  already set\n"));
        assert!(got.contains("[skip] short-name  you chose off\n"));
        assert!(got.contains("[fail] trust  cannot write\n"));
        assert!(got.contains("1 done, 1 unchanged, 1 skipped, 1 failed"));
        assert!(got.contains("Fix the failed lines"));
    }

    #[test]
    fn security_shows_as_a_fixed_row_only_when_it_will_run() {
        let flags = Flags::default();
        assert!(!items(&flags, false)
            .iter()
            .any(|i| i.choice.id == "security"));
        let on = items(&flags, true);
        let sec = on.iter().find(|i| i.choice.id == "security");
        assert_eq!(sec.map(|i| i.fixed), Some(Some(true)));
    }

    #[test]
    fn every_asked_part_flips_when_answered_against_its_default() {
        use crate::init::choices::resolve;
        let guided: Vec<(&str, bool)> = catalog::PARTS
            .iter()
            .filter(|p| p.asked)
            .map(|p| (p.id, !p.default_on))
            .collect();
        let c = resolve(&Flags::default(), true, false, &mut |q, d| {
            guided_answer(&guided, q, d)
        });
        assert!(!c.hooks && !c.statusline && !c.path);
        assert!(!c.system_prompt && !c.short_name);
        assert!(
            !c.telemetry,
            "telemetry is on by default, so a no flips it off"
        );
        assert_eq!(
            c.attribution,
            crate::init::attribution::Choice::On,
            "attribution is OFF by default, so a yes turns it ON"
        );
    }

    #[test]
    fn the_telemetry_question_is_left_out_when_it_is_skipped() {
        let flags = Flags::default();

        let skipped = items_for(&flags, false, &["telemetry"], &[], &[]);
        let asked = items_for(&flags, false, &[], &["system-prompt"], &["attribution"]);
        let default =
            |list: &[Item], id: &str| list.iter().find(|i| i.choice.id == id).map(|i| i.default);

        assert!(!skipped.iter().any(|i| i.choice.id == "telemetry"));
        assert!(asked.iter().any(|i| i.choice.id == "telemetry"));
        assert!(!asked.iter().any(|i| i.choice.id == "collector-service"));
        assert_eq!(skipped.len() + 1, asked.len());
        assert_eq!(default(&skipped, "attribution"), Some(false));
        assert_eq!(default(&asked, "attribution"), Some(true));
        assert_eq!(default(&asked, "system-prompt"), Some(false));
        let a = skipped
            .iter()
            .find(|i| i.choice.id == "attribution")
            .unwrap();
        assert_eq!(
            summary_detail(a, true),
            "ON: Claude Code adds its attribution"
        );
        assert_eq!(
            summary_detail(a, false),
            "OFF (default): Claude Code adds no attribution"
        );
    }

    #[test]
    fn the_welcome_promises_no_change_before_confirm() {
        let got = welcome(&Caps::plain(80))
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(got.contains("Nothing is written until you confirm"));
        assert!(got.contains("never changes Claude Code's memory"));
    }
}
