// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Per-component effort ceilings (ADR-0017). A component is an agent, a
//! command or a skill, and its ceiling comes from `effort.<kind>.<name>`.
//!
//! The ceiling for a component is the lowest of Claude Code's own
//! `maxEffortLevel`, playbook's `maxEffortLevel` and the component's key. The
//! effective effort is the lower of the shipped effort and that ceiling, so a
//! ceiling never raises anything. For an agent, `resolve` also names the file
//! to dispatch, because an agent's effort is fixed by its file.

use super::{lower, rank};
use crate::agents::variants::{variant_name, MARKER, VARIANTS};
use crate::config;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

/// What a component is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Agent,
    Command,
    Skill,
}

impl Kind {
    /// Parse `agents`, `commands` or `skills` (singular forms work too).
    pub fn parse(text: &str) -> Option<Kind> {
        match text {
            "agent" | "agents" => Some(Kind::Agent),
            "command" | "commands" => Some(Kind::Command),
            "skill" | "skills" => Some(Kind::Skill),
            _ => None,
        }
    }

    /// The plural word used in config keys and plugin directories.
    pub fn plural(self) -> &'static str {
        match self {
            Kind::Agent => "agents",
            Kind::Command => "commands",
            Kind::Skill => "skills",
        }
    }

    /// The component's source file under the plugin root.
    fn file(self, root: &Path, name: &str) -> std::path::PathBuf {
        match self {
            Kind::Agent => root.join("agents").join(format!("{name}.md")),
            Kind::Command => root.join("commands").join(format!("{name}.md")),
            Kind::Skill => root.join("skills").join(name).join("SKILL.md"),
        }
    }
}

/// The config key that holds a component's ceiling.
fn config_key(kind: Kind, name: &str) -> String {
    format!("effort.{}.{name}", kind.plural())
}

/// The `effort:` value in a file's frontmatter, if any.
fn shipped_effort(path: &Path) -> Option<String> {
    frontmatter_value(path, "effort:")
}

/// The `model:` value in a file's frontmatter, if any.
fn shipped_model(path: &Path) -> Option<String> {
    frontmatter_value(path, "model:")
}

fn frontmatter_value(path: &Path, key: &str) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    if lines.next()? != "---" {
        return None;
    }
    for line in lines {
        if line == "---" {
            break;
        }
        if let Some(rest) = line.strip_prefix(key) {
            let value = rest.trim().trim_matches(['"', '\'']);
            return (!value.is_empty()).then(|| value.to_string());
        }
    }
    None
}

/// The variants this session can dispatch, named by the launcher in this
/// variable (comma separated). A session started without the launcher has none.
pub const VARIANTS_ENV: &str = "PLAYBOOK_AGENT_VARIANTS";

/// Set by the launcher to the plugin name when the variants are agents of a
/// throwaway plugin, so their `subagent_type` carries that prefix.
pub const VARIANTS_PLUGIN_ENV: &str = "PLAYBOOK_AGENT_VARIANTS_PLUGIN";

/// The agent variants a session can dispatch: their names, and the plugin
/// that holds them, which prefixes their `subagent_type`.
#[derive(Clone, Copy)]
struct Variants<'a> {
    names: &'a [String],
    plugin: Option<&'a str>,
}

fn prefixed(file: &str, plugin: Option<&str>) -> String {
    match plugin {
        Some(plugin) if !plugin.is_empty() => format!("{plugin}:{file}"),
        _ => file.to_string(),
    }
}

/// The variant names in `VARIANTS_ENV`.
fn available_variants() -> Vec<String> {
    std::env::var(VARIANTS_ENV)
        .unwrap_or_default()
        .split(',')
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .collect()
}

