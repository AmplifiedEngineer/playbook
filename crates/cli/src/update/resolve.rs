// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Which release to install: parse the GitHub releases API, order versions,
//! pick latest, explicit or pre-release, and name the asset for a platform.

use serde_json::Value;
use std::cmp::Ordering;

/// One published release, as far as the updater cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub prerelease: bool,
    /// When GitHub published it, as GitHub wrote it.
    pub published_at: Option<String>,
}

/// The release list request, relative to the API base. One page of 100 is
/// enough to reach past the plugin and telemetry releases that share the
/// list with the binary.
pub const RELEASES_PAGE: &str = "/releases?per_page=100";

impl Release {
    /// The tag without its leading `v`.
    pub fn version(&self) -> &str {
        self.tag.strip_prefix('v').unwrap_or(&self.tag)
    }
}

/// Parses `GET /releases` (an array) or `GET /releases/tags/<tag>` (one
/// object), dropping drafts and anything without a tag.
pub fn parse_releases(body: &str) -> Result<Vec<Release>, String> {
    let value: Value =
        serde_json::from_str(body).map_err(|err| format!("unreadable release data: {err}"))?;
    let items = match value {
        Value::Array(items) => items,
        object @ Value::Object(_) => vec![object],
        _ => return Err("unexpected release data".to_string()),
    };
    Ok(items
        .iter()
        .filter(|item| item["draft"].as_bool() != Some(true))
        .filter_map(|item| {
            Some(Release {
                tag: item["tag_name"].as_str()?.to_string(),
                prerelease: item["prerelease"].as_bool().unwrap_or(false),
                published_at: item["published_at"].as_str().map(str::to_string),
            })
        })
        .collect())
}

impl Release {
    /// Whether this is a published plain `vX.Y.Z` binary release, so not a
    /// pre-release and not a plugin or telemetry tag.
    pub fn is_plain_stable(&self) -> bool {
        let Some(rest) = self.tag.strip_prefix('v') else {
            return false;
        };
        let parts: Vec<&str> = rest.split('.').collect();
        !self.prerelease
            && parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    }
}

/// Picks the release to install. An explicit version matches with or without
/// a leading `v` and ignores `pre`; otherwise the newest stable release wins,
/// or the newest of any kind when `pre` is set.
pub fn select(releases: &[Release], requested: Option<&str>, pre: bool) -> Result<Release, String> {
    if let Some(want) = requested {
        let want = want.strip_prefix('v').unwrap_or(want);
        return releases
            .iter()
            .find(|r| r.version() == want)
            .cloned()
            .ok_or_else(|| format!("no release v{want}; run `playbook update --list`"));
    }
    releases
        .iter()
        .filter(|r| pre || !r.prerelease)
        .max_by(|a, b| compare(a.version(), b.version()))
        .cloned()
        .ok_or_else(|| "no matching release is published".to_string())
}

fn split(version: &str) -> (Vec<u64>, Option<String>) {
    let version = version.strip_prefix('v').unwrap_or(version);
    let version = version.split('+').next().unwrap_or("");
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre.to_string())),
        None => (version, None),
    };
    let nums = core.split('.').map(|n| n.parse().unwrap_or(0)).collect();
    (nums, pre)
}

/// Orders two version strings; a pre-release sorts below its release.
pub fn compare(a: &str, b: &str) -> Ordering {
    let (mut an, ap) = split(a);
    let (mut bn, bp) = split(b);
    let width = an.len().max(bn.len());
    an.resize(width, 0);
    bn.resize(width, 0);
    an.cmp(&bn).then_with(|| match (ap, bp) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => compare_pre(&x, &y),
    })
}

/// Dot-separated identifiers; numeric ones compare as numbers (rc.9 < rc.10).
fn compare_pre(a: &str, b: &str) -> Ordering {
    let mut left = a.split('.');
    let mut right = b.split('.');
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let order = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(m), Ok(n)) => m.cmp(&n),
                    _ => x.cmp(y),
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

