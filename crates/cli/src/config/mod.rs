// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Playbook's own tiered (repo < org < global < default) configuration
//! system for keys such as `autoReview.enabled` and `autoReview.type`.
//!
//! Distinct from `common::config_hash` (drift detection on Claude Code's own
//! `settings.json`) and `settings::keys` (the `settings.shared.json` seed
//! allowlist): this module governs playbook's own config keys and their
//! defaults, not Claude Code's.

pub mod keys;
pub mod store;
pub mod write;

use serde_json::Value;
use std::path::{Path, PathBuf};

/// Which tier of the repo/org/global/default chain actually supplied a
/// resolved value, so a caller (or a future `playbook config get` command)
/// can report where a setting came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Repo,
    Org,
    Global,
    Default,
}

impl Source {
    /// The tier name `config get` prints.
    pub fn label(self) -> &'static str {
        match self {
            Source::Repo => "repo",
            Source::Org => "org",
            Source::Global => "global",
            Source::Default => "default",
        }
    }
}

/// Everything that can stop `resolve` or `write::set` from producing or
/// storing a value. A missing tier file is not one of these cases when
/// reading, since it is a normal "no override here" outcome handled by
/// falling through to the next tier; `write::set` treats it the same way,
/// as "nothing to merge into yet".
#[derive(Debug)]
pub enum ConfigError {
    /// The config database is unreadable, or a legacy tier file being
    /// imported is not a JSON object. Holds the path of the offender.
    Malformed(PathBuf),
    /// `key` is not in `keys::KNOWN_KEYS`, so no tier and no default can
    /// ever supply it.
    UnknownKey(String),
    /// `value`'s JSON type does not match what `keys::default_value(key)`
    /// implies for `key`.
    WrongType { key: String, expected: &'static str },
    /// `value` is a string outside `keys::allowed_enum_values(key)`.
    InvalidEnumValue {
        key: String,
        value: String,
        allowed: &'static [&'static str],
    },
    /// `value` is a JSON number but not a non-negative integer (negative,
    /// or fractional such as `3.5`).
    InvalidNumber { key: String, value: String },
    /// `value` is a number outside the range `key` documents, for example a
    /// budget of 0.
    OutOfRange {
        key: String,
        value: String,
        constraint: &'static str,
    },
    /// A spend cap and the session ceiling would be out of order: a set
    /// `auto.sessionCeilingUsd` below `auto.budgetUsd` or
    /// `auto.agentBudgetUsd`. `reason` says which and how to fix it.
    CapOrder {
        key: String,
        value: String,
        reason: String,
    },
    /// A write targeted the org or repo tier but no `repo_slug` was
    /// available to build that tier's path from.
    MissingRepoContext,
    /// A tier's directory could not be created or written into: its parent
    /// path exists as a non-directory, or is not writable.
    DirectoryUnwritable(PathBuf),
    /// `key` is a per-user setting, so only the global tier holds it.
    GlobalOnly(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Malformed(path) => write!(
                f,
                "config file is not a valid JSON object: {}",
                path.display()
            ),
            ConfigError::UnknownKey(key) => write!(
                f,
                "unknown config key: {key}, valid keys are: {}, and effort.<agents|commands|skills>.<name>",
                keys::KNOWN_KEYS.join(", ")
            ),
            ConfigError::WrongType { key, expected } => {
                write!(f, "config key {key} expects a {expected} value")
            }
            ConfigError::InvalidEnumValue {
                key,
                value,
                allowed,
            } => write!(
                f,
                "config key {key} does not accept '{value}', valid values are: {}",
                allowed.join(", ")
            ),
            ConfigError::GlobalOnly(key) => write!(
                f,
                "{key} is a per-user setting; set it with --global"
            ),
            ConfigError::InvalidNumber { key, value } => write!(
                f,
                "config key {key} does not accept '{value}', must be a non-negative integer"
            ),
            ConfigError::OutOfRange {
                key,
                value,
                constraint,
            } => write!(
                f,
                "config key {key} does not accept '{value}', must be {constraint}"
            ),
            ConfigError::CapOrder { key, value, reason } => {
                write!(f, "config key {key} does not accept '{value}': {reason}")
            }
            ConfigError::MissingRepoContext => write!(
                f,
                "could not resolve a repo-scoped config location; repo_slug is unresolved, \
                 refusing to write an org or repo tier config value"
            ),
            ConfigError::DirectoryUnwritable(path) => write!(
                f,
                "config directory could not be created or written into: {}",
                path.display()
            ),
        }
    }
}

