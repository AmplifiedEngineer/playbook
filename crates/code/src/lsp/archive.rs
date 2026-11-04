// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Unpacking a downloaded server. The extractor reads each archive entry and
//! writes the file itself, rather than calling `unpack`, so a path, link or
//! size it did not approve never reaches the disk.

use flate2::read::GzDecoder;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use tar::EntryType;

/// Caps on one archive, injectable for tests.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_entries: usize,
    /// Bytes written across all files.
    pub max_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_entries: 20_000,
            max_bytes: 512 * 1024 * 1024,
        }
    }
}

/// The most a single gzip file may decompress to.
pub const MAX_GZ_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug)]
pub enum ArchiveError {
    Absolute(String),
    Parent(String),
    /// A symlink or hardlink entry.
    Link(String),
    /// A device, fifo or other special entry.
    Device(String),
    EntryLimit {
        limit: usize,
    },
    SizeLimit {
        limit: u64,
    },
    Malformed(String),
    Io(io::Error),
}

impl fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Absolute(p) => write!(f, "archive entry {p:?} has an absolute path"),
            Self::Parent(p) => write!(f, "archive entry {p:?} climbs out with a parent path"),
            Self::Link(p) => write!(f, "archive entry {p:?} is a link, which is refused"),
            Self::Device(p) => write!(f, "archive entry {p:?} is a device or special file"),
            Self::EntryLimit { limit } => {
                write!(
                    f,
                    "archive has more than the entry limit of {limit} entries"
                )
            }
            Self::SizeLimit { limit } => {
                write!(f, "archive content is over the size limit of {limit} bytes")
            }
            Self::Malformed(why) => write!(f, "archive is malformed: {why}"),
            Self::Io(e) => write!(f, "archive could not be written: {e}"),
        }
    }
}

impl std::error::Error for ArchiveError {}

impl From<io::Error> for ArchiveError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Decompresses a single gzip file to `dest`, which must not exist, as an
/// executable. Fails with `SizeLimit` if it would pass `max` bytes.
pub fn gunzip_to_file(bytes: &[u8], dest: &Path, max: u64) -> Result<u64, ArchiveError> {
    let mut out = OpenOptions::new().write(true).create_new(true).open(dest)?;
    let written = copy_capped(GzDecoder::new(bytes), &mut out, max)?;
    set_mode(dest, 0o755)?;
    Ok(written)
}

/// What was unpacked.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub files: usize,
    pub bytes: u64,
}