/// Everything known about one component's effort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub kind: Kind,
    pub name: String,
    /// Whether the component's source file exists under the plugin root.
    pub known: bool,
    pub shipped: Option<String>,
    /// The `model:` in the component's frontmatter.
    pub model: Option<String>,
    /// What that model allows for this component (see `model_cap`).
    pub model_cap: Option<String>,
    /// The component's own key, `auto` when unset.
    pub configured: String,
    /// playbook's `maxEffortLevel`.
    pub global: String,
    pub claude: Option<String>,
    /// The lowest ceiling in play, `None` when nothing limits.
    pub ceiling: Option<String>,
    /// Shipped effort lowered by the ceiling.
    pub effective: Option<String>,
    /// For an agent: the name to dispatch by default.
    pub file: Option<String>,
    /// For an agent: the `subagent_type` to pass the `Agent` tool for `file`.
    pub subagent_type: Option<String>,
    /// For an agent: every name at or below the ceiling, lowest first.
    pub allowed: Vec<String>,
    /// False when the ceiling is below every agent the session has, so the
    /// lowest one is returned and still exceeds it.
    pub satisfied: bool,
    /// The plugin holding the session's variants, which prefixes their
    /// `subagent_type`. `None` when they are plain agents.
    pub variants_plugin: Option<String>,
}

fn level_of(home: &Path, key: &str) -> String {
    match config::resolve_valid(key, home, None) {
        Ok((Value::String(level), _, _)) => level,
        _ => "auto".to_string(),
    }
}

/// `auto` and `max` set no ceiling.
fn as_ceiling(level: &str) -> Option<&str> {
    (level != "auto" && level != "max").then_some(level)
}

/// The agents with their efforts, lowest first: the base and each variant
/// the session has (`available`, from the launcher).
fn agent_files(name: &str, shipped: &str, available: &[String]) -> Vec<(String, String)> {
    let mut files = vec![(name.to_string(), shipped.to_string())];
    if VARIANTS.iter().any(|(base, _)| *base == name) {
        for tier in crate::agents::variants::TIERS {
            let variant = variant_name(name, tier);
            if tier != shipped && available.contains(&variant) {
                files.push((variant, tier.to_string()));
            }
        }
    }
    files.sort_by_key(|(_, effort)| rank(effort).unwrap_or(usize::MAX));
    files
}

/// Resolve one component. `root` is the plugin root holding `agents/`,
/// `commands/` and `skills/`; `None` leaves `known` false and `shipped` empty.
pub fn resolve(
    kind: Kind,
    name: &str,
    home: &Path,
    claude: Option<&str>,
    root: Option<&Path>,
) -> Resolution {
    resolve_capped(kind, name, home, claude, root, None)
}

/// `resolve` with an extra ceiling `cap` from the calling command, such as
/// `quick-review` holding its reviewer to `medium`. The lowest of the cap and
/// the user's ceilings applies, and a cap never raises anything.
pub fn resolve_capped(
    kind: Kind,
    name: &str,
    home: &Path,
    claude: Option<&str>,
    root: Option<&Path>,
    cap: Option<&str>,
) -> Resolution {
    let names = available_variants();
    let plugin = std::env::var(VARIANTS_PLUGIN_ENV).ok();
    let variants = Variants {
        names: &names,
        plugin: plugin.as_deref(),
    };
    resolve_inner(kind, name, home, claude, root, variants, cap)
}

/// `resolve` with the session's variants given instead of read from the
/// environment. They are plain agents, with no plugin prefix.
pub fn resolve_with(
    kind: Kind,
    name: &str,
    home: &Path,
    claude: Option<&str>,
    root: Option<&Path>,
    available: &[String],
) -> Resolution {
    let variants = Variants {
        names: available,
        plugin: None,
    };
    resolve_inner(kind, name, home, claude, root, variants, None)
}

