// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Release notes (ADR 0037). Every release carries `release-notes.json`,
//! built from the Conventional Commits by `scripts/release-notes.sh` and
//! listed in its SHA256SUMS. `playbook upgrade`, the automatic update and the
//! install script read it to show the breaking changes of a release. The
//! first run after an upgrade shows only a link, since it stays offline.

use crate::update::download::Fetcher;
use crate::update::resolve::compare;
use crate::update::verify::REPO;
use serde::Deserialize;
use std::cmp::Ordering;

/// The release asset.
pub const ASSET: &str = "release-notes.json";

/// The format this binary reads. A file with another format is unreadable.
pub const FORMAT: u64 = 1;

/// The name of the section that lists breaking changes.
const BREAKING: &str = "breaking";

/// The notes of one release. Fields this binary does not know are ignored,
/// as ADR 0037 asks of a reader.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Notes {
    pub format: u64,
    /// The component the notes are for; only `playbook` is read here.
    #[serde(default)]
    pub component: Option<String>,
    pub version: String,
    /// The previous release of the component, or `None` for the first one.
    #[serde(default)]
    pub previous: Option<String>,
    #[serde(default)]
    pub sections: Vec<Section>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Section {
    pub name: String,
    #[serde(default)]
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Item {
    pub text: String,
    #[serde(default)]
    pub pr: Option<u64>,
}

/// Parses a notes file. A file with another format, or with a version that
/// is not MAJOR.MINOR.PATCH, is an error.
pub fn parse(bytes: &[u8]) -> Result<Notes, String> {
    let notes: Notes = serde_json::from_slice(bytes).map_err(|e| format!("{ASSET}: {e}"))?;
    if notes.format != FORMAT {
        return Err(format!(
            "{ASSET} has format {}, and this playbook reads format {FORMAT}",
            notes.format
        ));
    }
    if !is_version(&notes.version) || notes.previous.as_deref().is_some_and(|v| !is_version(v)) {
        return Err(format!(
            "{ASSET} names a version that is not MAJOR.MINOR.PATCH"
        ));
    }
    if notes.component.as_deref().is_some_and(|c| c != "playbook") {
        return Err(format!("{ASSET} is not for playbook"));
    }
    Ok(notes)
}

fn is_version(v: &str) -> bool {
    crate::plugin_root::is_plain_version(v)
}

impl Notes {
    /// The breaking changes of the release.
    pub fn breaking(&self) -> impl Iterator<Item = &Item> {
        self.sections
            .iter()
            .filter(|s| s.name == BREAKING)
            .flat_map(|s| s.items.iter())
    }

    /// Whether an update from `from` crosses a release before this one,
    /// whose notes are not in this file.
    pub fn skips_releases_after(&self, from: &str) -> bool {
        self.previous
            .as_deref()
            .is_some_and(|p| compare(p, from) == Ordering::Greater)
    }
}

/// The release page of playbook `version`.
pub fn release_url(version: &str) -> String {
    format!(
        "https://github.com/{REPO}/releases/tag/v{}",
        version.trim_start_matches('v')
    )
}

/// The list of every release.
pub fn releases_url() -> String {
    format!("https://github.com/{REPO}/releases")
}

/// The line the first run after an upgrade prints. `from` is `None` when
/// the home shows an older binary that kept no version record.
pub fn updated_line(from: Option<&str>, to: &str) -> String {
    match from {
        Some(from) => format!(
            "playbook updated from {from} to {to}. Release notes: {}",
            release_url(to)
        ),
        None => format!(
            "playbook updated to {to}. Release notes: {}",
            release_url(to)
        ),
    }
}

/// Text from a release as one safe terminal line: control characters,
/// escape sequences included, line and paragraph separators and the marks
/// that reorder text are removed.
fn plain(text: &str) -> String {
    text.chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(
                    c,
                    '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
                )
        })
        .collect()
}

/// What an update from `from` to `to` prints: the breaking changes, a line
/// when the update crosses other releases, then the link to the notes. With
/// no notes, only the link.
pub fn render(notes: Option<&Notes>, from: &str, to: &str) -> String {
    let mut out = String::new();
    if let Some(notes) = notes {
        let mut breaking = notes.breaking().peekable();
        if breaking.peek().is_some() {
            out.push_str(&format!("Breaking changes in playbook {to}:\n"));
            for item in breaking {
                let pr = item.pr.map(|n| format!(" (#{n})")).unwrap_or_default();
                out.push_str(&format!("  - {}{pr}\n", plain(&item.text)));
            }
        }
        if notes.skips_releases_after(from) {
            out.push_str(&format!(
                "This update also crosses earlier releases. Their notes: {}\n",
                releases_url()
            ));
        }
    }
    out.push_str(&format!("Full notes: {}\n", release_url(to)));
    out
}

