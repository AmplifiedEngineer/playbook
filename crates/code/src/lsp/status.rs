// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! What `lsp list` and `lsp status` report: each known server, whether it is
//! installed, and what it still needs. Nothing here starts a server in the
//! workspace, so no trust is needed.

use super::discover::{self, BadPath, Found, PROBE_TIMEOUT};
use super::registry::{Prerequisite, ServerSpec, SERVERS};
use crate::host::LspSettings;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// The one command that installs `gopls`; playbook never installs it itself.
pub const GOPLS_INSTALL: &str = "go install golang.org/x/tools/gopls@v0.24.0";

/// Whether the Rust standard library sources are installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RustSrc {
    Present,
    Missing,
    /// `rustup` is not available, so the answer is not known.
    Unknown,
}

/// The machine facts a report reads, injectable for tests.
pub struct Machine {
    pub path_dirs: Vec<PathBuf>,
    pub probe_timeout: Duration,
    pub rust_src: Box<dyn Fn() -> RustSrc>,
}

impl Machine {
    pub fn real() -> Self {
        Self {
            path_dirs: discover::process_path(),
            probe_timeout: PROBE_TIMEOUT,
            rust_src: Box::new(rust_src_from_rustup),
        }
    }
}

/// Asks `rustup` from the temp dir, with auto install off, so a toolchain file
/// in the repo can neither steer the answer nor trigger a download.
fn rust_src_from_rustup() -> RustSrc {
    let mut command = Command::new("rustup");
    command
        .args(["component", "list", "--installed"])
        .current_dir(std::env::temp_dir())
        .env("RUSTUP_AUTO_INSTALL", "0");
    let Some(out) = playbook_common::proc::run_with_timeout(&mut command, PROBE_TIMEOUT)
        .filter(|o| o.status.success())
    else {
        return RustSrc::Unknown;
    };
    let listed = String::from_utf8_lossy(&out.stdout);
    if listed.lines().any(|line| line.starts_with("rust-src")) {
        RustSrc::Present
    } else {
        RustSrc::Missing
    }
}

pub struct ServerStatus {
    pub spec: &'static ServerSpec,
    pub found: Result<Option<Found>, BadPath>,
    /// Prerequisites not met, in words.
    pub missing: Vec<String>,
}

pub fn collect(settings: &LspSettings, machine: &Machine) -> Vec<ServerStatus> {
    SERVERS
        .iter()
        .map(|spec| ServerStatus {
            spec,
            found: discover::find(spec, settings, &machine.path_dirs, machine.probe_timeout),
            missing: missing_prerequisites(spec, machine),
        })
        .collect()
}

fn missing_prerequisites(spec: &ServerSpec, machine: &Machine) -> Vec<String> {
    spec.prerequisites
        .iter()
        .filter_map(|need| match need {
            Prerequisite::Node => on_path("node", machine),
            Prerequisite::Cargo => on_path("cargo", machine),
            Prerequisite::Go => on_path("go", machine),
            Prerequisite::RustSrc => match (machine.rust_src)() {
                RustSrc::Present => None,
                RustSrc::Missing => Some("rust-src (run `rustup component add rust-src`)".into()),
                RustSrc::Unknown => Some("rust-src (could not check: rustup not found)".into()),
            },
        })
        .collect()
}

fn on_path(program: &str, machine: &Machine) -> Option<String> {
    discover::search_path(program, &machine.path_dirs)
        .is_none()
        .then(|| program.to_string())
}

/// What to do when a server is not found.
fn install_hint(spec: &ServerSpec) -> String {
    if spec.id == "gopls" {
        return format!("install it yourself: {GOPLS_INSTALL}");
    }
    format!(
        "put `{}` on PATH or set code.lsp.{}.path",
        spec.executable, spec.id
    )
}

/// The sources a server can come from, in precedence order.
const SOURCES: &str = "config path, PATH";

pub fn list_text() -> String {
    SERVERS
        .iter()
        .map(|spec| {
            format!(
                "{}\n  languages: {}\n  license: {}\n  sources: {SOURCES}\n",
                spec.id,
                spec.languages.join(", "),
                spec.license
            )
        })
        .collect()
}

pub fn list_json() -> Value {
    let servers: Vec<Value> = SERVERS
        .iter()
        .map(|spec| {
            json!({
                "id": spec.id,
                "languages": spec.languages,
                "license": spec.license,
            })
        })
        .collect();
    json!({ "servers": servers })
}

pub fn status_text(settings: &LspSettings, statuses: &[ServerStatus]) -> String {
    let mut out = trust_line(settings);
    for status in statuses {
        out.push_str(&format!("{}\n", status.spec.id));
        match &status.found {
            Ok(Some(found)) => out.push_str(&format!(
                "  found: {} ({}), version {}\n",
                found.path.display(),
                found.source.label(),
                found.version
            )),
            Ok(None) => out.push_str(&format!("  not found; {}\n", install_hint(status.spec))),
            Err(bad) => out.push_str(&format!("  configured path is unusable: {bad}\n")),
        }
        for need in &status.missing {
            out.push_str(&format!("  missing: {need}\n"));
        }
    }
    out
}

fn trust_line(settings: &LspSettings) -> String {
    let tier = settings.trust_tier.as_deref().unwrap_or("default");
    if settings.trust_workspace {
        format!("workspace trust: yes (set at {tier})\n")
    } else {
        format!(
            "workspace trust: no ({tier}); servers do not start here until \
             `playbook config set code.lsp.trustWorkspace true`\n"
        )
    }
}