fn resolve_inner(
    kind: Kind,
    name: &str,
    home: &Path,
    claude: Option<&str>,
    root: Option<&Path>,
    variants: Variants,
    cap: Option<&str>,
) -> Resolution {
    let path = root.map(|r| kind.file(r, name));
    let known = path.as_deref().is_some_and(Path::is_file);
    let shipped = path.as_deref().and_then(shipped_effort);
    let model = path.as_deref().and_then(shipped_model);
    let model_cap = super::model_cap::cap_for(model.as_deref(), name);
    let configured = level_of(home, &config_key(kind, name));
    let global = level_of(home, super::KEY);

    let ceiling = lower(
        lower(
            lower(lower(claude, as_ceiling(&global)), as_ceiling(&configured)),
            cap.and_then(as_ceiling),
        ),
        model_cap,
    )
    .map(str::to_string);
    let effective = match (shipped.as_deref(), ceiling.as_deref()) {
        (Some(s), Some(c)) => lower(Some(s), Some(c)).map(str::to_string),
        (Some(s), None) => Some(s.to_string()),
        (None, _) => None,
    };

    let (mut file, mut allowed, mut satisfied) = (None, Vec::new(), true);
    let mut subagent_type = None;
    if kind == Kind::Agent {
        if let Some(base_effort) = shipped.as_deref() {
            let files = agent_files(name, base_effort, variants.names);
            let limit = ceiling.as_deref().and_then(rank);
            allowed = files
                .iter()
                .filter(|(_, e)| limit.is_none_or(|l| rank(e).is_some_and(|r| r <= l)))
                .map(|(f, _)| f.clone())
                .collect();
            // Never above the shipped effort, so a ceiling above it picks the base.
            let want = rank(effective.as_deref().unwrap_or(base_effort));
            let pick = files
                .iter()
                .rev()
                .find(|(_, e)| rank(e) <= want)
                .or_else(|| files.first());
            if let Some((f, e)) = pick {
                satisfied = limit.is_none_or(|l| rank(e).is_some_and(|r| r <= l));
                subagent_type = Some(if f == name {
                    format!("playbook:{f}")
                } else {
                    prefixed(f, variants.plugin)
                });
                file = Some(f.clone());
            }
            if allowed.is_empty() {
                allowed = file.iter().cloned().collect();
            }
        }
    }

    Resolution {
        kind,
        name: name.to_string(),
        known,
        shipped,
        model,
        model_cap: model_cap.map(str::to_string),
        configured,
        global,
        claude: claude.map(str::to_string),
        ceiling,
        effective,
        file,
        subagent_type,
        allowed,
        satisfied,
        variants_plugin: variants.plugin.map(str::to_string),
    }
}

impl Resolution {
    pub fn to_json(&self) -> Value {
        json!({
            "kind": self.kind.plural(),
            "name": self.name,
            "known": self.known,
            "shipped": self.shipped,
            "model": self.model,
            "modelCap": self.model_cap,
            "configured": self.configured,
            "global": self.global,
            "claudeCode": self.claude,
            "ceiling": self.ceiling,
            "effective": self.effective,
            "effort": self.effective,
            "file": self.file,
            "subagentType": self.subagent_type,
            "allowed": self.allowed,
            "allowedTypes": self.allowed_types(),
            "satisfied": self.satisfied,
        })
    }

    /// `allowed` as the `subagent_type` values to pass the `Agent` tool.
    pub fn allowed_types(&self) -> Vec<String> {
        self.allowed
            .iter()
            .map(|f| {
                if *f == self.name {
                    format!("playbook:{f}")
                } else {
                    prefixed(f, self.variants_plugin.as_deref())
                }
            })
            .collect()
    }

    pub fn to_text(&self) -> String {
        let show = |v: &Option<String>| v.clone().unwrap_or_else(|| "none".to_string());
        let mut out = format!(
            "{} {}: shipped {}, ceiling {}, effective {}",
            self.kind.plural(),
            self.name,
            show(&self.shipped),
            show(&self.ceiling),
            show(&self.effective),
        );
        if let Some(file) = &self.file {
            let ty = self.subagent_type.as_deref().unwrap_or(file);
            out.push_str(&format!("\ndispatch: {ty}"));
            if !self.satisfied {
                out.push_str(
                    " (no variant is low enough for the ceiling, or the session has none)",
                );
            }
        }
        if !self.known {
            out.push_str("\nwarning: no such component in the plugin files");
        }
        out
    }
}