/// The notes of release `version` at `base/release-notes.json`, checked
/// against the release's SHA256SUMS: `sums` when the caller has it, else the
/// file next to the asset. `Ok(None)` when the release has no such asset,
/// which a release from before ADR 0037 lacks. Any other failure is an
/// error, a file this binary cannot read or notes of another version
/// included.
pub fn fetch(
    fetcher: &dyn Fetcher,
    base: &str,
    version: &str,
    sums: Option<&str>,
) -> Result<Option<Notes>, String> {
    let url = format!("{base}/{ASSET}");
    let body = match fetcher.get(&url) {
        Ok(body) => body,
        // The fetcher puts the URL first; only the rest says 404.
        Err(err) if err.strip_prefix(&url).unwrap_or(&err).contains("404") => return Ok(None),
        Err(err) => return Err(format!("could not download {ASSET}: {err}")),
    };
    let fetched;
    let sums = match sums {
        Some(sums) => sums,
        None => {
            let bytes = fetcher
                .get(&format!("{base}/SHA256SUMS"))
                .map_err(|e| format!("could not download SHA256SUMS: {e}"))?;
            fetched = String::from_utf8_lossy(&bytes).into_owned();
            &fetched
        }
    };
    crate::update::verify::verify_checksum(&body, sums, ASSET)?;
    let notes = parse(&body)?;
    if notes.version != version {
        return Err(format!("{ASSET} is for {}, not {version}", notes.version));
    }
    Ok(Some(notes))
}

/// `playbook upgrade --print-notes <file>`, which the install script runs on
/// the binary it just installed: prints the notes of this binary's release
/// from a file the script checked, and records them as shown so the first
/// run step does not print its line too. A file that cannot be read, or
/// notes of another release, print only the link. Never fails.
pub fn print_notes(file: &std::path::Path, from: Option<&str>) -> String {
    print_notes_in(&crate::common::paths::playbook_root(), file, from)
}