pub fn status_json(settings: &LspSettings, statuses: &[ServerStatus]) -> Value {
    let servers: Vec<Value> = statuses.iter().map(server_json).collect();
    json!({
        "trust": {
            "workspace": settings.trust_workspace,
            "tier": settings.trust_tier.as_deref().unwrap_or("default"),
        },
        "servers": servers,
    })
}

fn server_json(status: &ServerStatus) -> Value {
    let mut doc = json!({
        "id": status.spec.id,
        "languages": status.spec.languages,
        "license": status.spec.license,
        "missing": status.missing,
    });
    match &status.found {
        Ok(Some(found)) => {
            doc["found"] = json!(true);
            doc["source"] = json!(found.source.label());
            doc["path"] = json!(found.path.display().to_string());
            doc["version"] = json!(found.version);
        }
        Ok(None) => {
            doc["found"] = json!(false);
            doc["hint"] = json!(install_hint(status.spec));
        }
        Err(bad) => {
            doc["found"] = json!(false);
            doc["error"] = json!(bad.to_string());
        }
    }
    doc
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(tag: &str) -> PathBuf {
        let dir = playbook_common::test_support::scratch_dir(tag);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn script(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn machine(path_dirs: Vec<PathBuf>, rust_src: RustSrc) -> Machine {
        Machine {
            path_dirs,
            probe_timeout: Duration::from_secs(10),
            rust_src: Box::new(move || rust_src),
        }
    }

    fn find<'a>(statuses: &'a [ServerStatus], id: &str) -> &'a ServerStatus {
        statuses.iter().find(|s| s.spec.id == id).unwrap()
    }

    #[test]
    fn an_empty_path_reports_all_four_servers_not_found() {
        // Arrange
        let machine = machine(vec![scratch("status-empty")], RustSrc::Unknown);

        // Act
        let statuses = collect(&LspSettings::default(), &machine);

        // Assert
        assert_eq!(statuses.len(), 4);
        assert!(statuses.iter().all(|s| matches!(s.found, Ok(None))));
        let text = status_text(&LspSettings::default(), &statuses);
        assert_eq!(text.matches("  not found;").count(), 4, "{text}");
    }

    #[test]
    fn prerequisites_are_reported_per_server() {
        // Arrange
        let dir = scratch("status-prereq");
        script(&dir, "node", "exit 0");
        let machine = machine(vec![dir], RustSrc::Missing);

        // Act
        let statuses = collect(&LspSettings::default(), &machine);

        // Assert
        assert!(find(&statuses, "typescript-language-server")
            .missing
            .is_empty());
        let rust = &find(&statuses, "rust-analyzer").missing;
        assert!(
            rust[0].starts_with("cargo") && rust[1].starts_with("rust-src"),
            "{rust:?}"
        );
        assert_eq!(find(&statuses, "gopls").missing, ["go"]);
    }

    #[test]
    fn rust_src_present_is_not_reported_and_unknown_says_why() {
        for (rust_src, expected) in [
            (RustSrc::Present, None),
            (RustSrc::Unknown, Some("could not check")),
        ] {
            // Arrange
            let dir = scratch("status-rustsrc");
            script(&dir, "cargo", "exit 0");

            // Act
            let statuses = collect(&LspSettings::default(), &machine(vec![dir], rust_src));

            // Assert
            let missing = &find(&statuses, "rust-analyzer").missing;
            match expected {
                None => assert!(missing.is_empty(), "{missing:?}"),
                Some(text) => assert!(
                    missing.len() == 1 && missing[0].contains(text),
                    "{missing:?}"
                ),
            }
        }
    }

    #[test]
    fn a_configured_server_shows_its_source_and_version() {
        // Arrange
        let dir = scratch("status-config");
        let fake = script(&dir, "ra", "echo rust-analyzer 9.9");
        let settings = LspSettings {
            server_paths: vec![("rust-analyzer".into(), fake.clone())],
            ..LspSettings::default()
        };

        // Act
        let statuses = collect(&settings, &machine(vec![], RustSrc::Present));
        let json = status_json(&settings, &statuses);

        // Assert
        let ra = json["servers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == "rust-analyzer")
            .unwrap();
        assert_eq!(ra["source"], "config");
        assert_eq!(ra["version"], "rust-analyzer 9.9");
        assert_eq!(ra["path"], fake.display().to_string());
    }

    #[test]
    fn gopls_not_found_prints_the_go_install_line() {
        // Arrange
        let statuses = collect(&LspSettings::default(), &machine(vec![], RustSrc::Unknown));

        // Act
        let text = status_text(&LspSettings::default(), &statuses);

        // Assert
        assert!(text.contains(GOPLS_INSTALL), "{text}");
    }

    #[test]
    fn the_trust_line_names_the_state_and_the_tier() {
        // Arrange
        let trusted = LspSettings {
            trust_workspace: true,
            trust_tier: Some("repo".into()),
            ..LspSettings::default()
        };

        // Act
        let yes = trust_line(&trusted);
        let no = trust_line(&LspSettings::default());

        // Assert
        assert!(yes.contains("yes") && yes.contains("repo"), "{yes}");
        assert!(
            no.contains("no") && no.contains("code.lsp.trustWorkspace"),
            "{no}"
        );
    }

    #[test]
    fn list_names_every_server_with_languages_and_license() {
        // Act
        let text = list_text();
        let json = list_json();

        // Assert
        for spec in &SERVERS {
            assert!(
                text.contains(spec.id) && text.contains(spec.license),
                "{}",
                spec.id
            );
        }
        assert_eq!(json["servers"].as_array().unwrap().len(), 4);
    }
}
