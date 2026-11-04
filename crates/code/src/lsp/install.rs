// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Installing and removing managed language servers under one root folder.
//! Only `install` touches the network, and it checks every download against
//! the compiled-in hash before anything is unpacked.

use super::archive::{self, Limits, MAX_GZ_BYTES};
use super::discover;
use super::manifest::{self, ArchiveKind, Asset, Pin, Release, Runner};
use super::registry;
use crate::host::Fetch;
use serde_json::json;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// A staging folder this old is left over from a run that died.
const STALE_STAGING: Duration = Duration::from_secs(60 * 60);

/// The one command that installs `gopls`; playbook does not.
pub const GOPLS_INSTALL: &str = "go install golang.org/x/tools/gopls@v0.24.0";

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub enum InstallError {
    /// The id is not a server id.
    InvalidId(String),
    /// A server playbook never installs.
    External(&'static str),
    Unsupported(manifest::ManifestError),
    Download {
        url: String,
        why: String,
    },
    HashMismatch {
        url: String,
    },
    Archive(archive::ArchiveError),
    /// The installed server did not start.
    DoesNotStart(String),
    NotInstalled(String),
    /// A folder under the root resolves outside it.
    Outside(PathBuf),
    Io(std::io::Error),
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId(id) => write!(
                f,
                "invalid id {id:?}; valid ids are: {}",
                registry::SERVERS
                    .iter()
                    .map(|s| s.id)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::External(id) => write!(
                f,
                "{id} is not installed by playbook; install it yourself with: {GOPLS_INSTALL}"
            ),
            Self::Unsupported(e) => write!(f, "{e}"),
            Self::Download { url, why } => write!(f, "could not download {url}: {why}"),
            Self::HashMismatch { url } => {
                write!(f, "hash mismatch for {url}; nothing was installed")
            }
            Self::Archive(e) => write!(f, "{e}"),
            Self::DoesNotStart(why) => {
                write!(
                    f,
                    "the installed server did not start, so it was removed: {why}"
                )
            }
            Self::NotInstalled(id) => write!(f, "{id} is not installed"),
            Self::Outside(p) => write!(
                f,
                "{} resolves outside the install folder, so nothing was removed",
                p.display()
            ),
            Self::Io(e) => write!(f, "install failed: {e}"),
        }
    }
}

impl std::error::Error for InstallError {}

impl From<std::io::Error> for InstallError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<archive::ArchiveError> for InstallError {
    fn from(e: archive::ArchiveError) -> Self {
        Self::Archive(e)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Installed {
    Done {
        dir: PathBuf,
    },
    /// Already there, and `force` was not given.
    AlreadyInstalled {
        dir: PathBuf,
    },
}

pub struct Options<'a> {
    pub root: &'a Path,
    pub target: &'a str,
    pub force: bool,
    /// For the stale staging sweep.
    pub now: SystemTime,
    /// Whether the installed server starts; false removes the install.
    pub starts: &'a dyn Fn(&Path, &Pin) -> Result<(), String>,
}

/// The real start check: runs `--version` on the installed server from the
/// temp dir. A Node server needs `node` on `PATH`.
pub fn starts_with_version_probe(staging: &Path, pin: &Pin) -> Result<(), String> {
    let release = &pin.releases[0];
    let entry = staging.join(release.component).join(release.entry);
    let program = match release.runner {
        Runner::Direct => entry.clone(),
        Runner::Node => discover::search_path("node", &discover::process_path())
            .ok_or("node was not found on PATH")?,
    };
    let mut command = std::process::Command::new(&program);
    if release.runner == Runner::Node {
        command.arg(&entry);
    }
    command.arg("--version").current_dir(std::env::temp_dir());
    let ok = playbook_common::proc::run_with_timeout(&mut command, discover::PROBE_TIMEOUT)
        .is_some_and(|out| out.status.success());
    if ok {
        Ok(())
    } else {
        Err(format!("`{} --version` failed", entry.display()))
    }
}

/// Rejects ids and versions that could leave their folder.
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn pin_of(id: &str) -> Result<&'static Pin, InstallError> {
    if !safe_name(id) || registry::by_id(id).is_none() {
        return Err(InstallError::InvalidId(id.to_string()));
    }
    manifest::pin_for(id).ok_or(InstallError::External(
        registry::by_id(id).map_or("server", |s| s.id),
    ))
}

/// The folder an installed `pin` lives in.
pub fn version_dir(root: &Path, pin: &Pin) -> PathBuf {
    root.join(pin.server_id).join(pin.releases[0].version)
}

pub fn install(
    id: &str,
    fetch: &dyn Fetch,
    options: &Options<'_>,
) -> Result<Installed, InstallError> {
    install_pin(pin_of(id)?, fetch, options)
}

