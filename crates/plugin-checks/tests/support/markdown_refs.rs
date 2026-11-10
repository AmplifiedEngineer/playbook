// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Text of a command or skill file together with the reference files it names.
//! A separate file from `markdown.rs`, so a test file that only walks folders
//! does not carry code it never calls.

use std::fs;
use std::path::Path;

/// The `references/<file>.md` paths a file names, from the plugin root. A full
/// `skills/<skill>/references/<file>.md` path names itself; a bare
/// `references/<file>.md` means the folder of `own_skill`, when there is one.
pub(crate) fn named_references(text: &str, own_skill: Option<&str>) -> Vec<String> {
    let full =
        regex::Regex::new(r"skills/([a-z0-9-]+)/references/([A-Za-z0-9_.-]+\.md)").expect("regex");
    let bare = regex::Regex::new(r"references/([A-Za-z0-9_.-]+\.md)").expect("regex");
    let mut out: Vec<String> = full
        .captures_iter(text)
        .map(|c| format!("skills/{}/references/{}", &c[1], &c[2]))
        .collect();
    if let Some(skill) = own_skill {
        let rest = full.replace_all(text, "");
        out.extend(
            bare.captures_iter(&rest)
                .map(|c| format!("skills/{skill}/references/{}", &c[1])),
        );
    }
    out.sort();
    out.dedup();
    out
}

/// The text of the file at `rel` (from the plugin root) followed by the text of
/// every existing reference it names, read through `read`. A check that pins
/// text a slimming PR moves reads this instead of the main file alone.
pub(crate) fn text_with_refs(rel: &str, read: &dyn Fn(&str) -> Option<String>) -> String {
    let mut text = read(rel).unwrap_or_default();
    let own_skill = rel
        .strip_prefix("skills/")
        .and_then(|r| r.split('/').next())
        .filter(|_| rel.ends_with("/SKILL.md"));
    for reference in named_references(&text.clone(), own_skill) {
        if let Some(body) = read(&reference) {
            text.push('\n');
            text.push_str(&body);
        }
    }
    text
}

/// `text_with_refs` over the real plugin folder.
pub(crate) fn command_text_with_refs(rel: &str) -> String {
    let root = Path::new(env!("PLAYBOOK_PLUGIN_ROOT"));
    text_with_refs(rel, &|p| fs::read_to_string(root.join(p)).ok())
}
