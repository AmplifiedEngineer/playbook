// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::lsp::manifest::{Asset, Hash};
use crate::lsp::testkit::scratch;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Mutex;

fn leak<T>(value: T) -> &'static T {
    Box::leak(Box::new(value))
}

fn gz(data: &[u8]) -> Vec<u8> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

/// A one-file `.gz` pin for server `rust-analyzer` whose hash matches `bytes`.
fn pin_for_bytes(bytes: &[u8]) -> (&'static Pin, String) {
    let url = "https://example.test/rust-analyzer-test.gz".to_string();
    let hash = playbook_common::hash::sha256_hex(bytes);
    let asset = leak(vec![Asset {
        target: "any",
        url: leak(url.clone()),
        hash: Hash::Sha256(leak(hash)),
        archive: ArchiveKind::Gz,
        entries: 0,
    }]);
    let release = leak(vec![Release {
        component: "rust-analyzer",
        version: "9999-01-01",
        released: (2026, 1, 1),
        license: "MIT",
        assets: asset,
        entry: "rust-analyzer",
        runner: Runner::Direct,
    }]);
    let pin = leak(Pin {
        server_id: "rust-analyzer",
        releases: release,
    });
    (pin, url)
}

struct MapFetch(HashMap<String, Vec<u8>>);

impl Fetch for MapFetch {
    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        self.0
            .get(url)
            .cloned()
            .ok_or_else(|| "no such url".to_string())
    }
}

/// Blocks the download until the test releases it.
struct HeldFetch {
    inner: MapFetch,
    release: Mutex<Receiver<()>>,
}

impl Fetch for HeldFetch {
    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        self.release.lock().unwrap().recv().unwrap();
        self.inner.get(url)
    }
}

fn starts_ok(_: &Path, _: &Pin) -> Result<(), String> {
    Ok(())
}

fn options<'a>(root: &'a Path, force: bool) -> Options<'a> {
    Options {
        root,
        target: "aarch64-apple-darwin",
        force,
        now: SystemTime::now(),
        starts: &starts_ok,
    }
}

fn staging_dirs(root: &Path) -> Vec<String> {
    fs::read_dir(root)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".staging-"))
        .collect()
}

#[test]
fn right_bytes_install_the_files_and_a_receipt() {
    // Arrange
    let root = scratch("inst-ok");
    let (pin, url) = pin_for_bytes(&gz(b"#!/bin/sh\necho hi\n"));
    let fetch = MapFetch(HashMap::from([(url.clone(), gz(b"#!/bin/sh\necho hi\n"))]));

    // Act
    let got = install_pin(pin, &fetch, &options(&root, false)).unwrap();

    // Assert
    let dir = version_dir(&root, pin);
    assert_eq!(got, Installed::Done { dir: dir.clone() });
    assert_eq!(
        fs::read(dir.join("rust-analyzer/rust-analyzer")).unwrap(),
        b"#!/bin/sh\necho hi\n"
    );
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("install.json")).unwrap()).unwrap();
    assert_eq!(receipt["source"], "managed");
    assert_eq!(receipt["platform"], "aarch64-apple-darwin");
    assert_eq!(receipt["releases"][0]["url"], url.as_str());
    assert_eq!(receipt["releases"][0]["license"], "MIT");
    assert!(staging_dirs(&root).is_empty());
}

#[test]
fn wrong_bytes_leave_nothing_under_the_root_and_report_a_hash_mismatch() {
    // Arrange
    let root = scratch("inst-bad-hash");
    let (pin, url) = pin_for_bytes(&gz(b"expected"));
    let fetch = MapFetch(HashMap::from([(url, gz(b"tampered"))]));

    // Act
    let err = install_pin(pin, &fetch, &options(&root, false)).unwrap_err();

    // Assert
    assert!(matches!(err, InstallError::HashMismatch { .. }));
    assert!(err.to_string().contains("hash mismatch"), "{err}");
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
}

#[test]
fn a_second_install_without_force_reports_already_installed() {
    // Arrange
    let root = scratch("inst-twice");
    let bytes = gz(b"x");
    let (pin, url) = pin_for_bytes(&bytes);
    let fetch = MapFetch(HashMap::from([(url, bytes)]));
    install_pin(pin, &fetch, &options(&root, false)).unwrap();

    // Act
    let again = install_pin(pin, &fetch, &options(&root, false)).unwrap();

    // Assert
    assert!(matches!(again, Installed::AlreadyInstalled { .. }));
}

#[test]
fn force_replaces_the_old_folder() {
    // Arrange
    let root = scratch("inst-force");
    let bytes = gz(b"x");
    let (pin, url) = pin_for_bytes(&bytes);
    let fetch = MapFetch(HashMap::from([(url, bytes)]));
    install_pin(pin, &fetch, &options(&root, false)).unwrap();
    let marker = version_dir(&root, pin).join("old-marker");
    fs::write(&marker, "old").unwrap();

    // Act
    let again = install_pin(pin, &fetch, &options(&root, true)).unwrap();

    // Assert
    assert!(matches!(again, Installed::Done { .. }));
    assert!(!marker.exists());
    assert!(version_dir(&root, pin).join("install.json").is_file());
}