/// Every component under `root`, base agents only (generated variants are
/// left out), sorted by kind then name.
pub fn components(root: &Path) -> Vec<(Kind, String)> {
    let mut found = Vec::new();
    let md_stems = |dir: &Path, skip_generated: bool| -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let path = e.path();
                if path.extension()? != "md" {
                    return None;
                }
                if skip_generated && fs::read_to_string(&path).is_ok_and(|t| t.contains(MARKER)) {
                    return None;
                }
                Some(path.file_stem()?.to_string_lossy().into_owned())
            })
            .collect();
        names.sort();
        names
    };
    for name in md_stems(&root.join("agents"), true) {
        found.push((Kind::Agent, name));
    }
    for name in md_stems(&root.join("commands"), false) {
        found.push((Kind::Command, name));
    }
    let mut skills: Vec<String> = fs::read_dir(root.join("skills"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("SKILL.md").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    skills.sort();
    for name in skills {
        found.push((Kind::Skill, name));
    }
    found
}

/// Component keys set in the global config that match no component under
/// `root`, as `effort.<kind>.<name>`.
fn stale_keys(home: &Path, root: &Path) -> Vec<String> {
    let store_root = crate::common::paths::playbook_root_from(home);
    let Ok(keys) = config::store::global_keys(&store_root) else {
        return Vec::new();
    };
    let mut stale = Vec::new();
    for key in keys {
        let mut parts = key.splitn(3, '.');
        let (Some("effort"), Some(kind_name), Some(name)) =
            (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let Some(kind) = Kind::parse(kind_name) else {
            continue;
        };
        if !kind.file(root, name).is_file() {
            stale.push(config_key(kind, name));
        }
    }
    stale.sort();
    stale
}

/// `playbook effort list`: every component with its shipped effort, ceiling
/// and effective level.
pub fn run_list(home: &Path, claude: Option<&str>, root: Option<&Path>, json: bool) -> String {
    let Some(root) = root else {
        return "no plugin root found: run `playbook path plugin-root` to see what was tried"
            .to_string();
    };
    let rows: Vec<Resolution> = components(root)
        .into_iter()
        .map(|(kind, name)| resolve(kind, &name, home, claude, Some(root)))
        .collect();
    let stale = stale_keys(home, root);
    if json {
        return json!({
            "components": rows.iter().map(Resolution::to_json).collect::<Vec<_>>(),
            "unknownKeys": stale,
        })
        .to_string();
    }
    let dash = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".to_string());
    let mut out =
        String::from("component                         shipped  set     effective  model cap\n");
    for r in &rows {
        out.push_str(&format!(
            "{:<33} {:<8} {:<7} {:<10} {}\n",
            format!("{}/{}", r.kind.plural(), r.name),
            dash(&r.shipped),
            r.configured,
            dash(&r.effective),
            dash(&r.model_cap),
        ));
    }
    out.push_str(&format!(
        "model cap: Haiku never runs at max, stops at {} unless a role opts in (git: xhigh)\n",
        super::model_cap::HAIKU_DEFAULT_CAP
    ));
    for key in &stale {
        out.push_str(&format!("warning: {key} matches no component\n"));
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_variant_type_carries_the_session_plugin_prefix_only_when_there_is_one() {
        assert_eq!(
            prefixed("reviewer-low", Some("playbook-variants")),
            "playbook-variants:reviewer-low"
        );
        assert_eq!(prefixed("reviewer-low", None), "reviewer-low");
        assert_eq!(prefixed("reviewer-low", Some("")), "reviewer-low");
    }
    use crate::common::test_support::scratch_dir;
    use crate::config::write::{self, Tier};

    fn plugin(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let base = scratch_dir(tag);
        let root = base.join("plugin");
        let home = base.join("home");
        for d in ["agents", "commands", "skills/writing"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        fs::create_dir_all(&home).unwrap();
        let agent =
            |effort: &str| format!("---\nname: x\ndescription: d\neffort: {effort}\n---\nBody\n");
        fs::write(root.join("agents/reviewer.md"), agent("high")).unwrap();
        fs::write(root.join("agents/git.md"), agent("low")).unwrap();
        fs::write(root.join("commands/deep-review.md"), agent("high")).unwrap();
        fs::write(
            root.join("skills/writing/SKILL.md"),
            "---\nname: w\n---\nBody\n",
        )
        .unwrap();
        (home, root)
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn set(home: &Path, key: &str, value: &str) {
        write::set(Tier::Global, key, &Value::String(value.into()), home, None).unwrap();
    }

    #[test]
    fn a_haiku_agent_gets_the_model_cap_as_a_ceiling() {
        let (home, root) = plugin("cmp-haiku-cap");
        fs::write(
            root.join("agents/cheap.md"),
            "---\nname: c\ndescription: d\nmodel: haiku\neffort: low\n---\nBody\n",
        )
        .unwrap();
        let avail = names(&["cheap-medium", "cheap-high", "cheap-xhigh"]);
        let r = resolve_with(Kind::Agent, "cheap", &home, None, Some(&root), &avail);
        assert_eq!(r.model.as_deref(), Some("haiku"));
        assert_eq!(r.model_cap.as_deref(), Some("medium"));
        assert_eq!(r.ceiling.as_deref(), Some("medium"));
    }

    #[test]
    fn the_git_agent_may_reach_xhigh_on_haiku() {
        let (home, root) = plugin("cmp-git-optin");
        fs::write(
            root.join("agents/git.md"),
            "---\nname: g\ndescription: d\nmodel: haiku\neffort: low\n---\nBody\n",
        )
        .unwrap();
        let r = resolve(Kind::Agent, "git", &home, None, Some(&root));
        assert_eq!(r.model_cap.as_deref(), Some("xhigh"));
        set(&home, "maxEffortLevel", "medium");
        let r = resolve(Kind::Agent, "git", &home, None, Some(&root));
        assert_eq!(
            r.ceiling.as_deref(),
            Some("medium"),
            "the user ceiling still wins"
        );
    }

    #[test]
    fn a_component_ceiling_lowers_the_effective_effort() {
        let (home, root) = plugin("cmp-lower");
        set(&home, "effort.commands.deep-review", "medium");
        let r = resolve(Kind::Command, "deep-review", &home, None, Some(&root));
        assert_eq!(r.shipped.as_deref(), Some("high"));
        assert_eq!(r.ceiling.as_deref(), Some("medium"));
        assert_eq!(r.effective.as_deref(), Some("medium"));
    }

    #[test]
    fn a_ceiling_never_raises_the_shipped_effort() {
        let (home, root) = plugin("cmp-never-raise");
        set(&home, "effort.agents.git", "xhigh");
        let r = resolve_with(Kind::Agent, "git", &home, None, Some(&root), &[]);
        assert_eq!(r.effective.as_deref(), Some("low"));
        assert_eq!(r.file.as_deref(), Some("git"));
    }

    #[test]
    fn claude_code_is_the_top_ceiling_and_auto_adds_none() {
        let (home, root) = plugin("cmp-claude");
        set(&home, "effort.commands.deep-review", "xhigh");
        let r = resolve(
            Kind::Command,
            "deep-review",
            &home,
            Some("low"),
            Some(&root),
        );
        assert_eq!(r.ceiling.as_deref(), Some("low"));
        assert_eq!(r.effective.as_deref(), Some("low"));
        let auto = resolve(Kind::Command, "deep-review", &home, None, Some(&root));
        set(&home, "effort.commands.deep-review", "auto");
        let auto2 = resolve(Kind::Command, "deep-review", &home, None, Some(&root));
        assert_eq!(auto2.ceiling, None);
        assert_eq!(auto2.effective.as_deref(), Some("high"));
        assert_eq!(auto.ceiling.as_deref(), Some("xhigh"));
    }

    #[test]
    fn the_global_ceiling_applies_to_a_component_without_its_own() {
        let (home, root) = plugin("cmp-global");
        write::set(
            Tier::Global,
            "maxEffortLevel",
            &Value::String("medium".into()),
            &home,
            None,
        )
        .unwrap();
        let r = resolve(Kind::Command, "deep-review", &home, None, Some(&root));
        assert_eq!(r.effective.as_deref(), Some("medium"));
    }

    #[test]
    fn the_spawn_effort_is_the_effective_level_for_every_agent() {
        let (home, root) = plugin("cmp-spawn-effort");
        write::set(
            Tier::Global,
            "maxEffortLevel",
            &Value::String("medium".into()),
            &home,
            None,
        )
        .unwrap();
        for (kind, name) in components(&root) {
            if kind != Kind::Agent {
                continue;
            }
            let json = resolve(kind, &name, &home, None, Some(&root)).to_json();
            let level = json["effort"].as_str();
            assert_eq!(level, json["effective"].as_str(), "{name}");
            assert!(
                level.is_none_or(|l| rank(l) <= rank("medium")),
                "{name} spawns above the ceiling"
            );
        }
    }

    #[test]
    fn with_no_ceiling_the_spawn_effort_is_the_shipped_effort() {
        let (home, root) = plugin("cmp-spawn-shipped");
        let json = resolve(Kind::Agent, "reviewer", &home, None, Some(&root)).to_json();
        assert_eq!(json["effort"].as_str(), json["shipped"].as_str());
    }

    #[test]
    fn a_command_cap_picks_the_variant_at_or_below_it() {
        let (home, root) = plugin("cmp-cap");
        let have = vec!["reviewer-low".to_string(), "reviewer-medium".to_string()];
        let r = resolve_inner(
            Kind::Agent,
            "reviewer",
            &home,
            None,
            Some(&root),
            Variants {
                names: &have,
                plugin: None,
            },
            Some("medium"),
        );
        assert_eq!(r.subagent_type.as_deref(), Some("reviewer-medium"));
        assert_eq!(r.file.as_deref(), Some("reviewer-medium"));
        assert!(r.satisfied);
        // No variants in the session: the base agent runs and says it is above the cap.
        let none = resolve_inner(
            Kind::Agent,
            "reviewer",
            &home,
            None,
            Some(&root),
            Variants {
                names: &[],
                plugin: None,
            },
            Some("medium"),
        );
        assert_eq!(none.subagent_type.as_deref(), Some("playbook:reviewer"));
        assert!(!none.satisfied);
        // A lower user ceiling still wins over the cap.
        let low = resolve_inner(
            Kind::Agent,
            "reviewer",
            &home,
            Some("low"),
            Some(&root),
            Variants {
                names: &have,
                plugin: None,
            },
            Some("medium"),
        );
        assert_eq!(low.subagent_type.as_deref(), Some("reviewer-low"));
        // A cap above the shipped effort changes nothing.
        let high = resolve_inner(
            Kind::Agent,
            "reviewer",
            &home,
            None,
            Some(&root),
            Variants {
                names: &have,
                plugin: None,
            },
            Some("max"),
        );
        assert_eq!(high.subagent_type.as_deref(), Some("playbook:reviewer"));
    }

    #[test]
    fn variants_of_a_session_plugin_carry_its_prefix_and_the_base_does_not() {
        let (home, root) = plugin("cmp-prefix");
        let have = names(&["reviewer-low", "reviewer-medium"]);
        let variants = Variants {
            names: &have,
            plugin: Some("playbook-variants"),
        };
        let r = resolve_inner(
            Kind::Agent,
            "reviewer",
            &home,
            None,
            Some(&root),
            variants,
            Some("medium"),
        );
        assert_eq!(
            r.subagent_type.as_deref(),
            Some("playbook-variants:reviewer-medium")
        );
        assert_eq!(
            r.allowed_types(),
            [
                "playbook-variants:reviewer-low",
                "playbook-variants:reviewer-medium"
            ]
        );
        let base = resolve_inner(
            Kind::Agent,
            "reviewer",
            &home,
            None,
            Some(&root),
            variants,
            None,
        );
        assert_eq!(base.subagent_type.as_deref(), Some("playbook:reviewer"));
        assert!(base
            .allowed_types()
            .contains(&"playbook:reviewer".to_string()));
    }

    #[test]
    fn an_agent_resolves_to_the_variant_at_or_below_the_ceiling() {
        let (home, root) = plugin("cmp-variant");
        set(&home, "effort.agents.reviewer", "low");
        let have = names(&["reviewer-low", "reviewer-medium", "reviewer-xhigh"]);
        let r = resolve_with(Kind::Agent, "reviewer", &home, None, Some(&root), &have);
        assert_eq!(r.file.as_deref(), Some("reviewer-low"));
        assert_eq!(r.subagent_type.as_deref(), Some("reviewer-low"));
        assert_eq!(r.allowed, vec!["reviewer-low"]);
        assert!(r.satisfied);
    }

    #[test]
    fn with_no_ceiling_an_agent_keeps_its_base_and_lists_every_variant() {
        let (home, root) = plugin("cmp-base");
        let have = names(&["reviewer-low", "reviewer-medium", "reviewer-xhigh"]);
        let r = resolve_with(Kind::Agent, "reviewer", &home, None, Some(&root), &have);
        assert_eq!(r.file.as_deref(), Some("reviewer"));
        assert_eq!(r.subagent_type.as_deref(), Some("playbook:reviewer"));
        assert!(r.allowed.contains(&"reviewer-low".to_string()));
        assert!(r.allowed.contains(&"reviewer-xhigh".to_string()));
    }

    #[test]
    fn a_session_without_the_launcher_has_only_the_base_agent() {
        let (home, root) = plugin("cmp-no-launcher");
        set(&home, "effort.agents.reviewer", "low");
        let r = resolve_with(Kind::Agent, "reviewer", &home, None, Some(&root), &[]);
        assert_eq!(r.file.as_deref(), Some("reviewer"));
        assert!(!r.satisfied);
    }

    #[test]
    fn an_agent_with_no_variant_low_enough_says_it_cannot_comply() {
        let (home, root) = plugin("cmp-unsatisfied");
        fs::write(
            root.join("agents/critic.md"),
            "---\nname: critic\ndescription: d\neffort: high\n---\n",
        )
        .unwrap();
        set(&home, "effort.agents.critic", "low");
        let r = resolve_with(Kind::Agent, "critic", &home, None, Some(&root), &[]);
        assert_eq!(r.file.as_deref(), Some("critic"));
        assert!(!r.satisfied);
        assert!(r.to_text().contains("no variant is low enough"));
    }

    #[test]
    fn a_skill_with_no_effort_has_no_effective_level() {
        let (home, root) = plugin("cmp-skill");
        let r = resolve(Kind::Skill, "writing", &home, None, Some(&root));
        assert!(r.known);
        assert_eq!(r.shipped, None);
        assert_eq!(r.effective, None);
    }

    #[test]
    fn an_unknown_component_is_reported_not_rejected() {
        let (home, root) = plugin("cmp-unknown");
        set(&home, "effort.agents.ghost", "low");
        let r = resolve_with(Kind::Agent, "ghost", &home, None, Some(&root), &[]);
        assert!(!r.known);
        assert!(r.to_text().contains("no such component"));
        assert_eq!(stale_keys(&home, &root), vec!["effort.agents.ghost"]);
    }

    #[test]
    fn list_shows_every_kind() {
        let (home, root) = plugin("cmp-list");
        let names: Vec<String> = components(&root)
            .into_iter()
            .map(|(k, n)| format!("{}/{n}", k.plural()))
            .collect();
        assert_eq!(
            names,
            vec![
                "agents/git",
                "agents/reviewer",
                "commands/deep-review",
                "skills/writing"
            ]
        );
        let text = run_list(&home, None, Some(&root), false);
        assert!(text.contains("agents/reviewer"), "{text}");
    }
}
