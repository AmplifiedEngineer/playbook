// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! `install.sh` resolving and installing a release: which ref it picks, how it
//! fails on API errors, checksum and version checks, the platform check, and
//! the build attestation modes. The real script runs against stubbed `curl`,
//! `uname` and `gh`.

#[path = "support/install_stubs.rs"]
mod install_stubs;
#[path = "support/shell_exec.rs"]
mod shell_exec;
#[path = "support/shell_fixture.rs"]
mod shell_fixture;
#[path = "support/shell_run.rs"]
mod shell_run;

use install_stubs::{install_stubs, sha256_hex};
use shell_fixture::Work;
use shell_run::{both, repo_root};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const RELEASE_BODY: &str = r#"{"tag_name": "v1.2.3", "name": "release"}"#;
const ASSET: &str = "playbook-1.2.3-x86_64-unknown-linux-musl";
const GOOD_ASSET: &str = "#!/usr/bin/env bash\necho \"playbook 1.2.3\"\n";

struct Ctx {
    work: Work,
    stubs: PathBuf,
}

impl Ctx {
    fn new(tag: &str) -> Ctx {
        let work = Work::new(tag);
        let stubs = work.dir("stubs");
        install_stubs(&stubs);
        Ctx { work, stubs }
    }

    fn path(&self) -> String {
        format!(
            "{}:{}",
            self.stubs.display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }

    /// Run `install.sh --no-init` in a fresh home with `envs`. Returns
    /// (exit code, combined output, home, bin dir).
    fn install(&self, name: &str, envs: &[(&str, &str)]) -> (i32, String, PathBuf, PathBuf) {
        let home = self.work.dir(&format!("home-{name}"));
        let bindir = self.work.path(&format!("bin-{name}"));
        let mut cmd = Command::new("bash");
        cmd.arg(repo_root().join("install.sh"))
            .arg("--no-init")
            .env_remove("PLAYBOOK_REQUIRE_ATTESTATION")
            .env_remove("STUB_GH")
            .env_remove("GH_LOG")
            .env("PATH", self.path())
            .env("CLAUDE_HOME", home.join(".claude"))
            .env("HOME", &home)
            .env("PLAYBOOK_BIN_DIR", &bindir)
            .env("SHELL", "/bin/bash");
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("bash spawns");
        (out.status.code().unwrap_or(-1), both(&out), home, bindir)
    }

    /// A full release install with the good asset and a matching checksum.
    fn good(&self, name: &str, extra: &[(&str, &str)]) -> (i32, String, PathBuf, PathBuf) {
        let sums = format!("{}  {ASSET}", sha256_hex(GOOD_ASSET.as_bytes()));
        let mut envs = vec![
            ("STUB_CODE", "200"),
            ("STUB_BODY", RELEASE_BODY),
            ("STUB_ASSET_BODY", GOOD_ASSET),
            ("STUB_SUMS_BODY", sums.as_str()),
        ];
        envs.extend_from_slice(extra);
        self.install(name, &envs)
    }
}

#[test]
fn a_published_release_resolves_to_its_tag() {
    let c = Ctx::new("res-release");
    let (_, out, ..) = c.install("a", &[("STUB_CODE", "200"), ("STUB_BODY", RELEASE_BODY)]);
    assert!(out.contains("Installing release v1.2.3"), "{out}");
}

#[test]
fn no_release_404_is_refused() {
    let c = Ctx::new("res-404");
    let (_, out, ..) = c.install(
        "a",
        &[
            ("STUB_CODE", "404"),
            ("STUB_BODY", r#"{"message":"Not Found"}"#),
        ],
    );
    assert!(
        out.contains("published no release") && out.contains("cannot be fetched"),
        "{out}"
    );
}

#[test]
fn a_rate_limit_403_aborts_and_never_installs_anything() {
    let c = Ctx::new("res-403");
    let (rc, out, _, bindir) = c.install(
        "a",
        &[
            ("STUB_CODE", "403"),
            ("STUB_BODY", r#"{"message":"API rate limit exceeded"}"#),
        ],
    );
    assert!(
        rc != 0 && !out.contains("Installing release") && out.contains("HTTP 403"),
        "{out}"
    );
    assert!(!bindir.join("playbook").exists());
}

#[test]
fn a_transport_failure_aborts_and_never_installs_anything() {
    let c = Ctx::new("res-transport");
    let (rc, out, _, bindir) = c.install("a", &[("STUB_TRANSPORT_FAIL", "1")]);
    assert!(
        rc != 0 && !out.contains("Installing release") && out.contains("HTTP 000"),
        "{out}"
    );
    assert!(!bindir.join("playbook").exists());
}

#[test]
fn a_server_error_503_aborts_and_never_installs_anything() {
    let c = Ctx::new("res-503");
    let (rc, out, _, bindir) = c.install("a", &[("STUB_CODE", "503"), ("STUB_BODY", "")]);
    assert!(
        rc != 0 && !out.contains("Installing release") && out.contains("HTTP 503"),
        "{out}"
    );
    assert!(!bindir.join("playbook").exists());
}

#[test]
fn a_200_without_tag_name_refuses_to_guess() {
    let c = Ctx::new("res-notag");
    let (_, out, ..) = c.install(
        "a",
        &[
            ("STUB_CODE", "200"),
            ("STUB_BODY", r#"{"message":"weird"}"#),
        ],
    );
    assert!(
        !out.contains("Installing release") && out.contains("no tag_name"),
        "{out}"
    );
}

#[test]
fn a_branch_ref_pin_skips_the_api_and_refuses_a_binary() {
    let c = Ctx::new("res-pin");
    let (_, out, ..) = c.install("a", &[("PLAYBOOK_REF", "my-branch"), ("STUB_CODE", "403")]);
    assert!(
        out.contains("pins a branch or commit") && !out.contains("HTTP 403"),
        "{out}"
    );
}

#[test]
fn a_release_tag_pin_installs_that_binary_without_the_api() {
    let c = Ctx::new("res-tag-pin");
    let sums = format!("{}  {ASSET}", sha256_hex(GOOD_ASSET.as_bytes()));
    let (_, out, _, bindir) = c.install(
        "a",
        &[
            ("PLAYBOOK_REF", "v1.2.3"),
            ("STUB_CODE", "403"),
            ("STUB_ASSET_BODY", GOOD_ASSET),
            ("STUB_SUMS_BODY", &sums),
        ],
    );
    assert!(
        out.contains("Installed playbook 1.2.3")
            && !out.contains("pins a branch")
            && !out.contains("HTTP 403"),
        "{out}"
    );
    assert!(bindir.join("playbook").exists());
}

#[test]
fn a_release_tag_pin_without_a_release_installs_nothing() {
    let c = Ctx::new("res-tag-missing");
    let (rc, _, _, bindir) = c.install(
        "a",
        &[
            ("PLAYBOOK_REF", "v9.9.9"),
            ("STUB_ASSET_BODY", ""),
            ("STUB_SUMS_BODY", ""),
        ],
    );
    assert!(rc != 0);
    assert!(!bindir.join("playbook").exists());
}

#[test]
fn a_corrupted_download_fails_the_checksum_and_installs_nothing() {
    let c = Ctx::new("res-cksum");
    let zeros = format!("{:064}  {ASSET}", 0);
    let (rc, out, home, bindir) = c.install(
        "a",
        &[
            ("STUB_CODE", "200"),
            ("STUB_BODY", RELEASE_BODY),
            ("STUB_ASSET_BODY", GOOD_ASSET),
            ("STUB_SUMS_BODY", &zeros),
        ],
    );
    assert!(rc != 0 && out.contains("checksum mismatch"), "{out}");
    assert!(!home.join(".claude").exists() && !bindir.join("playbook").exists());
}

#[test]
fn sha256sums_served_as_an_html_error_page_is_refused() {
    let c = Ctx::new("res-html");
    let (rc, out, home, bindir) = c.install(
        "a",
        &[
            ("STUB_CODE", "200"),
            ("STUB_BODY", RELEASE_BODY),
            ("STUB_ASSET_BODY", GOOD_ASSET),
            ("STUB_SUMS_BODY", "<html><body>404: Not Found</body></html>"),
        ],
    );
    assert!(rc != 0 && out.contains("no checksum line"), "{out}");
    assert!(!home.join(".claude").exists() && !bindir.join("playbook").exists());
}

#[test]
fn a_zero_byte_binary_with_a_matching_checksum_is_refused() {
    let c = Ctx::new("res-zero");
    let empty = format!("{}  {ASSET}", sha256_hex(b""));
    let (rc, out, _, bindir) = c.install(
        "a",
        &[
            ("STUB_CODE", "200"),
            ("STUB_BODY", RELEASE_BODY),
            ("STUB_ASSET_BODY", ""),
            ("STUB_SUMS_BODY", &empty),
        ],
    );
    assert!(rc != 0 && !out.contains("checksum mismatch"), "{out}");
    assert!(
        out.contains("did not run") || out.contains("version mismatch"),
        "{out}"
    );
    assert!(!bindir.join("playbook").exists());
}

#[test]
fn a_binary_reporting_a_different_version_than_the_tag_is_refused() {
    let c = Ctx::new("res-version");
    let body = "#!/usr/bin/env bash\necho \"playbook 9.9.9\"\n";
    let sums = format!("{}  {ASSET}", sha256_hex(body.as_bytes()));
    let (rc, out, _, bindir) = c.install(
        "a",
        &[
            ("STUB_CODE", "200"),
            ("STUB_BODY", RELEASE_BODY),
            ("STUB_ASSET_BODY", body),
            ("STUB_SUMS_BODY", &sums),
        ],
    );
    assert!(rc != 0 && out.contains("version mismatch"), "{out}");
    assert!(!bindir.join("playbook").exists());
}

#[test]
fn an_unsupported_platform_names_itself_and_the_windows_asset() {
    let c = Ctx::new("res-platform");
    let (rc, out, ..) = c.install(
        "a",
        &[
            ("STUB_CODE", "200"),
            ("STUB_BODY", RELEASE_BODY),
            ("STUB_UNAME_S", "SunOS"),
            ("STUB_UNAME_M", "sparc64"),
        ],
    );
    assert!(rc != 0, "{out}");
    assert!(
        out.contains("SunOS") && out.contains("sparc64") && out.contains("windows-msvc.exe"),
        "{out}"
    );
}

#[test]
fn without_shasum_or_sha256sum_it_stops_before_any_download() {
    let c = Ctx::new("res-nosum");
    let dir = c.work.dir("no-cksum-bin");
    for tool in ["bash", "tar"] {
        if let Some(real) = std::env::var_os("PATH").and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join(tool))
                .find(|f| f.is_file())
        }) {
            #[cfg(unix)]
            let _ = std::os::unix::fs::symlink(real, dir.join(tool));
        }
    }
    fs::copy(c.stubs.join("curl"), dir.join("curl")).unwrap();
    let home = c.work.dir("home-nosum");
    let curl_log = c.work.path("no-cksum-curl.log");
    let out = Command::new("bash")
        .arg(repo_root().join("install.sh"))
        .arg("--no-init")
        .env("PATH", &dir)
        .env("CLAUDE_HOME", home.join(".claude"))
        .env("HOME", &home)
        .env("PLAYBOOK_BIN_DIR", c.work.path("bin-nosum"))
        .env("SHELL", "/bin/bash")
        .env("CURL_LOG", &curl_log)
        .output()
        .unwrap();
    let text = both(&out);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("shasum or sha256sum is required"), "{text}");
    assert!(!curl_log.exists(), "curl was called");
    assert!(!home.join(".claude").exists());
}