#[test]
fn a_start_check_failure_removes_the_install() {
    // Arrange
    let root = scratch("inst-nostart");
    let bytes = gz(b"x");
    let (pin, url) = pin_for_bytes(&bytes);
    let fetch = MapFetch(HashMap::from([(url, bytes)]));
    let fails = |_: &Path, _: &Pin| Err("no node".to_string());
    let opts = Options {
        starts: &fails,
        ..options(&root, false)
    };

    // Act
    let err = install_pin(pin, &fetch, &opts).unwrap_err();

    // Assert
    assert!(matches!(err, InstallError::DoesNotStart(_)));
    assert!(!version_dir(&root, pin).exists());
    assert!(staging_dirs(&root).is_empty());
}

#[test]
fn a_racing_install_that_finishes_second_loses_to_the_first_to_publish() {
    // Arrange
    let root = scratch("inst-race");
    let bytes = gz(b"x");
    let (pin, url) = pin_for_bytes(&bytes);
    let (release, held) = channel();
    let slow = HeldFetch {
        inner: MapFetch(HashMap::from([(url.clone(), bytes.clone())])),
        release: Mutex::new(held),
    };
    let quick = MapFetch(HashMap::from([(url, bytes)]));

    // Act
    let (first, second) = std::thread::scope(|scope| {
        let first = scope.spawn(|| install_pin(pin, &slow, &options(&root, false)).unwrap());
        let second = install_pin(pin, &quick, &options(&root, false)).unwrap();
        release.send(()).unwrap();
        (first.join().unwrap(), second)
    });

    // Assert
    assert!(matches!(second, Installed::Done { .. }));
    assert!(
        matches!(first, Installed::AlreadyInstalled { .. }),
        "{first:?}"
    );
    let versions: Vec<_> = fs::read_dir(root.join("rust-analyzer"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(versions.len(), 1);
    assert!(version_dir(&root, pin).join("install.json").is_file());
    assert!(staging_dirs(&root).is_empty());
}

#[test]
fn an_unsupported_platform_installs_nothing() {
    // Arrange
    let root = scratch("inst-windows");
    let opts = Options {
        target: "x86_64-pc-windows-msvc",
        ..options(&root, false)
    };

    // Act
    let err = install("rust-analyzer", &MapFetch(HashMap::new()), &opts).unwrap_err();

    // Assert
    assert!(err.to_string().contains("unsupported"), "{err}");
    assert!(staging_dirs(&root).is_empty());
}

#[test]
fn ids_that_are_not_servers_or_could_leave_the_folder_are_rejected_before_touching_disk() {
    for id in ["nosuch", "../x", "a/b", "", ".", "rust-analyzer/../.."] {
        // Arrange
        let root = scratch("inst-ids").join("never-created");

        // Act
        let err = install(id, &MapFetch(HashMap::new()), &options(&root, false)).unwrap_err();
        let removed = remove(id, &root).unwrap_err();

        // Assert
        assert!(err.to_string().contains("invalid id"), "{id}: {err}");
        assert!(
            removed.to_string().contains("invalid id"),
            "{id}: {removed}"
        );
        assert!(!root.exists(), "{id}");
    }
}

#[test]
fn gopls_is_refused_with_the_go_install_line() {
    let root = scratch("inst-gopls").join("never-created");
    let err = install("gopls", &MapFetch(HashMap::new()), &options(&root, false)).unwrap_err();
    assert!(err.to_string().contains(GOPLS_INSTALL), "{err}");
    assert!(!root.exists());
}

#[test]
fn staging_folders_are_swept_only_when_over_an_hour_old() {
    // Arrange
    let root = scratch("inst-sweep");
    let now = SystemTime::now();
    for minutes in [59u64, 60, 61] {
        let dir = root.join(format!(".staging-1-{minutes}"));
        fs::create_dir_all(&dir).unwrap();
        let handle = fs::File::open(&dir).unwrap();
        handle
            .set_modified(now - Duration::from_secs(minutes * 60))
            .unwrap();
    }

    // Act
    sweep_staging(&root, now);

    // Assert
    let mut left = staging_dirs(&root);
    left.sort();
    assert_eq!(left, [".staging-1-59", ".staging-1-60"]);
}

#[test]
fn remove_deletes_every_version_and_reports_when_nothing_is_installed() {
    // Arrange
    let root = scratch("inst-remove");
    fs::create_dir_all(root.join("pyright/1.0.0")).unwrap();
    fs::create_dir_all(root.join("pyright/1.1.414")).unwrap();

    // Act
    let removed = remove("pyright", &root);
    let again = remove("pyright", &root).unwrap_err();

    // Assert
    assert!(removed.is_ok());
    assert!(!root.join("pyright").exists());
    assert!(matches!(again, InstallError::NotInstalled(_)));
    assert!(again.to_string().contains("not installed"), "{again}");
}

#[cfg(unix)]
#[test]
fn a_version_folder_swapped_for_a_link_outside_is_refused_and_its_target_survives() {
    // Arrange
    let base = scratch("inst-link");
    let root = base.join("lsp");
    let outside = base.join("elsewhere");
    fs::create_dir_all(root.join("pyright")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("marker"), "keep").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("pyright/1.1.414")).unwrap();

    // Act
    let err = remove("pyright", &root).unwrap_err();

    // Assert
    assert!(matches!(err, InstallError::Outside(_)));
    assert!(err.to_string().contains("outside"), "{err}");
    assert!(outside.join("marker").is_file());
}