/// Adopt a `config.json` that appeared after playbook moved its config into
/// SQLite. Returns one warning line per file that could not be adopted.
/// Files with no conflict are imported silently. Never fails the caller.
pub fn adopt_late_config(home: &Path) -> Vec<String> {
    let root = crate::common::paths::playbook_root_from(home);
    match store::adopt_late_files(&root) {
        Ok(report) => report
            .conflicts
            .iter()
            .map(|path| {
                format!(
                    "{} was written after playbook moved its config into SQLite and is ignored: \
                     it differs from the stored values or is not valid JSON. \
                     Change settings with `playbook config set`, or move the file away.",
                    path.display()
                )
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// An auto spend cap or ceiling in whole cents, the way the spend hook reads
/// it: a number rounded to cents, kept only when it is above 0. `None` means
/// the hook uses the built-in default instead (or, for the ceiling, ten times
/// `auto.budgetUsd`).
pub fn auto_cap_cents(value: &Value) -> Option<i64> {
    value
        .as_f64()
        .filter(|usd| usd.is_finite())
        .map(|usd| (usd * 100.0).round() as i64)
        .filter(|cents| *cents > 0)
}

/// The default of a key that `resolve` already proved is known.
#[allow(
    clippy::expect_used,
    reason = "callers validate the key against KNOWN_KEYS first, so a default always exists"
)]
fn known_default(key: &str) -> Value {
    keys::default_value(key).expect("key was already validated as known")
}

/// Resolve `key` through the repo, org, global, then built-in default tier,
/// in that order, returning the value and which tier supplied it. `home`
/// stands in for `$HOME` so callers (and tests) can point resolution at a
/// scratch directory instead of the real one, matching
/// `common::session::home_dir`'s injection convention. The repo and org
/// tiers are skipped entirely when `repo_slug` is `None`, since there is no
/// `<owner>/<repo>` to build their paths from.
pub fn resolve(
    key: &str,
    home: &Path,
    repo_slug: Option<&str>,
) -> Result<(Value, Source), ConfigError> {
    // Checked up front, not just as the post-tiers fallback: a tier file can
    // legitimately contain an arbitrary dotted path (a stale key from a
    // retired setting, a typo, a future key written by a newer binary), and
    // that must not let an unknown key resolve successfully just because
    // some file happens to have it.
    keys::default_value(key).ok_or_else(|| ConfigError::UnknownKey(key.to_string()))?;
    // A per-user key lives only in the global tier, so a stale repo or org
    // row never shows as the value the binary uses.
    let repo_slug = if keys::is_user_key(key) {
        None
    } else {
        repo_slug
    };

    let root = crate::common::paths::playbook_root_from(home);
    let found = store::lookup(&root, key, repo_slug)?;
    if let Some(value) = found.repo {
        return Ok((value, Source::Repo));
    }
    if let Some(value) = found.org {
        return Ok((value, Source::Org));
    }
    if let Some(value) = found.global {
        return Ok((value, Source::Global));
    }

    // Never `None` here: the top-of-function check already proved `key` is
    // known, so a default value always exists.
    Ok((known_default(key), Source::Default))
}

/// A value `resolve_valid` dropped: the tier that held it and why it was
/// refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoredValue {
    pub tier: Source,
    pub warning: String,
}

/// Keys that once held a boolean and now hold words: the word a stored
/// `true` and a stored `false` mean.
const LEGACY_BOOL_WORDS: &[(&str, &str, &str)] = &[("update.plugin", "auto", "off")];

/// The word a legacy boolean stored under `key` stands for.
fn legacy_word(key: &str, value: &Value) -> Option<&'static str> {
    let flag = value.as_bool()?;
    LEGACY_BOOL_WORDS
        .iter()
        .find(|(k, _, _)| *k == key)
        .map(|(_, on, off)| if flag { *on } else { *off })
}