#[test]
fn a_successful_install_places_the_binary_and_wires_path_once() {
    let c = Ctx::new("res-ok");
    let (_, out, home, bindir) = c.good("a", &[]);
    // A second run must not add a second PATH line.
    let sums = format!("{}  {ASSET}", sha256_hex(GOOD_ASSET.as_bytes()));
    let second = Command::new("bash")
        .arg(repo_root().join("install.sh"))
        .arg("--no-init")
        .env("PATH", c.path())
        .env("CLAUDE_HOME", home.join(".claude"))
        .env("HOME", &home)
        .env("PLAYBOOK_BIN_DIR", &bindir)
        .env("SHELL", "/bin/bash")
        .env("STUB_CODE", "200")
        .env("STUB_BODY", RELEASE_BODY)
        .env("STUB_ASSET_BODY", GOOD_ASSET)
        .env("STUB_SUMS_BODY", sums)
        .output()
        .expect("bash spawns");
    assert!(second.status.success(), "{}", both(&second));
    let bashrc = fs::read_to_string(home.join(".bashrc")).unwrap_or_default();
    let lines = bashrc
        .lines()
        .filter(|l| l.contains(&bindir.display().to_string()))
        .count();
    assert!(bindir.join("playbook").exists());
    assert_eq!(lines, 1, "{bashrc}");
    assert!(out.contains("Installed playbook 1.2.3"), "{out}");
}

