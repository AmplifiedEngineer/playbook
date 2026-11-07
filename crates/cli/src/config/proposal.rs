// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `playbook config apply-proposal`: shows the config that `pbsync config
//! pull` proposed and, after a yes, stores it in the global tier.

use super::sync_files::{value_text, write_sync_files};
use super::write::{self, Tier};
use super::{keys, parse, store};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::Path;

/// The proposal file `pbsync config pull` writes in the data folder.
pub const PROPOSAL_FILE: &str = "config-proposal.json";
const NO_PROPOSAL: &str = "No sync proposal to apply.";

#[derive(Deserialize)]
struct Proposal {
    proposed: BTreeMap<String, String>,
    #[serde(default)]
    from: BTreeMap<String, String>,
}

/// A proposed key that passed every check.
struct Change {
    key: String,
    value: Value,
    old: Option<String>,
    from: String,
}

/// The streams `run` talks to, so tests and the binary share one code path.
pub struct Streams<'a> {
    pub input: &'a mut dyn BufRead,
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
}

/// Run the command and return its exit code. Nothing is stored before the
/// answer is yes, and the proposal file goes only after every key is stored.
pub fn run(
    home: &Path,
    repo_slug: Option<&str>,
    yes: bool,
    io: &mut Streams<'_>,
) -> std::io::Result<i32> {
    let path = crate::usage::data::data_dir(home).join(PROPOSAL_FILE);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            writeln!(io.out, "{NO_PROPOSAL}")?;
            return Ok(0);
        }
        Err(err) => {
            writeln!(
                io.err,
                "config apply-proposal: cannot read {}: {err}",
                path.display()
            )?;
            return Ok(1);
        }
    };
    let proposal: Proposal = match serde_json::from_str(&raw) {
        Ok(proposal) => proposal,
        Err(err) => {
            writeln!(
                io.err,
                "config apply-proposal: {} is not a valid proposal: {err}",
                path.display()
            )?;
            return Ok(1);
        }
    };
    let root = crate::common::paths::playbook_root_from(home);
    let (changes, rejected) = match classify(&proposal, &root) {
        Ok(split) => split,
        Err(err) => {
            writeln!(io.err, "config apply-proposal: {err}")?;
            return Ok(1);
        }
    };
    for (key, reason) in &rejected {
        writeln!(io.out, "rejected {key}: {reason}")?;
    }
    if changes.is_empty() {
        return Ok(0);
    }
    for change in &changes {
        let old = change.old.as_deref().unwrap_or("(unset)");
        let new = value_text(&change.value);
        writeln!(
            io.out,
            "{}: {old} -> {new} (from {})",
            change.key, change.from
        )?;
    }
    if !yes && !confirm(io, changes.len())? {
        writeln!(io.err, "config apply-proposal: nothing was changed")?;
        return Ok(1);
    }
    for change in &changes {
        if let Err(err) = write::set(Tier::Global, &change.key, &change.value, home, None) {
            writeln!(io.err, "config apply-proposal: {err}")?;
            return Ok(1);
        }
    }
    if let Err(err) = write_sync_files(home) {
        writeln!(
            io.err,
            "config apply-proposal: warning: sync files not refreshed: {err}"
        )?;
    }
    std::fs::remove_file(&path)?;
    writeln!(
        io.out,
        "config apply-proposal: applied {} setting(s)",
        changes.len()
    )?;
    note_shadowed(io.out, &root, repo_slug, &changes)?;
    Ok(0)
}

fn confirm(io: &mut Streams<'_>, count: usize) -> std::io::Result<bool> {
    write!(
        io.out,
        "Apply {count} setting(s) to your global config? [y/N] "
    )?;
    io.out.flush()?;
    let mut answer = String::new();
    io.input.read_line(&mut answer)?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// The valid changes, and the rejected keys with the reason for each.
type Classified = (Vec<Change>, Vec<(String, String)>);

/// Split the proposal into valid changes and rejected keys with the reason.
fn classify(proposal: &Proposal, root: &Path) -> Result<Classified, super::ConfigError> {
    let mut changes = Vec::new();
    let mut rejected = Vec::new();
    for (key, text) in &proposal.proposed {
        match check(key, text) {
            Ok(value) => changes.push(Change {
                key: key.clone(),
                value,
                old: store::lookup(root, key, None)?
                    .global
                    .as_ref()
                    .map(value_text),
                from: proposal.from.get(key).cloned().unwrap_or_default(),
            }),
            Err(reason) => rejected.push((key.clone(), reason)),
        }
    }
    Ok((changes, rejected))
}

/// The value a proposed key would store, or why it must not be stored.
fn check(key: &str, text: &str) -> Result<Value, String> {
    if !keys::is_shared(key) {
        return Err("not a key that syncs between machines".to_string());
    }
    let value = parse::parse_config_value(key, text).map_err(|err| err.to_string())?;
    write::validate_key_and_value(key, &value).map_err(|err| err.to_string())?;
    Ok(value)
}

/// One line when a project or org value still wins over an applied key.
fn note_shadowed(
    out: &mut dyn Write,
    root: &Path,
    repo_slug: Option<&str>,
    changes: &[Change],
) -> std::io::Result<()> {
    let Some(slug) = repo_slug else {
        return Ok(());
    };
    let shadowed: Vec<&str> = changes
        .iter()
        .filter(|change| {
            store::lookup(root, &change.key, Some(slug))
                .is_ok_and(|found| found.repo.is_some() || found.org.is_some())
        })
        .map(|change| change.key.as_str())
        .collect();
    if shadowed.is_empty() {
        return Ok(());
    }
    writeln!(
        out,
        "note: project settings override the applied value for {}.",
        shadowed.join(", ")
    )
}