fn print_notes_in(root: &std::path::Path, file: &std::path::Path, from: Option<&str>) -> String {
    let running = env!("CARGO_PKG_VERSION");
    let notes = std::fs::read(file).ok().and_then(|b| parse(&b).ok());
    let notes = notes.filter(|n| n.version == running);
    // A home with old state files is left to `migrate`, which imports them
    // and says so; its first run line then shows the release.
    if crate::state::pending_legacy(root) == 0 {
        crate::notices::record(root, running);
    }
    render(notes.as_ref(), from.unwrap_or(running), running)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::hash::sha256_hex;
    use crate::update::download::fake::FakeFetcher;

    const BASE: &str = "https://dl.test/v0.28.0";

    fn notes_json(version: &str, previous: &str, breaking: &str) -> String {
        format!(
            r#"{{"format":1,"component":"playbook","version":"{version}","tag":"v{version}",
            "previous":{previous},"previous_tag":null,"date":"2026-10-16","future":true,
            "sections":[{{"name":"breaking","title":"Breaking changes","items":[{breaking}]}},
            {{"name":"fixes","title":"Fixes","items":[{{"type":"fix","scope":null,"text":"a fix","pr":9,"breaking":false}}]}}]}}"#
        )
    }

    const ONE_BREAK: &str = r#"{"type":"feat","scope":"cli","text":"the flag moved; run playbook setup","pr":12,"breaking":true}"#;

    fn fetcher_with(body: &str, sums_for: &[u8]) -> (FakeFetcher, String) {
        let mut f = FakeFetcher::default();
        f.0.insert(format!("{BASE}/{ASSET}"), body.as_bytes().to_vec());
        (f, format!("{}  {ASSET}\n", sha256_hex(sums_for)))
    }

    #[test]
    fn render_lists_the_breaking_changes_then_the_link() {
        // Arrange
        let notes = parse(notes_json("0.28.0", r#""0.27.0""#, ONE_BREAK).as_bytes()).unwrap();

        // Act
        let text = render(Some(&notes), "0.27.0", "0.28.0");

        // Assert
        assert_eq!(
            text,
            "Breaking changes in playbook 0.28.0:\n  - the flag moved; run playbook setup (#12)\n\
             Full notes: https://github.com/amplifiedengineer/playbook/releases/tag/v0.28.0\n"
        );
    }

    #[test]
    fn render_names_the_releases_page_when_the_update_skips_a_release() {
        // Arrange
        let notes = parse(notes_json("0.28.0", r#""0.27.0""#, "").as_bytes()).unwrap();

        // Act
        let skipped = render(Some(&notes), "0.26.0", "0.28.0");
        let next = render(Some(&notes), "0.27.0", "0.28.0");
        let none = render(None, "0.26.0", "0.28.0");

        // Assert
        assert!(skipped
            .contains("Their notes: https://github.com/amplifiedengineer/playbook/releases\n"));
        assert_eq!(
            next,
            "Full notes: https://github.com/amplifiedengineer/playbook/releases/tag/v0.28.0\n"
        );
        assert_eq!(none, next);
    }

    #[test]
    fn render_removes_control_characters_from_the_text() {
        // Arrange
        let item = r#"{"text":"red \u001b[31mtext\u0007 here\u202e\u2028","pr":null}"#;
        let notes = parse(notes_json("0.28.0", "null", item).as_bytes()).unwrap();

        // Act
        let text = render(Some(&notes), "0.27.0", "0.28.0");

        // Assert
        assert!(text.contains("  - red [31mtext here\n"), "{text}");
    }

    #[test]
    fn parse_ignores_unknown_fields_and_refuses_another_format_or_a_bad_version() {
        // Arrange
        let ok = notes_json("0.28.0", "null", "");
        let format_two = ok.replace(r#""format":1"#, r#""format":2"#);
        let bad_version = notes_json("0.28", "null", "");
        let bad_previous = notes_json("0.28.0", r#""v0.27.0""#, "");
        let other = ok.replace(
            r#""component":"playbook""#,
            r#""component":"playbook-telemetry""#,
        );

        // Act and assert
        assert_eq!(parse(ok.as_bytes()).unwrap().previous, None);
        assert!(parse(format_two.as_bytes())
            .unwrap_err()
            .contains("format 2"));
        assert!(parse(bad_version.as_bytes()).is_err());
        assert!(parse(bad_previous.as_bytes()).is_err());
        assert!(parse(b"not json").is_err());
        assert!(parse(other.as_bytes())
            .unwrap_err()
            .contains("not for playbook"));
    }

    #[test]
    fn fetch_checks_the_asset_against_the_sums() {
        // Arrange
        let body = notes_json("0.28.0", r#""0.27.0""#, ONE_BREAK);
        let (good, sums) = fetcher_with(&body, body.as_bytes());
        let (tampered, other_sums) = fetcher_with(&body, b"something else");

        // Act
        let found = fetch(&good, BASE, "0.28.0", Some(&sums));
        let mismatch = fetch(&tampered, BASE, "0.28.0", Some(&other_sums));
        let missing = fetch(&FakeFetcher::default(), BASE, "0.28.0", Some(&sums));
        let no_sums = fetch(&good, BASE, "0.28.0", None);
        let wrong_version = fetch(&good, BASE, "0.29.0", Some(&sums));
        let mut with_sums = good;
        with_sums
            .0
            .insert(format!("{BASE}/SHA256SUMS"), sums.into_bytes());
        let read_sums = fetch(&with_sums, BASE, "0.28.0", None);

        // Assert
        assert_eq!(found.unwrap().unwrap().breaking().count(), 1);
        assert!(mismatch.is_err());
        assert_eq!(missing, Ok(None));
        assert!(no_sums.unwrap_err().contains("SHA256SUMS"));
        assert!(read_sums.unwrap().is_some());
        assert!(wrong_version.unwrap_err().contains("is for 0.28.0"));
    }

    /// A fetcher whose every request fails with `error` after the URL.
    struct Failing(&'static str);

    impl Fetcher for Failing {
        fn get(&self, url: &str) -> Result<Vec<u8>, String> {
            Err(format!("{url}: {}", self.0))
        }
    }

    #[test]
    fn fetch_reads_only_a_404_as_a_missing_asset() {
        // Arrange: the URL itself holds 404.
        let base = "https://dl.test/v0.404.0";

        // Act
        let missing = fetch(&Failing("http status: 404"), base, "0.404.0", Some(""));
        let down = fetch(&Failing("http status: 500"), base, "0.404.0", Some(""));

        // Assert
        assert_eq!(missing, Ok(None));
        assert!(down.unwrap_err().contains("could not download"));
    }

    #[test]
    fn print_notes_shows_the_breaking_changes_of_this_release_and_records_them() {
        // Arrange
        let running = env!("CARGO_PKG_VERSION");
        let root = crate::common::test_support::scratch_dir("print-notes");
        std::fs::create_dir_all(&root).unwrap();
        let this = root.join("this.json");
        let other = root.join("other.json");
        std::fs::write(&this, notes_json(running, "null", ONE_BREAK)).unwrap();
        std::fs::write(&other, notes_json("0.0.1", "null", ONE_BREAK)).unwrap();

        // Act
        let shown = print_notes_in(&root, &this, Some("0.0.1"));
        let another = print_notes_in(&root, &other, Some("0.0.1"));

        // Assert
        assert!(
            shown.starts_with(&format!("Breaking changes in playbook {running}:\n")),
            "{shown}"
        );
        assert_eq!(another, format!("Full notes: {}\n", release_url(running)));
        assert_eq!(
            crate::state::get(&root, "notices/binary").unwrap(),
            Some(running.to_string())
        );
    }

    #[test]
    fn the_updated_line_names_both_versions_or_only_the_new_one() {
        assert_eq!(
            updated_line(Some("0.27.0"), "0.28.0"),
            "playbook updated from 0.27.0 to 0.28.0. Release notes: https://github.com/amplifiedengineer/playbook/releases/tag/v0.28.0"
        );
        assert!(updated_line(None, "0.28.0").starts_with("playbook updated to 0.28.0. "));
    }
}