fn attest(tag: &str, extra: &[(&str, &str)]) -> (i32, String, String, PathBuf) {
    let c = Ctx::new(tag);
    let home_probe = c.work.dir("probe");
    let log = home_probe.join("gh.log");
    let log_s = log.display().to_string();
    let mut envs: Vec<(&str, &str)> = vec![("GH_LOG", &log_s)];
    envs.extend_from_slice(extra);
    let (rc, out, _, bindir) = c.good("a", &envs);
    let gh = fs::read_to_string(&log).unwrap_or_default();
    (rc, out, gh, bindir)
}

#[test]
fn attestation_passes_and_names_the_repo_and_signer_workflow() {
    let (_, out, gh, _) = attest("att-pass", &[("STUB_GH", "pass")]);
    assert!(out.contains("Verified the build attestation"), "{out}");
    assert!(
        gh.contains(&format!("/{ASSET} --repo amplifiedengineer/playbook")),
        "{gh}"
    );
    assert!(
        gh.contains("--signer-workflow amplifiedengineer/playbook/.github/workflows/release.yml"),
        "{gh}"
    );
    assert!(!gh.contains("pragmatic-engineer"), "{gh}");
}

#[test]
fn a_release_attested_under_the_second_org_name_still_verifies() {
    let (rc, out, gh, _) = attest("att-middle", &[("STUB_GH", "middle")]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("Verified the build attestation"), "{out}");
    assert!(gh.contains("--repo amplified-engineer/playbook"), "{gh}");
}