/// `resolve`, except a string outside the key's allowed enum values (a file
/// edited by hand, since `write::set` refuses it) is dropped in favour of the
/// built-in default, with lower tiers not consulted. The third element
/// describes the dropped value.
pub fn resolve_valid(
    key: &str,
    home: &Path,
    repo_slug: Option<&str>,
) -> Result<(Value, Source, Option<IgnoredValue>), ConfigError> {
    let (value, source) = resolve(key, home, repo_slug)?;
    let value = legacy_word(key, &value).map_or(value, Value::from);
    if let Some(problem) = value
        .as_str()
        .and_then(|s| keys::model_override_error(key, s))
    {
        let default = known_default(key);
        let ignored = IgnoredValue {
            tier: source,
            warning: format!("ignoring invalid value {value} for config key {key}: {problem}"),
        };
        return Ok((default, Source::Default, Some(ignored)));
    }
    if let Some(fixed) = gc_fallback(key, &value, source, home, repo_slug)? {
        return Ok(fixed);
    }
    let Some(allowed) = keys::allowed_enum_values(key) else {
        return Ok((value, source, None));
    };
    if value.as_str().is_some_and(|s| allowed.contains(&s)) {
        return Ok((value, source, None));
    }
    let warning = format!(
        "ignoring invalid value {value} for config key {key}, valid values are: {}",
        allowed.join(", ")
    );
    let default = known_default(key);
    let ignored = IgnoredValue {
        tier: source,
        warning,
    };
    Ok((default, Source::Default, Some(ignored)))
}

/// A resolved value that `resolve_valid` returns, with the tier and any
/// dropped-value note.
type Checked = (Value, Source, Option<IgnoredValue>);

/// For a numeric `gc.*` key whose stored value is outside its range (or, for
/// the pressure key, above the idle key), the default with a warning. `None`
/// when the key is not one of those or the value is fine.
fn gc_fallback(
    key: &str,
    value: &Value,
    source: Source,
    home: &Path,
    repo_slug: Option<&str>,
) -> Result<Option<Checked>, ConfigError> {
    let Some(entry) = keys::gc_number(key) else {
        return Ok(None);
    };
    let mut good = keys::gc_number_or_default(key, value) == value.as_i64();
    let mut above_idle = false;
    if good && key == keys::GC_PRESSURE_KEY {
        let (idle, _) = resolve(keys::GC_PRESSURE_CAP_KEY, home, repo_slug)?;
        let idle = keys::gc_number_or_default(keys::GC_PRESSURE_CAP_KEY, &idle).unwrap_or(0);
        good = value
            .as_i64()
            .is_some_and(|n| keys::gc_pressure_within_idle(n, idle) == n);
        above_idle = !good;
    }
    if good {
        return Ok(None);
    }
    let warning = if above_idle {
        format!(
            "ignoring invalid value {value} for config key {key}, it must not exceed {}",
            keys::GC_PRESSURE_CAP_KEY
        )
    } else {
        format!(
            "ignoring invalid value {value} for config key {key}, valid values are {} to {}",
            entry.min, entry.max
        )
    };
    let ignored = IgnoredValue {
        tier: source,
        warning,
    };
    Ok(Some((known_default(key), Source::Default, Some(ignored))))
}

/// The legacy global tier JSON file, imported into the store on first open.
pub(crate) fn global_config_path(root: &Path) -> PathBuf {
    root.join("config.json")
}

