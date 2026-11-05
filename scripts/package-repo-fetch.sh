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
# stable releases whose SHA256SUMS lists a .deb or .rpm. Only vX.Y.Z tags are
# considered. A release whose SHA256SUMS lists no package is skipped before any
# attestation check, because nothing from it is used. A release that lists
# packages must verify strictly: the attestation of SHA256SUMS and of every
# package must name SOURCE_REF (the default branch) and the release workflow,
# and the hashes must match. Any failure there stops the run. Prints the
# publish time of the newest kept release as epoch seconds on the last line of
# stdout. Needs GH_TOKEN, GH_REPO and SOURCE_REF.
set -euo pipefail

fail() { echo "FAIL $*" >&2; exit 1; }
root="${1:-}" count="${2:-5}"
[ -n "$root" ] || { echo "usage: package-repo-fetch.sh <assets-root> [<count>]" >&2; exit 2; }
: "${GH_REPO:?}" "${SOURCE_REF:?}"
mkdir -p "$root"
signer="$GH_REPO/.github/workflows/release.yml"
kept=0 newest=""

verify() { gh attestation verify "$1" --repo "$GH_REPO" --source-ref "$SOURCE_REF" --signer-workflow "$signer" > /dev/null; }

while IFS=$'\t' read -r tag published; do
  [ "$kept" -lt "$count" ] || break
  case "$tag" in
    v[0-9]*.[0-9]*.[0-9]*) ;;
    *) continue ;;
  esac
  v="${tag#v}"
  dir="$root/$v"
  mkdir -p "$dir"
  rc=0
  # The subshell runs inside `||`, where bash ignores `set -e`. Every step that
  # can fail therefore ends in an explicit `|| exit`, so a failed check can
  # never be mistaken for a pass. Exit 3 means "lists no packages, skip".
  (
    cd "$dir" || exit 1
    gh release download "$tag" --pattern SHA256SUMS || exit 1
    grep -E "  playbook(_${v}_(amd64|arm64)\.deb|-${v}\.(x86_64|aarch64)\.rpm)\$" SHA256SUMS > packages.sums || true
    if [ ! -s packages.sums ]; then
      echo "release $tag lists no packages, so it is skipped" >&2
      exit 3
    fi
    verify SHA256SUMS || { echo "attestation of SHA256SUMS in $tag does not match $SOURCE_REF" >&2; exit 1; }
    while read -r _ name; do gh release download "$tag" --pattern "$name" || exit 1; done < packages.sums
    sha256sum -c packages.sums > /dev/null || exit 1
    while read -r _ name; do
      verify "$name" || { echo "attestation of $name in $tag does not match $SOURCE_REF" >&2; exit 1; }
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
