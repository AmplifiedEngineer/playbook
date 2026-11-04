// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The language server releases playbook may install, with the integrity value
//! of each download compiled in.
//!
//! To bump a pin: download the asset twice from separate networks, compare it
//! with the digest the host publishes (the GitHub release asset digest, or the
//! npm `integrity` field), and commit the value, the version and the release
//! date. Never take a hash from the download host at install time: a host that
//! serves a bad file would serve its hash too. A pin younger than two weeks is
//! not taken, which leaves time for a bad release to be pulled.

use std::fmt;

/// How a download is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hash {
    /// Lower case hex SHA-256 of the file.
    Sha256(&'static str),
    /// An npm `integrity` value: `sha512-` and the base64 digest.
    NpmSha512(&'static str),
}

impl Hash {
    pub fn value(self) -> &'static str {
        match self {
            Self::Sha256(v) | Self::NpmSha512(v) => v,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    /// One gzip compressed executable.
    Gz,
    /// A gzip compressed tar of files.
    Tgz,
}

#[derive(Debug)]
pub struct Asset {
    /// A target triple, or `any` for a platform independent package.
    pub target: &'static str,
    pub url: &'static str,
    pub hash: Hash,
    pub archive: ArchiveKind,
    /// Entries in the archive when pinned; 0 for a single file.
    pub entries: usize,
}

/// A release date as year, month, day.
pub type Date = (i32, u32, u32);

#[derive(Debug)]
pub struct Release {
    /// The package or program name.
    pub component: &'static str,
    pub version: &'static str,
    pub released: Date,
    pub license: &'static str,
    pub assets: &'static [Asset],
}

/// Everything installed for one server. The first release is the server.
#[derive(Debug)]
pub struct Pin {
    pub server_id: &'static str,
    pub releases: &'static [Release],
}

/// One `rust-analyzer` asset; the URL is built from the target.
macro_rules! rust_analyzer_asset {
    ($target:literal, $sha256:literal) => {
        Asset {
            target: $target,
            url: concat!(
                "https://github.com/rust-lang/rust-analyzer/releases/download/2026-10-05/rust-analyzer-",
                $target,
                ".gz"
            ),
            hash: Hash::Sha256($sha256),
            archive: ArchiveKind::Gz,
            entries: 0,
        }
    };
}

const RUST_ANALYZER: [Asset; 5] = [
    rust_analyzer_asset!(
        "aarch64-apple-darwin",
        "64d3f6b8aacd94a93a03298d7988dfa0c747f8a4fcc539a4e59d9c9f920ca62f"
    ),
    rust_analyzer_asset!(
        "x86_64-apple-darwin",
        "32400dfe2b1472fff9689e063dbb80b8c90e0d4f883af49de7abbba3f2e20705"
    ),
    rust_analyzer_asset!(
        "aarch64-unknown-linux-gnu",
        "3974c863acbbd96ef2cc02a7aaa79123935b05f27ffe4fd393fc229522f75cb9"
    ),
    rust_analyzer_asset!(
        "x86_64-unknown-linux-gnu",
        "28070188df63b6f217768040781decc8db43bc9d29b126847acb365575b09bc9"
    ),
    rust_analyzer_asset!(
        "x86_64-unknown-linux-musl",
        "c7f8b193d3b226fd69c899c3b991a961dd9afebbe53cefca0595e089ed5285de"
    ),
];

const TYPESCRIPT_LANGUAGE_SERVER: [Asset; 1] = [Asset {
    target: "any",
    url: "https://registry.npmjs.org/typescript-language-server/-/typescript-language-server-6.0.1.tgz",
    hash: Hash::NpmSha512("sha512-c5hEHM/7rdFRbQWoHXuddyJ53oF0M2dKeXQOYdwUek556oarltKeumKJGLp/ZMcGV7gW30mGn1MR3V5GDiLiKg=="),
    archive: ArchiveKind::Tgz,
    entries: 5,
}];

const TYPESCRIPT: [Asset; 1] = [Asset {
    target: "any",
    url: "https://registry.npmjs.org/typescript/-/typescript-6.0.3.tgz",
    hash: Hash::NpmSha512("sha512-y2TvuxSZPDyQakkFRPZHKFm+KKVqIisdg9/CZwm9ftvKXLP8NRWj38/ODjNbr43SsoXqNuAisEf1GdCxqWcdBw=="),
    archive: ArchiveKind::Tgz,
    entries: 140,
}];

const PYRIGHT: [Asset; 1] = [Asset {
    target: "any",
    url: "https://registry.npmjs.org/pyright/-/pyright-1.1.414.tgz",
    hash: Hash::NpmSha512("sha512-FPZZb51jepDX4eP7TEYDeNFtmE3WgwkkEcJpvH3/QmUSsj0EAy3LXu+xB4T/FWDejtsXlCfFr/rbWFjwuwuXww=="),
    archive: ArchiveKind::Tgz,
    entries: 5423,
}];

/// `gopls` is not here: playbook never downloads it.
pub static PINS: [Pin; 3] = [
    Pin {
        server_id: "rust-analyzer",
        releases: &[Release {
            component: "rust-analyzer",
            version: "2026-10-05",
            released: (2026, 10, 5),
            license: "MIT OR Apache-2.0",
            assets: &RUST_ANALYZER,
        }],
    },
    Pin {
        server_id: "typescript-language-server",
        releases: &[
            Release {
                component: "typescript-language-server",
                version: "6.0.1",
                released: (2026, 9, 24),
                license: "Apache-2.0",
                assets: &TYPESCRIPT_LANGUAGE_SERVER,
            },
            Release {
                component: "typescript",
                version: "6.0.3",
                released: (2026, 4, 16),
                license: "Apache-2.0",
                assets: &TYPESCRIPT,
            },
        ],
    },
    Pin {
        server_id: "pyright",
        releases: &[Release {
            component: "pyright",
            version: "1.1.414",
            released: (2026, 9, 9),
            license: "MIT",
            assets: &PYRIGHT,
        }],
    },
];

/// A pin older than this many days is flagged in `lsp status`.
pub const STALE_AFTER_DAYS: i64 = 365;

pub fn pin_for(server_id: &str) -> Option<&'static Pin> {
    PINS.iter().find(|pin| pin.server_id == server_id)
}

#[derive(Debug, PartialEq, Eq)]
pub enum ManifestError {
    /// No download exists for this platform.
    Unsupported {
        server_id: &'static str,
        component: &'static str,
        target: String,
    },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self::Unsupported {
            server_id,
            component,
            target,
        } = self;
        write!(
            f,
            "unsupported platform {target} for {component}; install it yourself and set \
             code.lsp.{server_id}.path"
        )
    }
}

