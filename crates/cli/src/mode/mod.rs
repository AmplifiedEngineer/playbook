// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook mode`: set the repo's `ask`/`auto` mode and report which mode
//! resolves, from where, and what the guard hook sees.

pub mod context;

use crate::common::mode::{self, Mode, Resolved, Source};
use crate::common::{home_dir, repo_slug};
use crate::config::{self, write};
use serde_json::{json, Value};

fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Ask => "ask",
        Mode::Auto => "auto",
    }
}

fn source_label(source: Source) -> &'static str {
    match source {
        Source::Flag => "flag",
        Source::Env => "env",
        Source::Config => "config",
        Source::Default => "default",
    }
}

/// Write `mode` into the repo tier of the config.
pub fn run_set(mode: Mode) -> Result<String, String> {
    let slug = repo_slug();
    let label = mode_label(mode);
    write::set(
        write::Tier::Repo,
        "mode",
        &Value::String(label.to_string()),
        &home_dir(),
        (!slug.is_empty()).then_some(slug.as_str()),
    )
    .map_err(|err| err.to_string())?;
    Ok(format!("mode set to {label} for this repo"))
}

/// The `mode` config value when a tier sets it, `None` for the built-in
/// default or an unreadable config, which the hook side already reports.
fn configured_mode(slug: &str) -> Option<String> {
    match config::resolve("mode", &home_dir(), (!slug.is_empty()).then_some(slug)) {
        Ok((_, config::Source::Default)) | Err(_) => None,
        Ok((value, _)) => Some(mode::value_text(&value)),
    }
}

/// A flag overrides the mode this command reports but not the one the guard
/// hook resolves from env and config, so a disagreement is worth surfacing.
fn flag_warning(shown: Mode, hook: Mode) -> Option<&'static str> {
    match (shown, hook) {
        (Mode::Auto, Mode::Ask) => Some(
            "--auto only changes this command. PLAYBOOK_MODE and the mode setting say ask, \
             so questions aren't blocked and the spend cap is off. \
             Run `playbook mode auto` to turn them on.",
        ),
        (Mode::Ask, Mode::Auto) => Some(
            "--ask only changes this command. PLAYBOOK_MODE or the mode setting says auto, \
             so the question tool is still blocked.",
        ),
        _ => None,
    }
}

/// The mode `flag`, the environment and the config resolve to, as the
/// `mode` object of `pr finish context`: `mode`, `source` and `warning`
/// (the resolver warnings joined, empty when there are none).
pub fn report(flag: Option<Mode>) -> Value {
    let slug = repo_slug();
    let env = std::env::var("PLAYBOOK_MODE").ok();
    let shown = mode::resolve(flag, env.as_deref(), configured_mode(&slug).as_deref());
    json!({
        "mode": mode_label(shown.mode),
        "source": source_label(shown.source),
        "warning": shown.warnings.join("; "),
    })
}

/// Report the resolved mode, as one line or as a JSON object.
pub fn run_status(flag: Option<Mode>, json: bool) -> String {
    let slug = repo_slug();
    let env = std::env::var("PLAYBOOK_MODE").ok();
    let shown = mode::resolve(flag, env.as_deref(), configured_mode(&slug).as_deref());
    let hook = mode::resolve_for_hook();
    report_resolver_warnings(&shown, &hook);
    let warning = flag_warning(shown.mode, hook.mode);

    if json {
        json!({
            "mode": mode_label(shown.mode),
            "source": source_label(shown.source),
            "hook_mode": mode_label(hook.mode),
            "warning": warning,
        })
        .to_string()
    } else {
        let line = format!(
            "mode: {} (source: {})",
            mode_label(shown.mode),
            source_label(shown.source)
        );
        match warning {
            Some(text) => format!("{line}\nwarning: {text}"),
            None => line,
        }
    }
}

fn report_resolver_warnings(shown: &Resolved, hook: &Resolved) {
    let extra = hook.warnings.iter().filter(|w| !shown.warnings.contains(w));
    for warning in shown.warnings.iter().chain(extra) {
        eprintln!("mode: {warning}");
    }
}