/// Whether any org or repo tier config file exists under `root`. When none
/// does, resolving with or without a slug gives the same answer, so a caller
/// can skip the slug lookup.
pub(crate) fn any_scoped_config(root: &Path) -> bool {
    let subdirs = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default()
    };
    if subdirs(&root.join("orgs"))
        .iter()
        .any(|o| o.join("config.json").exists())
    {
        return true;
    }
    // A slug can have more than two segments (GitLab subgroups), so walk deep.
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.join("repos"), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if dir.file_name().is_some_and(|n| n == ".config") {
            if dir.join("config.json").exists() {
                return true;
            }
            continue;
        }
        if depth < 8 {
            stack.extend(subdirs(&dir).into_iter().map(|d| (d, depth + 1)));
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::write::Tier;
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A fresh scratch directory standing in for `$HOME`, unique per call so
    /// parallel tests never collide.
    fn scratch_home(tag: &str) -> PathBuf {
        let n = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "playbook-config-mod-{}-{tag}-{n}",
            crate::testing::run_id()
        ));
        fs::create_dir_all(&dir).expect("scratch home should be creatable");
        dir
    }

    #[test]
    fn an_invalid_theme_value_in_the_config_file_falls_back_to_the_default_theme() {
        let home = scratch_home("unknown-theme");
        let file = home.join(".config/playbook/config.json");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, r#"{"usage.theme": "solarized"}"#).unwrap();
        let (value, _, ignored) = resolve_valid("usage.theme", &home, None).unwrap();
        assert_eq!(value, Value::String("default".into()));
        assert!(ignored.is_some());
    }

    #[test]
    fn a_stored_boolean_for_update_plugin_reads_as_its_word() {
        // Arrange
        let home = scratch_home("update-plugin-legacy-bool");
        let root = crate::common::paths::playbook_root_from(&home);
        fs::create_dir_all(&root).unwrap();

        for (stored, word) in [(true, "auto"), (false, "off")] {
            store::put(&root, Tier::Global, None, "update.plugin", &json!(stored)).unwrap();

            // Act
            let (value, source, ignored) = resolve_valid("update.plugin", &home, None).unwrap();

            // Assert
            assert_eq!(value, json!(word), "{stored}");
            assert_eq!(source, Source::Global, "{stored}");
            assert!(ignored.is_none(), "{stored}");
        }
    }

    #[test]
    fn scoped_config_is_found_for_every_slug_depth_and_absent_otherwise() {
        // Arrange
        let home = scratch_home("scoped-any");
        let root = home.join("root");
        fs::create_dir_all(root.join("repos/acme/widgets/memory")).unwrap();
        fs::create_dir_all(root.join("orgs")).unwrap();
        let absent = any_scoped_config(&root);
        let cases = [
            "repos/acme/widgets/.config",
            "repos/group/sub/repo/.config",
            "orgs/acme",
        ];

        // Act, Assert
        assert!(!absent, "dirs alone are not config");
        for case in cases {
            let dir = root.join(case);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("config.json"), "{}").unwrap();
            assert!(any_scoped_config(&root), "{case}");
            fs::remove_file(dir.join("config.json")).unwrap();
        }
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn a_gc_number_is_range_checked_on_write() {
        let low = write::validate_key_and_value("gc.graceDays", &json!(0));
        let high = write::validate_key_and_value("gc.graceDays", &json!(366));
        let text = write::validate_key_and_value("gc.graceDays", &json!(1.5));
        let ok = write::validate_key_and_value("gc.graceDays", &json!(30));

        assert!(matches!(low, Err(ConfigError::OutOfRange { .. })));
        assert!(matches!(high, Err(ConfigError::OutOfRange { .. })));
        assert!(matches!(text, Err(ConfigError::OutOfRange { .. })));
        assert!(ok.is_ok());
    }

    #[test]
    fn resolve_valid_drops_an_out_of_range_gc_value_and_a_pressure_above_idle() {
        let home = scratch_home("gc-range");
        let root = crate::common::paths::playbook_root_from(&home);
        let put = |key: &str, value: i64| {
            store::put(&root, write::Tier::Global, None, key, &Value::from(value)).unwrap();
        };
        put("gc.graceDays", 366);
        put("gc.buildOutputIdleDays", 3);
        put("gc.buildOutputPressureIdleDays", 4);

        let grace = resolve_valid("gc.graceDays", &home, None).unwrap();
        let pressure = resolve_valid("gc.buildOutputPressureIdleDays", &home, None).unwrap();

        assert_eq!((grace.0, grace.1), (Value::from(7), Source::Default));
        assert!(grace.2.is_some());
        assert_eq!(pressure.0, Value::from(1));
        let note = pressure.2.as_ref().unwrap();
        assert!(note
            .warning
            .contains("must not exceed gc.buildOutputIdleDays"));
        assert!(grace.2.as_ref().unwrap().warning.contains("1 to"));
        put("gc.buildOutputPressureIdleDays", 3);
        let kept = resolve_valid("gc.buildOutputPressureIdleDays", &home, None).unwrap();
        assert_eq!((kept.0, kept.2), (Value::from(3), None));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn stale_after_days_defaults_to_thirty_with_no_override() {
        // Arrange
        let home = scratch_home("stale-after-days-default");

        // Act
        let result = resolve("worktreeCleanup.staleAfterDays", &home, None);

        // Assert
        assert_eq!(result.unwrap(), (Value::Number(30.into()), Source::Default));

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn stale_after_days_round_trips_through_write_and_resolve() {
        // Arrange
        let home = scratch_home("stale-after-days-round-trip");
        write::set(
            Tier::Repo,
            "worktreeCleanup.staleAfterDays",
            &Value::Number(14.into()),
            &home,
            Some("owner/repo"),
        )
        .expect("write should succeed once staleAfterDays is a known key");

        // Act
        let result = resolve("worktreeCleanup.staleAfterDays", &home, Some("owner/repo"));

        // Assert
        assert_eq!(result.unwrap(), (Value::Number(14.into()), Source::Repo));

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn rejects_invalid_stale_after_days_values() {
        // Each case pairs an invalid value with substrings its rejection
        // error must contain.
        let cases: Vec<(Value, Vec<&str>)> = vec![
            (
                Value::String("not-a-number".to_string()),
                vec!["worktreeCleanup.staleAfterDays", "number"],
            ),
            (
                Value::Number((-5).into()),
                vec!["worktreeCleanup.staleAfterDays", "-5"],
            ),
            (
                Value::Number(serde_json::Number::from_f64(3.5).unwrap()),
                vec!["worktreeCleanup.staleAfterDays", "3.5"],
            ),
        ];

        for (value, expected_substrings) in cases {
            // Arrange
            let home = scratch_home("stale-after-days-rejects");

            // Act
            let result = write::set(
                Tier::Repo,
                "worktreeCleanup.staleAfterDays",
                &value,
                &home,
                Some("owner/repo"),
            );

            // Assert
            let message = result
                .expect_err("invalid value should be rejected")
                .to_string();
            for expected in expected_substrings {
                assert!(
                    message.contains(expected),
                    "expected error message {message:?} to contain {expected:?}"
                );
            }

            let _ = fs::remove_dir_all(&home);
        }
    }

    #[test]
    fn accepts_zero_as_stale_after_days_value() {
        // Arrange
        let home = scratch_home("stale-after-days-accepts-zero");

        // Act
        let result = write::set(
            Tier::Repo,
            "worktreeCleanup.staleAfterDays",
            &Value::Number(0.into()),
            &home,
            Some("owner/repo"),
        );

        // Assert
        assert!(result.is_ok(), "zero should be accepted as non-negative");

        let _ = fs::remove_dir_all(&home);
    }
}