/// Installs the releases of `pin`. Split from [`install`] so tests can use a
/// pin whose hashes match their own bytes.
fn install_pin(
    pin: &'static Pin,
    fetch: &dyn Fetch,
    options: &Options<'_>,
) -> Result<Installed, InstallError> {
    let final_dir = version_dir(options.root, pin);
    if final_dir.exists() && !options.force {
        return Ok(Installed::AlreadyInstalled { dir: final_dir });
    }
    fs::create_dir_all(options.root)?;
    sweep_staging(options.root, options.now);
    let downloads = pin
        .releases
        .iter()
        .map(|release| download(pin, release, fetch, options.target))
        .collect::<Result<Vec<_>, _>>()?;
    let staging = options.root.join(format!(
        ".staging-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::SeqCst)
    ));
    make_private_dir(&staging)?;
    let built = unpack_all(&downloads, &staging)
        .and_then(|()| write_receipt(pin, options.target, &staging))
        .and_then(|()| (options.starts)(&staging, pin).map_err(InstallError::DoesNotStart));
    if let Err(e) = built {
        let _ = fs::remove_dir_all(&staging);
        return Err(e);
    }
    publish(&staging, &final_dir, options.force)
}

struct Download {
    release: &'static Release,
    asset: &'static Asset,
    bytes: Vec<u8>,
}

fn download(
    pin: &'static Pin,
    release: &'static Release,
    fetch: &dyn Fetch,
    target: &str,
) -> Result<Download, InstallError> {
    let asset = manifest::asset_for(pin, release, target).map_err(InstallError::Unsupported)?;
    let bytes = fetch.get(asset.url).map_err(|why| InstallError::Download {
        url: asset.url.to_string(),
        why,
    })?;
    if !manifest::verify(asset.hash, &bytes) {
        return Err(InstallError::HashMismatch {
            url: asset.url.to_string(),
        });
    }
    Ok(Download {
        release,
        asset,
        bytes,
    })
}

fn unpack_all(downloads: &[Download], staging: &Path) -> Result<(), InstallError> {
    for download in downloads {
        let dir = staging.join(download.release.component);
        fs::create_dir(&dir)?;
        match download.asset.archive {
            ArchiveKind::Gz => {
                let file = dir.join(download.release.entry);
                archive::gunzip_to_file(&download.bytes, &file, MAX_GZ_BYTES)?;
            }
            ArchiveKind::Tgz => {
                archive::extract_tgz(&download.bytes, &dir, &Limits::default())?;
            }
        }
    }
    Ok(())
}

fn write_receipt(pin: &Pin, target: &str, staging: &Path) -> Result<(), InstallError> {
    let releases: Vec<_> = pin
        .releases
        .iter()
        .map(|r| {
            let asset = manifest::asset_for(pin, r, target).ok();
            json!({
                "component": r.component,
                "version": r.version,
                "license": r.license,
                "url": asset.map(|a| a.url),
                "hash": asset.map(|a| a.hash.value()),
                "launch": launch_words(r),
            })
        })
        .collect();
    let receipt = json!({
        "source": "managed",
        "server": pin.server_id,
        "platform": target,
        "releases": releases,
    });
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(staging.join("install.json"))?;
    file.write_all(receipt.to_string().as_bytes())?;
    Ok(())
}

fn launch_words(release: &Release) -> String {
    match release.runner {
        Runner::Direct => format!("{}/{}", release.component, release.entry),
        Runner::Node => format!("node {}/{}", release.component, release.entry),
    }
}

/// Moves the finished staging folder into place. A folder that appeared in
/// the meantime wins unless `force` asked to replace it.
fn publish(staging: &Path, final_dir: &Path, force: bool) -> Result<Installed, InstallError> {
    if let Some(parent) = final_dir.parent() {
        fs::create_dir_all(parent)?;
    }
    if force && final_dir.exists() {
        fs::remove_dir_all(final_dir)?;
    }
    match fs::rename(staging, final_dir) {
        Ok(()) => Ok(Installed::Done {
            dir: final_dir.to_path_buf(),
        }),
        Err(_) if final_dir.exists() => {
            let _ = fs::remove_dir_all(staging);
            Ok(Installed::AlreadyInstalled {
                dir: final_dir.to_path_buf(),
            })
        }
        Err(e) => {
            let _ = fs::remove_dir_all(staging);
            Err(InstallError::Io(e))
        }
    }
}

/// Removes `.staging-*` folders last changed over an hour before `now`.
pub fn sweep_staging(root: &Path, now: SystemTime) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(".staging-") {
            continue;
        }
        let age = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok());
        if age.is_some_and(|age| age > STALE_STAGING) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Removes every installed version of `id` under `root`. It refuses when a
/// folder inside resolves outside `root`, so a swapped link cannot steer the
/// delete.
pub fn remove(id: &str, root: &Path) -> Result<PathBuf, InstallError> {
    if !safe_name(id) || registry::by_id(id).is_none() {
        return Err(InstallError::InvalidId(id.to_string()));
    }
    let dir = root.join(id);
    if !dir.exists() {
        return Err(InstallError::NotInstalled(id.to_string()));
    }
    let canonical_root = root.canonicalize()?;
    let mut inside = vec![dir.clone()];
    inside.extend(fs::read_dir(&dir)?.flatten().map(|e| e.path()));
    for path in inside {
        let resolved = path.canonicalize()?;
        if !resolved.starts_with(&canonical_root) {
            return Err(InstallError::Outside(path));
        }
    }
    fs::remove_dir_all(&dir)?;
    Ok(dir)
}

fn make_private_dir(dir: &Path) -> std::io::Result<()> {
    fs::create_dir(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