/// Extracts a gzip compressed tar into `root`, which must be a fresh
/// directory the caller owns. Nothing outside `root` is written.
pub fn extract_tgz(bytes: &[u8], root: &Path, limits: &Limits) -> Result<Report, ArchiveError> {
    let mut archive = tar::Archive::new(GzDecoder::new(bytes));
    let mut report = Report::default();
    let mut entries = 0;
    for entry in archive.entries().map_err(malformed)? {
        let mut entry = entry.map_err(malformed)?;
        entries += 1;
        if entries > limits.max_entries {
            return Err(ArchiveError::EntryLimit {
                limit: limits.max_entries,
            });
        }
        let kind = entry.header().entry_type();
        let raw = entry.path().map_err(malformed)?.into_owned();
        let name = raw.display().to_string();
        match kind {
            EntryType::XGlobalHeader => continue,
            EntryType::Symlink | EntryType::Link => return Err(ArchiveError::Link(name)),
            EntryType::Char | EntryType::Block | EntryType::Fifo => {
                return Err(ArchiveError::Device(name))
            }
            EntryType::Regular | EntryType::Continuous | EntryType::Directory => {}
            other => {
                return Err(ArchiveError::Malformed(format!(
                    "entry {name:?} has the unsupported type {other:?}"
                )))
            }
        }
        let relative = checked_relative(&raw, &name)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let target = root.join(&relative);
        if kind == EntryType::Directory {
            fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let room = limits.max_bytes - report.bytes;
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?;
        let written = copy_capped(&mut entry, &mut out, room).map_err(|e| match e {
            ArchiveError::SizeLimit { .. } => ArchiveError::SizeLimit {
                limit: limits.max_bytes,
            },
            other => other,
        })?;
        drop(out);
        let mode = entry.header().mode().unwrap_or(0o644);
        // Clear setuid, setgid and sticky; keep only the permission bits.
        set_mode(&target, mode & 0o777)?;
        report.files += 1;
        report.bytes += written;
    }
    Ok(report)
}

/// The entry path with no root, no prefix and no parent step, or why not.
fn checked_relative(raw: &Path, name: &str) -> Result<PathBuf, ArchiveError> {
    let mut clean = PathBuf::new();
    for part in raw.components() {
        match part {
            Component::Normal(p) => clean.push(p),
            Component::CurDir => {}
            Component::ParentDir => return Err(ArchiveError::Parent(name.to_string())),
            Component::RootDir | Component::Prefix(_) => {
                return Err(ArchiveError::Absolute(name.to_string()))
            }
        }
    }
    Ok(clean)
}

/// Copies up to `max` bytes; one byte more is `SizeLimit`.
fn copy_capped(mut from: impl Read, to: &mut impl Write, max: u64) -> Result<u64, ArchiveError> {
    let copied = io::copy(&mut from.by_ref().take(max.saturating_add(1)), to)?;
    if copied > max {
        return Err(ArchiveError::SizeLimit { limit: max });
    }
    Ok(copied)
}

fn malformed(e: io::Error) -> ArchiveError {
    ArchiveError::Malformed(e.to_string())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::manifest::PINS;
    use flate2::write::GzEncoder;
    use flate2::Compression;

    fn scratch(tag: &str) -> PathBuf {
        let dir = playbook_common::test_support::scratch_dir(tag);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn gz(data: &[u8]) -> Vec<u8> {
        let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    /// An archive built entry by entry without the builder's path checks, so
    /// hostile names and types can be written.
    struct Spec<'a> {
        name: &'a str,
        kind: EntryType,
        data: &'a [u8],
        link: Option<&'a str>,
        mode: u32,
    }

    fn file<'a>(name: &'a str, data: &'a [u8]) -> Spec<'a> {
        Spec {
            name,
            kind: EntryType::Regular,
            data,
            link: None,
            mode: 0o644,
        }
    }

    fn tgz(specs: &[Spec<'_>]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for spec in specs {
            let mut header = tar::Header::new_gnu();
            let name = spec.name.as_bytes();
            header.as_old_mut().name[..name.len()].copy_from_slice(name);
            header.set_entry_type(spec.kind);
            header.set_size(spec.data.len() as u64);
            header.set_mode(spec.mode);
            if let Some(link) = spec.link {
                header.as_old_mut().linkname[..link.len()].copy_from_slice(link.as_bytes());
            }
            header.set_cksum();
            builder.append(&header, spec.data).unwrap();
        }
        gz(&builder.into_inner().unwrap())
    }

    #[test]
    fn hostile_entries_each_fail_with_their_kind_and_write_nothing_outside_the_root() {
        let cases: Vec<(&str, Spec<'_>, &str)> = vec![
            ("parent", file("../evil", b"x"), "parent"),
            ("absolute", file("/etc/x", b"x"), "absolute"),
            (
                "symlink-in-root",
                Spec {
                    kind: EntryType::Symlink,
                    link: Some("a"),
                    ..file("l", b"")
                },
                "link",
            ),
            (
                "symlink-to-root",
                Spec {
                    kind: EntryType::Symlink,
                    link: Some("/"),
                    ..file("l", b"")
                },
                "link",
            ),
            (
                "hardlink",
                Spec {
                    kind: EntryType::Link,
                    link: Some("a"),
                    ..file("h", b"")
                },
                "link",
            ),
            (
                "device",
                Spec {
                    kind: EntryType::Char,
                    ..file("d", b"")
                },
                "device",
            ),
            (
                "fifo",
                Spec {
                    kind: EntryType::Fifo,
                    ..file("f", b"")
                },
                "device",
            ),
        ];
        for (tag, spec, text) in cases {
            // Arrange
            let outer = scratch(&format!("arc-{tag}"));
            let root = outer.join("root");
            fs::create_dir_all(&root).unwrap();
            let data = tgz(&[file("package/ok.txt", b"fine"), spec]);

            // Act
            let err = extract_tgz(&data, &root, &Limits::default()).unwrap_err();

            // Assert
            assert!(err.to_string().contains(text), "{tag}: {err}");
            let siblings: Vec<_> = fs::read_dir(&outer).unwrap().flatten().collect();
            assert_eq!(
                siblings.len(),
                1,
                "{tag}: nothing may appear beside the root"
            );
            assert!(!Path::new("/etc/x").exists());
        }
    }

    #[test]
    fn the_entry_limit_admits_the_cap_and_rejects_one_more() {
        for (count, ok) in [(9usize, true), (10, true), (11, false)] {
            // Arrange
            let root = scratch(&format!("arc-entries-{count}"));
            let names: Vec<String> = (0..count).map(|i| format!("f{i}")).collect();
            let specs: Vec<Spec<'_>> = names.iter().map(|n| file(n, b"x")).collect();
            let limits = Limits {
                max_entries: 10,
                ..Limits::default()
            };

            // Act
            let got = extract_tgz(&tgz(&specs), &root, &limits);

            // Assert
            match (ok, got) {
                (true, Ok(report)) => assert_eq!(report.files, count),
                (false, Err(e @ ArchiveError::EntryLimit { .. })) => {
                    assert!(e.to_string().contains("entry limit"), "{e}");
                }
                (_, other) => panic!("{count}: {other:?}"),
            }
        }
    }

    #[test]
    fn the_total_size_cap_admits_the_cap_and_rejects_one_more() {
        for (size, ok) in [(1023usize, true), (1024, true), (1025, false)] {
            // Arrange
            let root = scratch(&format!("arc-size-{size}"));
            let data = vec![b'x'; size];
            let limits = Limits {
                max_bytes: 1024,
                ..Limits::default()
            };

            // Act
            let got = extract_tgz(&tgz(&[file("big", &data)]), &root, &limits);

            // Assert
            match (ok, got) {
                (true, Ok(report)) => assert_eq!(report.bytes, size as u64),
                (false, Err(e @ ArchiveError::SizeLimit { .. })) => {
                    assert!(e.to_string().contains("size limit"), "{e}");
                }
                (_, other) => panic!("{size}: {other:?}"),
            }
        }
    }

    #[test]
    fn the_size_cap_counts_every_file_together() {
        // Arrange
        let root = scratch("arc-size-sum");
        let limits = Limits {
            max_bytes: 1024,
            ..Limits::default()
        };
        let half = vec![b'x'; 600];

        // Act
        let got = extract_tgz(&tgz(&[file("a", &half), file("b", &half)]), &root, &limits);

        // Assert
        assert!(
            matches!(got, Err(ArchiveError::SizeLimit { .. })),
            "{got:?}"
        );
    }

    #[test]
    fn a_gz_file_cap_admits_the_cap_and_rejects_one_more() {
        for (size, ok) in [(1023usize, true), (1024, true), (1025, false)] {
            // Arrange
            let dest = scratch(&format!("gz-{size}")).join("bin");
            let data = vec![b'x'; size];

            // Act
            let got = gunzip_to_file(&gz(&data), &dest, 1024);

            // Assert
            match (ok, got) {
                (true, Ok(written)) => assert_eq!(written, size as u64),
                (false, Err(e)) => assert!(e.to_string().contains("size limit"), "{e}"),
                (_, other) => panic!("{size}: {other:?}"),
            }
        }
    }

    #[test]
    fn a_truncated_archive_is_malformed_and_a_nested_path_extracts() {
        // Arrange
        let root = scratch("arc-trunc");
        let good = tgz(&[file("package/lib/cli.mjs", b"export {}")]);

        // Act
        let ok = extract_tgz(&good, &root, &Limits::default());
        let cut = extract_tgz(
            &good[..good.len() / 2],
            &scratch("arc-trunc-2"),
            &Limits::default(),
        );

        // Assert
        assert_eq!(ok.unwrap(), Report { files: 1, bytes: 9 });
        assert_eq!(
            fs::read(root.join("package/lib/cli.mjs")).unwrap(),
            b"export {}"
        );
        assert!(matches!(cut, Err(ArchiveError::Malformed(_))), "{cut:?}");
    }

    #[cfg(unix)]
    #[test]
    fn setuid_and_setgid_bits_are_dropped_and_the_exec_bits_kept() {
        use std::os::unix::fs::PermissionsExt;

        // Arrange
        let root = scratch("arc-mode");
        let data = tgz(&[Spec {
            mode: 0o4755,
            ..file("tool", b"#!/bin/sh\n")
        }]);

        // Act
        extract_tgz(&data, &root, &Limits::default()).unwrap();

        // Assert
        let mode = fs::metadata(root.join("tool"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777, 0o755);
    }

    #[test]
    fn every_pinned_archive_is_far_below_the_default_entry_cap() {
        let cap = Limits::default().max_entries;
        for pin in &PINS {
            for release in pin.releases {
                for asset in release.assets {
                    assert!(asset.entries < cap, "{}", asset.url);
                }
            }
        }
    }
}
