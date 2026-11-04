#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# package-repo-fetch.sh: downloads the newest releases that carry packages and
# checks each one against its attestation (ADR 0052).
#
# Usage: package-repo-fetch.sh <assets-root> [<count>]
#
# Writes <assets-root>/X.Y.Z/ for each of the newest <count> (default 5)
# stable releases whose SHA256SUMS lists a .deb or .rpm. It checks the
# attestation of SHA256SUMS first, then the hash and attestation of every
# listed package. Prints the publish time of the newest one as epoch seconds on
# the last line of stdout. Needs GH_TOKEN, GH_REPO and SOURCE_REF.
set -euo pipefail

fail() { echo "FAIL $*" >&2; exit 1; }
root="${1:-}" count="${2:-5}"
[ -n "$root" ] || { echo "usage: package-repo-fetch.sh <assets-root> [<count>]" >&2; exit 2; }
: "${GH_REPO:?}" "${SOURCE_REF:?}"
mkdir -p "$root"
signer="$GH_REPO/.github/workflows/release.yml"
kept=0 newest=""

while IFS=$'\t' read -r tag published; do
  [ "$kept" -lt "$count" ] || break
  v="${tag#v}"
  dir="$root/$v"
  mkdir -p "$dir"
  rc=0
  (
    cd "$dir"
    gh release download "$tag" --pattern SHA256SUMS
    gh attestation verify SHA256SUMS --repo "$GH_REPO" --source-ref "$SOURCE_REF" --signer-workflow "$signer" > /dev/null
    grep -E "  playbook(_${v}_(amd64|arm64)\.deb|-${v}\.(x86_64|aarch64)\.rpm)\$" SHA256SUMS > packages.sums || true
    if [ ! -s packages.sums ]; then
      echo "release $tag lists no packages, so it is skipped" >&2
      exit 3
    fi
    while read -r _ name; do gh release download "$tag" --pattern "$name"; done < packages.sums
    sha256sum -c packages.sums > /dev/null
    while read -r _ name; do
      gh attestation verify "$name" --repo "$GH_REPO" --source-ref "$SOURCE_REF" --signer-workflow "$signer" > /dev/null
    done < packages.sums
    rm packages.sums
  ) || rc=$?
  if [ "$rc" -eq 3 ]; then rm -rf "$dir"; continue; fi
  [ "$rc" -eq 0 ] || fail "release $tag did not verify"
  [ -n "$newest" ] || newest="$published"
  kept=$((kept + 1))
done < <(gh release list --exclude-drafts --exclude-pre-releases --limit 30 --json tagName,publishedAt \
  --jq '.[] | [.tagName, .publishedAt] | @tsv')

[ "$kept" -gt 0 ] || fail "no release with packages was found"
date -u -d "$newest" +%s
