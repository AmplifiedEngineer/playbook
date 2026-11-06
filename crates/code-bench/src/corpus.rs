// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The corpus manifest and the fetcher. The manifest pins every repo by
//! commit and every tool by version. The fetcher clones through an injected
//! transport, retries with backoff through an injected sleeper, caches by
//! commit, rejects CRLF checkouts and never fetches submodules.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The bundled manifest.
pub const MANIFEST: &str = include_str!("../corpus.toml");

/// Licenses that may enter the manifest.
pub const LICENSE_ALLOWLIST: [&str; 5] =
    ["MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC"];

/// Clone attempts per repo before the fetch fails.
pub const MAX_FETCH_ATTEMPTS: u32 = 3;

/// Wait before the second attempt. Each later wait doubles.
pub const BACKOFF_BASE: Duration = Duration::from_secs(2);

const COMMIT_HEX_LEN: usize = 40;
const NUL_PROBE_BYTES: usize = 8 * 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CorpusError {
    #[error("manifest does not parse: {0}")]
    Parse(String),
    #[error("{what} is invalid: {reason}")]
    Invalid { what: String, reason: &'static str },
    #[error("repo {repo} has license {license}, which is not allowed")]
    License { repo: String, license: String },
    #[error("fetch of {repo} failed after {attempts} attempts: {last}")]
    Fetch {
        repo: String,
        attempts: u32,
        last: String,
    },
    #[error("repo {repo} has CRLF line endings in {path}")]
    Crlf { repo: String, path: String },
    #[error("io error: {0}")]
    Io(String),
}

impl From<std::io::Error> for CorpusError {
    fn from(e: std::io::Error) -> Self {
        CorpusError::Io(e.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Pilot,
    Full,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tools {
    pub rg: String,
    pub ctags: String,
    pub tokenizer: String,
    pub oracles: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repo {
    pub name: String,
    pub url: String,
    pub commit: String,
    pub language: String,
    pub tier: Tier,
    pub size_kb: u64,
    pub license: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub tools: Tools,
    pub repos: Vec<Repo>,
}

fn invalid(what: impl Into<String>, reason: &'static str) -> CorpusError {
    CorpusError::Invalid {
        what: what.into(),
        reason,
    }
}

impl Manifest {
    /// Parses and validates a manifest. Unknown or missing fields, unpinned
    /// commits, empty tool versions and licenses outside the allowlist fail.
    pub fn parse(text: &str) -> Result<Self, CorpusError> {
        let manifest: Manifest =
            toml::from_str(text).map_err(|e| CorpusError::Parse(e.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn bundled() -> Result<Self, CorpusError> {
        Self::parse(MANIFEST)
    }

    pub fn tier(&self, tier: Tier) -> impl Iterator<Item = &Repo> {
        self.repos.iter().filter(move |r| r.tier == tier)
    }

    fn validate(&self) -> Result<(), CorpusError> {
        let tools = [&self.tools.rg, &self.tools.ctags, &self.tools.tokenizer]
            .into_iter()
            .chain(self.tools.oracles.values());
        if tools.into_iter().any(|v| v.trim().is_empty()) {
            return Err(invalid("tools", "a tool version is empty"));
        }
        for (i, repo) in self.repos.iter().enumerate() {
            let is_hex = repo.commit.bytes().all(|b| b.is_ascii_hexdigit());
            if repo.commit.len() != COMMIT_HEX_LEN || !is_hex {
                return Err(invalid(&repo.name, "commit is not a full hex sha"));
            }
            if !LICENSE_ALLOWLIST.contains(&repo.license.as_str()) {
                return Err(CorpusError::License {
                    repo: repo.name.clone(),
                    license: repo.license.clone(),
                });
            }
            if self.repos[..i].iter().any(|r| r.name == repo.name) {
                return Err(invalid(&repo.name, "name is listed twice"));
            }
        }
        Ok(())
    }
}

/// Puts a checkout of `commit` from `url` into `dest`.
pub trait Transport {
    fn clone_at(&self, url: &str, commit: &str, dest: &Path) -> Result<(), String>;
}

/// Waits between attempts.
pub trait Sleeper {
    fn sleep(&self, duration: Duration);
}

pub struct ThreadSleeper;

impl Sleeper for ThreadSleeper {
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// Fetches one commit with git and never touches submodules.
pub struct GitTransport;

impl GitTransport {
    fn git(dest: &Path, args: &[&str]) -> Result<(), String> {
        let out = Command::new("git")
            .args(["-c", "core.autocrlf=false", "-c", "submodule.recurse=false"])
            .args(args)
            .current_dir(dest)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            return Ok(());
        }
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

impl Transport for GitTransport {
    fn clone_at(&self, url: &str, commit: &str, dest: &Path) -> Result<(), String> {
        fs::create_dir_all(dest).map_err(|e| e.to_string())?;
        Self::git(dest, &["init", "--quiet"])?;
        Self::git(
            dest,
            &[
                "fetch",
                "--quiet",
                "--depth",
                "1",
                "--no-recurse-submodules",
                url,
                commit,
            ],
        )?;
        Self::git(dest, &["checkout", "--quiet", "--detach", "FETCH_HEAD"])
    }
}

fn backoff(failed_attempts: u32) -> Duration {
    BACKOFF_BASE * 2u32.pow(failed_attempts - 1)
}

/// Returns the checkout directory for `repo` under `cache`, cloning only when
/// the commit is not cached yet.
pub fn fetch(
    repo: &Repo,
    cache: &Path,
    transport: &dyn Transport,
    sleeper: &dyn Sleeper,
) -> Result<PathBuf, CorpusError> {
    let ready = cache.join(&repo.commit);
    if ready.is_dir() {
        return Ok(ready);
    }
    let partial = cache.join(format!("{}.partial", repo.commit));
    let mut last = String::new();
    for attempt in 1..=MAX_FETCH_ATTEMPTS {
        let _ = fs::remove_dir_all(&partial);
        match transport.clone_at(&repo.url, &repo.commit, &partial) {
            Ok(()) => {
                if let Some(path) = find_crlf(&partial)? {
                    let _ = fs::remove_dir_all(&partial);
                    return Err(CorpusError::Crlf {
                        repo: repo.name.clone(),
                        path,
                    });
                }
                fs::rename(&partial, &ready)?;
                return Ok(ready);
            }
            Err(e) => last = e,
        }
        if attempt < MAX_FETCH_ATTEMPTS {
            sleeper.sleep(backoff(attempt));
        }
    }
    let _ = fs::remove_dir_all(&partial);
    Err(CorpusError::Fetch {
        repo: repo.name.clone(),
        attempts: MAX_FETCH_ATTEMPTS,
        last,
    })
}

/// The first text file under `root` that holds a CRLF pair, outside `.git`.
fn find_crlf(root: &Path) -> Result<Option<String>, CorpusError> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let data = fs::read(&path)?;
            let probe = &data[..data.len().min(NUL_PROBE_BYTES)];
            if !probe.contains(&0) && data.windows(2).any(|w| w == b"\r\n") {
                let rel = path.strip_prefix(root).unwrap_or(&path);
                return Ok(Some(rel.display().to_string()));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokenizer::TOKENIZER_ID;
    use playbook_common::test_support::scratch_dir;
    use std::cell::{Cell, RefCell};

    const SHA: &str = "4649aa9700619f94cf9c66876e9549d83420e16c";

    fn manifest_text(commit: &str, license: &str, extra: &str) -> String {
        format!(
            "[tools]\nrg = \"1\"\nctags = \"1\"\ntokenizer = \"t\"\n[tools.oracles]\na = \"1\"\n\
             [[repos]]\nname = \"r\"\nurl = \"u\"\ncommit = \"{commit}\"\nlanguage = \"go\"\n\
             tier = \"pilot\"\nsize_kb = 1\nlicense = \"{license}\"\n{extra}"
        )
    }

    fn repo() -> Repo {
        Manifest::parse(&manifest_text(SHA, "MIT", ""))
            .unwrap()
            .repos
            .remove(0)
    }

    struct Counting {
        fail_first: u32,
        calls: Cell<u32>,
        files: Vec<(&'static str, &'static [u8])>,
    }

    impl Counting {
        fn ok() -> Self {
            Self {
                fail_first: 0,
                calls: Cell::new(0),
                files: vec![("a.rs", b"fn a() {}\n")],
            }
        }
    }

    impl Transport for Counting {
        fn clone_at(&self, _: &str, _: &str, dest: &Path) -> Result<(), String> {
            self.calls.set(self.calls.get() + 1);
            if self.calls.get() <= self.fail_first {
                return Err("network down".into());
            }
            fs::create_dir_all(dest).map_err(|e| e.to_string())?;
            for (name, body) in &self.files {
                fs::write(dest.join(name), body).map_err(|e| e.to_string())?;
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct Recording(RefCell<Vec<Duration>>);

    impl Sleeper for Recording {
        fn sleep(&self, duration: Duration) {
            self.0.borrow_mut().push(duration);
        }
    }

    #[test]
    fn bundled_manifest_loads_both_tiers() {
        let m = Manifest::bundled().unwrap();
        assert_eq!(m.tier(Tier::Pilot).count(), 3);
        assert_eq!(m.tier(Tier::Full).count(), 6);
        assert_eq!(m.tools.tokenizer, TOKENIZER_ID);
    }

    #[test]
    fn unknown_field_fails() {
        let text = manifest_text(SHA, "MIT", "extra = 1\n");
        assert!(matches!(Manifest::parse(&text), Err(CorpusError::Parse(_))));
    }

    #[test]
    fn unpinned_commit_fails() {
        let text = manifest_text("main", "MIT", "");
        assert!(matches!(
            Manifest::parse(&text),
            Err(CorpusError::Invalid { .. })
        ));
    }

    #[test]
    fn license_outside_allowlist_fails() {
        let text = manifest_text(SHA, "GPL-3.0-only", "");
        assert_eq!(
            Manifest::parse(&text).unwrap_err(),
            CorpusError::License {
                repo: "r".into(),
                license: "GPL-3.0-only".into()
            }
        );
    }

    #[test]
    fn every_allowlisted_license_loads() {
        for license in LICENSE_ALLOWLIST {
            assert!(
                Manifest::parse(&manifest_text(SHA, license, "")).is_ok(),
                "{license}"
            );
        }
    }

    #[test]
    fn fetch_succeeds_after_failures_below_the_limit() {
        let cache = scratch_dir("fetch-retry");
        let transport = Counting {
            fail_first: MAX_FETCH_ATTEMPTS - 1,
            ..Counting::ok()
        };
        let sleeper = Recording::default();
        let dir = fetch(&repo(), &cache, &transport, &sleeper).unwrap();
        assert!(dir.join("a.rs").is_file());
        assert_eq!(transport.calls.get(), MAX_FETCH_ATTEMPTS);
        assert_eq!(*sleeper.0.borrow(), vec![BACKOFF_BASE, BACKOFF_BASE * 2]);
    }

    #[test]
    fn fetch_fails_after_the_attempt_limit_and_names_the_repo() {
        let cache = scratch_dir("fetch-fail");
        let transport = Counting {
            fail_first: MAX_FETCH_ATTEMPTS,
            ..Counting::ok()
        };
        let sleeper = Recording::default();
        let err = fetch(&repo(), &cache, &transport, &sleeper).unwrap_err();
        assert!(err.to_string().contains("r"));
        assert!(matches!(
            err,
            CorpusError::Fetch {
                attempts: MAX_FETCH_ATTEMPTS,
                ..
            }
        ));
        assert_eq!(transport.calls.get(), MAX_FETCH_ATTEMPTS);
        assert_eq!(sleeper.0.borrow().len() as u32, MAX_FETCH_ATTEMPTS - 1);
    }

    #[test]
    fn crlf_checkout_fails_and_is_not_cached() {
        let cache = scratch_dir("fetch-crlf");
        let transport = Counting {
            files: vec![("w.txt", b"a\r\nb\r\n")],
            ..Counting::ok()
        };
        let err = fetch(&repo(), &cache, &transport, &Recording::default()).unwrap_err();
        assert!(matches!(err, CorpusError::Crlf { ref path, .. } if path == "w.txt"));
        assert!(!cache.join(SHA).exists());
    }

    #[test]
    fn second_fetch_of_the_same_commit_uses_the_cache() {
        let cache = scratch_dir("fetch-cache");
        let transport = Counting::ok();
        let sleeper = Recording::default();
        fetch(&repo(), &cache, &transport, &sleeper).unwrap();
        fetch(&repo(), &cache, &transport, &sleeper).unwrap();
        assert_eq!(transport.calls.get(), 1);
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "protocol.file.allow=always",
            ])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn git_transport_leaves_submodules_unfetched() {
        let base = scratch_dir("fetch-sub");
        let (inner, outer) = (base.join("inner"), base.join("outer"));
        for dir in [&inner, &outer] {
            fs::create_dir_all(dir).unwrap();
            git(dir, &["init", "--quiet"]);
        }
        fs::write(inner.join("lib.rs"), "fn lib() {}\n").unwrap();
        git(&inner, &["add", "."]);
        git(&inner, &["commit", "--quiet", "-m", "inner"]);
        fs::write(outer.join("main.rs"), "fn main() {}\n").unwrap();
        git(
            &outer,
            &[
                "submodule",
                "add",
                "--quiet",
                inner.to_str().unwrap(),
                "dep",
            ],
        );
        git(&outer, &["add", "."]);
        git(&outer, &["commit", "--quiet", "-m", "outer"]);
        let commit = git(&outer, &["rev-parse", "HEAD"]);

        let dest = base.join("checkout");
        GitTransport
            .clone_at(outer.to_str().unwrap(), &commit, &dest)
            .unwrap();
        assert!(dest.join("main.rs").is_file());
        assert!(dest.join(".gitmodules").is_file());
        assert!(!dest.join("dep").join("lib.rs").exists());
    }
}
