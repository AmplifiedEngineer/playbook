// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `code.*` settings, resolved into what `playbook code` needs. This
//! crate owns the config store, so `playbook-code` receives plain values.

use super::{resolve, Source};
use playbook_code::host::LspSettings;
use playbook_code::lsp::registry::SERVERS;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Resolves the language server settings for the repo `repo_slug` names. An
/// unreadable value counts as unset, and the trust default is false, so a
/// broken config never turns a server on.
pub fn lsp_settings(home: &Path, repo_slug: Option<&str>) -> LspSettings {
    let text = |key: &str| match resolve(key, home, repo_slug) {
        Ok((Value::String(s), _)) if !s.is_empty() => Some(PathBuf::from(s)),
        _ => None,
    };
    // An org row is refused on write, but a hand edited store could hold one.
    let trust_tier = match resolve("code.lsp.trustWorkspace", home, repo_slug) {
        Ok((Value::Bool(true), source @ (Source::Repo | Source::Global))) => Some(source.label()),
        _ => None,
    };
    let server_paths = SERVERS
        .iter()
        .filter_map(|spec| {
            let path = text(&format!("code.lsp.{}.path", spec.id))?;
            Some((spec.id.to_string(), path))
        })
        .collect();
    LspSettings {
        trust_workspace: trust_tier.is_some(),
        trust_tier: trust_tier.map(str::to_string),
        server_paths,
        python_interpreter: text("code.python.interpreter"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::write::{set, Tier};
    use serde_json::json;

    fn home(tag: &str) -> PathBuf {
        let dir = playbook_common::test_support::scratch_dir(tag);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn nothing_set_is_untrusted_with_no_paths() {
        // Arrange
        let home = home("code-default");

        // Act
        let got = lsp_settings(&home, Some("acme/app"));

        // Assert
        assert_eq!(got, LspSettings::default());
    }

    #[test]
    fn set_values_reach_the_settings() {
        // Arrange
        let home = home("code-set");
        let slug = Some("acme/app");
        set(
            Tier::Repo,
            "code.lsp.trustWorkspace",
            &json!(true),
            &home,
            slug,
        )
        .unwrap();
        set(
            Tier::Global,
            "code.lsp.gopls.path",
            &json!("/opt/gopls"),
            &home,
            None,
        )
        .unwrap();
        set(
            Tier::Global,
            "code.python.interpreter",
            &json!("/opt/py"),
            &home,
            None,
        )
        .unwrap();

        // Act
        let got = lsp_settings(&home, slug);
        let elsewhere = lsp_settings(&home, Some("acme/other"));

        // Assert
        assert!(got.trust_workspace);
        assert_eq!(got.trust_tier.as_deref(), Some("repo"));
        assert_eq!(got.server_path("gopls"), Some(Path::new("/opt/gopls")));
        assert_eq!(got.python_interpreter, Some(PathBuf::from("/opt/py")));
        assert!(!elsewhere.trust_workspace, "trust is per repo");
    }
}