impl std::error::Error for ManifestError {}

/// The asset of `release` for `target`. Platform independent packages match
/// every target.
pub fn asset_for(
    pin: &'static Pin,
    release: &'static Release,
    target: &str,
) -> Result<&'static Asset, ManifestError> {
    release
        .assets
        .iter()
        .find(|a| a.target == "any" || a.target == target)
        .ok_or_else(|| ManifestError::Unsupported {
            server_id: pin.server_id,
            component: release.component,
            target: target.to_string(),
        })
}

/// The target triple of a host, as the release assets name them.
pub fn target_triple(os: &str, arch: &str, musl: bool) -> String {
    match (os, arch) {
        ("macos", "aarch64") => "aarch64-apple-darwin".to_string(),
        ("macos", "x86_64") => "x86_64-apple-darwin".to_string(),
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu".to_string(),
        ("linux", "x86_64") if musl => "x86_64-unknown-linux-musl".to_string(),
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu".to_string(),
        (os, arch) => format!("{arch}-{os}"),
    }
}

pub fn current_target() -> String {
    target_triple(
        std::env::consts::OS,
        std::env::consts::ARCH,
        cfg!(target_env = "musl"),
    )
}

/// Days from 1970-01-01 to a civil date (proleptic Gregorian).
pub fn days_from_civil((year, month, day): Date) -> i64 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// How many whole days old a release is on `today` (days since 1970-01-01).
pub fn age_days(released: Date, today: i64) -> i64 {
    today - days_from_civil(released)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_releases() -> impl Iterator<Item = (&'static Pin, &'static Release)> {
        PINS.iter()
            .flat_map(|pin| pin.releases.iter().map(move |r| (pin, r)))
    }

    #[test]
    fn the_target_is_a_parameter_so_one_host_covers_every_triple() {
        for (os, arch, musl, expected) in [
            ("macos", "aarch64", false, "aarch64-apple-darwin"),
            ("macos", "x86_64", false, "x86_64-apple-darwin"),
            ("linux", "aarch64", false, "aarch64-unknown-linux-gnu"),
            ("linux", "x86_64", false, "x86_64-unknown-linux-gnu"),
            ("linux", "x86_64", true, "x86_64-unknown-linux-musl"),
        ] {
            // Act
            let target = target_triple(os, arch, musl);
            let pin = pin_for("rust-analyzer").unwrap();
            let asset = asset_for(pin, &pin.releases[0], &target).unwrap();

            // Assert
            assert_eq!(target, expected);
            assert!(
                asset.url.ends_with(&format!("rust-analyzer-{expected}.gz")),
                "{}",
                asset.url
            );
        }
    }

    #[test]
    fn windows_has_no_rust_analyzer_download_and_the_error_names_the_config_key() {
        // Arrange
        let target = target_triple("windows", "x86_64", false);
        let pin = pin_for("rust-analyzer").unwrap();

        // Act
        let err = asset_for(pin, &pin.releases[0], &target).unwrap_err();

        // Assert
        let text = err.to_string();
        assert!(
            text.contains("unsupported") && text.contains("code.lsp.rust-analyzer.path"),
            "{text}"
        );
    }

    #[test]
    fn npm_packages_match_every_target() {
        let pin = pin_for("pyright").unwrap();
        assert!(asset_for(pin, &pin.releases[0], "x86_64-pc-windows-msvc").is_ok());
    }

    #[test]
    fn no_asset_has_an_empty_hash_or_a_plain_http_url() {
        for (_, release) in all_releases() {
            for asset in release.assets {
                assert!(asset.url.starts_with("https://"), "{}", asset.url);
                let value = asset.hash.value();
                assert!(!value.is_empty(), "{}", asset.url);
                match asset.hash {
                    Hash::Sha256(hex) => {
                        assert_eq!(hex.len(), 64, "{}", asset.url);
                        assert!(hex
                            .bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
                    }
                    Hash::NpmSha512(integrity) => {
                        assert!(integrity.starts_with("sha512-"), "{}", asset.url);
                        assert_eq!(integrity.len(), "sha512-".len() + 88, "{}", asset.url);
                    }
                }
            }
        }
    }

    #[test]
    fn every_release_version_is_an_exact_version() {
        for (_, release) in all_releases() {
            let v = release.version;
            assert!(
                !v.is_empty()
                    && v.chars()
                        .all(|c| c.is_ascii_digit() || c == '.' || c == '-'),
                "{v}"
            );
            assert!(
                !v.contains('^') && !v.contains('~') && !v.contains('*'),
                "{v}"
            );
            assert!(release.assets.iter().all(|a| a.url.contains(v)), "{v}");
        }
    }

    #[test]
    fn the_typescript_server_installs_with_its_companion() {
        let pin = pin_for("typescript-language-server").unwrap();
        let names: Vec<_> = pin.releases.iter().map(|r| r.component).collect();
        assert_eq!(names, ["typescript-language-server", "typescript"]);
        assert!(pin_for("gopls").is_none());
    }

    #[test]
    fn age_is_counted_in_whole_days_across_month_and_year_ends() {
        let released = (2026, 10, 5);
        let day = days_from_civil(released);
        assert_eq!(days_from_civil((1970, 1, 1)), 0);
        assert_eq!(
            days_from_civil((2000, 3, 1)) - days_from_civil((2000, 2, 28)),
            2
        );
        assert_eq!(age_days(released, day), 0);
        assert_eq!(age_days(released, day + STALE_AFTER_DAYS), 365);
        assert_eq!(age_days((2025, 12, 31), days_from_civil((2026, 1, 1))), 1);
    }
}
