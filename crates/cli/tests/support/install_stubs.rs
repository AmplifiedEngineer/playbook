// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The `curl`, `uname` and `gh` stubs `install.sh` is driven with, and the
//! hash a stubbed release asset is checked against.

use super::shell_exec::write_exec;
use std::path::Path;

const CURL_STUB: &str = r#"out=""; url=""; fail_on_http_error=0
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    -w|--connect-timeout|--max-time) shift 2 ;;
    -*) case "$1" in *f*) fail_on_http_error=1 ;; esac; shift ;;
    *)  url="$1"; shift ;;
  esac
done
if [ -n "${CURL_LOG:-}" ]; then printf '%s\n' "$url" >> "$CURL_LOG"; fi
case "$url" in
  *api.github.com/repos/*/releases/latest)
    if [ "${STUB_TRANSPORT_FAIL:-0}" = "1" ]; then exit 7; fi
    code="${STUB_CODE:-200}"
    if [ "$fail_on_http_error" = "1" ] && [ "$code" -ge 400 ]; then exit 22; fi
    if [ -n "$out" ]; then
      printf '%s' "${STUB_BODY:-}" > "$out"
      printf '%s' "$code"
    else
      printf '%s' "${STUB_BODY:-}"
    fi
    exit 0 ;;
  *raw.githubusercontent.com/*/install.sh)
    [ -n "${STUB_INSTALLER_FILE:-}" ] || exit 22
    if [ -n "$out" ]; then cp "$STUB_INSTALLER_FILE" "$out"; else cat "$STUB_INSTALLER_FILE"; fi
    exit 0 ;;
  */releases/download/*/SHA256SUMS)
    if [ -n "$out" ]; then printf '%s' "${STUB_SUMS_BODY:-}" > "$out"
    else printf '%s' "${STUB_SUMS_BODY:-}"; fi
    exit 0 ;;
  */releases/download/*/release-notes.json)
    if [ -z "${STUB_NOTES_BODY:-}" ]; then exit 22; fi
    if [ -n "$out" ]; then printf '%s' "$STUB_NOTES_BODY" > "$out"
    else printf '%s' "$STUB_NOTES_BODY"; fi
    exit 0 ;;
  */releases/download/*)
    if [ -n "${STUB_ASSET_FILE:-}" ] && [ -n "$out" ]; then cp "$STUB_ASSET_FILE" "$out"; exit 0; fi
    if [ -n "$out" ]; then printf '%s' "${STUB_ASSET_BODY:-}" > "$out"
    else printf '%s' "${STUB_ASSET_BODY:-}"; fi
    exit 0 ;;
  *) exit 22 ;;
esac
"#;

const UNAME_STUB: &str = r#"case "$1" in
  -s) printf '%s\n' "${STUB_UNAME_S:-Linux}" ;;
  -m) printf '%s\n' "${STUB_UNAME_M:-x86_64}" ;;
  *) command -p uname "$@" ;;
esac
"#;

const GH_STUB: &str = r#"case "${STUB_GH:-absent}" in
  absent) exit 127 ;;
  *) [ "$1 $2" = "attestation --help" ] && exit 0
     [ "$1 $2" = "attestation verify" ] || exit 1
     echo "gh $*" >> "${GH_LOG:-/dev/null}"
     [ "$STUB_GH" = pass ] && exit 0
     [ "$STUB_GH" = middle ] && { case "$*" in *"--repo amplified-engineer/"*) exit 0 ;; esac; echo "no matching attestation" >&2; exit 1; }
     [ "$STUB_GH" = legacy ] && { case "$*" in *"--repo pragmatic-engineer/"*) exit 0 ;; esac; echo "no matching attestation" >&2; exit 1; }
     [ "$STUB_GH" = missing ] && { echo "no attestations found for subject" >&2; exit 1; }
     echo "no matching attestation" >&2; exit 1 ;;
esac
"#;
/// Write the `curl`, `uname` and `gh` stubs `install.sh` is driven with into
/// `dir`. `curl` serves the releases API, the release asset and SHA256SUMS from
/// `STUB_*` env vars (`STUB_ASSET_FILE` serves a file, for a binary asset such
/// as a tarball, and `STUB_INSTALLER_FILE` serves install.sh from
/// raw.githubusercontent.com) and fails on anything else, `uname` is fixed by
/// `STUB_UNAME_S` and `STUB_UNAME_M`, and `gh` plays the attestation check.
pub(crate) fn install_stubs(dir: &Path) {
    write_exec(&dir.join("curl"), CURL_STUB);
    write_exec(&dir.join("uname"), UNAME_STUB);
    write_exec(&dir.join("gh"), GH_STUB);
}

/// Lowercase hex SHA-256 of `data`.
pub(crate) fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