#[test]
fn a_release_attested_under_the_old_org_still_verifies() {
    let (rc, out, gh, _) = attest("att-legacy", &[("STUB_GH", "legacy")]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("Verified the build attestation"), "{out}");
    let new = gh
        .find("--repo amplifiedengineer/playbook")
        .expect("new repo tried");
    let old = gh
        .find("--signer-workflow pragmatic-engineer/playbook/.github/workflows/release.yml")
        .expect("old repo tried");
    assert!(new < old, "the new repo comes first: {gh}");
}

#[test]
fn an_attestation_failure_is_fatal_and_installs_nothing() {
    let (rc, out, _, bindir) = attest("att-fail", &[("STUB_GH", "fail")]);
    assert!(
        rc != 0 && out.contains("attestation verification failed"),
        "{out}"
    );
    assert!(
        out.contains("no matching attestation") && !out.contains("Installed playbook"),
        "{out}"
    );
    assert!(!bindir.join("playbook").exists());
}

#[test]
fn a_release_without_attestations_warns_and_installs() {
    let (_, out, ..) = attest("att-missing", &[("STUB_GH", "missing")]);
    assert!(
        out.contains("provenance not verified: no attestations found"),
        "{out}"
    );
    assert!(out.contains("Installed playbook 1.2.3"), "{out}");
}

#[test]
fn a_release_without_attestations_is_fatal_when_strict() {
    let (rc, out, ..) = attest(
        "att-missing-strict",
        &[
            ("STUB_GH", "missing"),
            ("PLAYBOOK_REQUIRE_ATTESTATION", "1"),
        ],
    );
    assert!(
        rc != 0 && out.contains("attestation verification failed"),
        "{out}"
    );
    assert!(!out.contains("Installed playbook"), "{out}");
}

#[test]
fn without_gh_it_notes_checksum_only_and_installs() {
    let (_, out, ..) = attest("att-absent", &[("STUB_GH", "absent")]);
    assert!(out.contains("provenance not verified"), "{out}");
    assert!(out.contains("Installed playbook 1.2.3"), "{out}");
}

#[test]
fn strict_mode_without_gh_is_fatal() {
    let (rc, out, ..) = attest(
        "att-strict-absent",
        &[("STUB_GH", "absent"), ("PLAYBOOK_REQUIRE_ATTESTATION", "1")],
    );
    assert!(
        rc != 0 && out.contains("PLAYBOOK_REQUIRE_ATTESTATION=1"),
        "{out}"
    );
    assert!(!out.contains("Installed playbook"), "{out}");
}

#[test]
fn strict_mode_with_a_passing_attestation_installs() {
    let (_, out, ..) = attest(
        "att-strict-pass",
        &[("STUB_GH", "pass"), ("PLAYBOOK_REQUIRE_ATTESTATION", "1")],
    );
    assert!(out.contains("Installed playbook 1.2.3"), "{out}");
}