fn target_triple(os: &str, arch: &str) -> Result<&'static str, String> {
    match (os, arch) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-musl"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-musl"),
        _ => Err(format!("no release binary for {os} {arch}")),
    }
}

/// The asset a release publishes for this platform, matching install.sh.
pub fn asset_name(version: &str, os: &str, arch: &str) -> Result<String, String> {
    Ok(format!("playbook-{version}-{}", target_triple(os, arch)?))
}

/// The tarball holding the `name` helper for this platform.
pub fn helper_asset_name(
    name: &str,
    version: &str,
    os: &str,
    arch: &str,
) -> Result<String, String> {
    Ok(format!(
        "{name}-{version}-{}.tar.gz",
        target_triple(os, arch)?
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = r#"[
        {"tag_name":"v0.18.0-rc.1","prerelease":true,"draft":false},
        {"tag_name":"v0.17.0","prerelease":false,"draft":false},
        {"tag_name":"v0.19.0","prerelease":false,"draft":true},
        {"tag_name":"v0.16.2","prerelease":false,"draft":false}
    ]"#;

    #[test]
    fn drafts_are_dropped_and_latest_skips_prereleases() {
        let releases = parse_releases(BODY).unwrap();
        assert_eq!(releases.len(), 3);
        assert_eq!(select(&releases, None, false).unwrap().tag, "v0.17.0");
    }

    #[test]
    fn pre_picks_the_newest_of_any_kind() {
        let releases = parse_releases(BODY).unwrap();
        assert_eq!(select(&releases, None, true).unwrap().tag, "v0.18.0-rc.1");
    }

    #[test]
    fn an_explicit_version_matches_with_or_without_v() {
        let releases = parse_releases(BODY).unwrap();
        assert_eq!(
            select(&releases, Some("0.16.2"), false).unwrap().tag,
            "v0.16.2"
        );
        assert_eq!(
            select(&releases, Some("v0.16.2"), false).unwrap().tag,
            "v0.16.2"
        );
        assert!(select(&releases, Some("9.9.9"), false).is_err());
    }

    #[test]
    fn a_single_release_object_parses() {
        let one = r#"{"tag_name":"v0.17.0","prerelease":false}"#;
        assert_eq!(parse_releases(one).unwrap()[0].version(), "0.17.0");
    }

    #[test]
    fn versions_order_numerically_and_prereleases_sort_below() {
        assert_eq!(compare("0.9.0", "0.10.0"), Ordering::Less);
        assert_eq!(compare("0.18.0-rc.1", "0.18.0"), Ordering::Less);
        assert_eq!(compare("v0.17.0", "0.17.0"), Ordering::Equal);
        assert_eq!(compare("0.18.0-rc.10", "0.18.0-rc.9"), Ordering::Greater);
        assert_eq!(compare("0.17", "0.17.0"), Ordering::Equal);
    }

    #[test]
    fn assets_follow_the_installer_naming() {
        assert_eq!(
            asset_name("0.17.0", "linux", "x86_64").unwrap(),
            "playbook-0.17.0-x86_64-unknown-linux-musl"
        );
        assert_eq!(
            asset_name("0.17.0", "macos", "aarch64").unwrap(),
            "playbook-0.17.0-aarch64-apple-darwin"
        );
        assert!(asset_name("0.17.0", "freebsd", "x86_64").is_err());
    }

    #[test]
    fn a_helper_asset_is_a_tarball_for_the_same_triple() {
        for name in ["pbdoc", "pbsync"] {
            assert_eq!(
                helper_asset_name(name, "0.17.0", "macos", "aarch64").unwrap(),
                format!("{name}-0.17.0-aarch64-apple-darwin.tar.gz")
            );
        }
        assert!(helper_asset_name("pbsync", "0.17.0", "freebsd", "x86_64").is_err());
    }
}
