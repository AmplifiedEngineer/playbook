#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# Installer for the playbook binary. It fetches the release binary, verifies it
# (SHA256, the build attestation when gh can check it, then a --version smoke
# test), installs it into PLAYBOOK_BIN_DIR (default $HOME/.local/bin) and puts
# that directory on PATH. Then it runs `playbook migrate`, which updates only
# playbook's own files. It touches nothing else. The skills are a separate
# Claude Code plugin that works without this binary, and this script never
# installs or removes a plugin.
#
# At the end it offers to run `playbook setup`, which adds playbook's hook
# entries to ~/.claude/settings.json, the status line, the shell
# integration and the Claude Code telemetry variables that point at a collector
# on this machine. It asks first, and with no terminal and no --yes it only prints
# the command, so nothing is wired without a yes.
#
# When it replaces an older binary, it prints a line with the release notes
# link and the breaking changes of the release (ADR 0037).
#
#   curl -fsSL https://raw.githubusercontent.com/pragmatic-engineer/playbook/main/install.sh | bash
#
# The binary comes from the latest GitHub release, or from the release named by
# PLAYBOOK_REF (a tag such as v1.2.3). A branch or a commit has no published
# binary, so it is refused.
#
# install-pbtop.sh runs this script with PLAYBOOK_INSTALL_ONLY=pbtop, which
# installs only pbtop, the standalone Agent Budget Dashboard, from the
# pbtop-<version>-<target>.tar.gz asset of the same release, verified the same
# way. It then stops: no migrate, no setup. The --pbtop flag does the same
# for one more release.
set -euo pipefail

RELEASE_REPO="pragmatic-engineer/playbook"
# Root for every directory playbook itself writes (memory, runtime state,
# the statusline script); mirrors playbook_root_from() in
# crates/common/src/paths.rs. Not overridable by env var: nothing downstream
# of this script reads such an override. The Claude Code folder is fixed too,
# because `playbook setup` always uses ~/.claude.
SKILLS_INSTALL_CMD="claude plugin marketplace add pragmatic-engineer/marketplace"
SKILLS_INSTALL_CMD="$SKILLS_INSTALL_CMD && claude plugin install playbook@pragmatic-engineer"
PLAYBOOK_CONFIG_DIR="$HOME/.config/playbook"
PLAYBOOK_BIN_DIR="${PLAYBOOK_BIN_DIR:-$HOME/.local/bin}"
REF="${PLAYBOOK_REF:-}"
OPT_SYSTEM_PROMPT=0
OPT_NO_SYSTEM_PROMPT=0
ASSUME_YES=0
BINARY_ONLY=0
NO_INIT=0
ONLY_PBTOP=0
[ "${PLAYBOOK_INSTALL_ONLY:-}" = pbtop ] && ONLY_PBTOP=1

if [ -t 1 ]; then
    C_B=$'\033[1;34m'; C_Y=$'\033[1;33m'; C_R=$'\033[1;31m'; C_0=$'\033[0m'
else
    C_B=""; C_Y=""; C_R=""; C_0=""
fi
log()  { printf '%s==>%s %s\n' "$C_B" "$C_0" "$*"; }
warn() { printf '%swarning:%s %s\n' "$C_Y" "$C_0" "$*" >&2; }
die()  { printf '%serror:%s %s\n' "$C_R" "$C_0" "$*" >&2; exit 1; }

# have_tty -> true when /dev/tty can really be opened. The device node exists
# on a machine with no controlling terminal (cron, CI), so -e is not enough.
have_tty() { { : < /dev/tty; } 2>/dev/null; }

# ask PROMPT [default]  ->  0 = yes, 1 = no. Default is yes unless the second
# argument is "n". With --yes, or when there is no controlling terminal (a
# non-interactive `curl | bash` in CI, say), the default is taken without
# prompting. Prompts read from /dev/tty so they work under `curl | bash`.
ask() {
    local prompt="$1" def="${2:-Y}" ans hint
    if [ "$ASSUME_YES" -eq 1 ] || ! have_tty; then
        [ "$def" = "n" ] && return 1 || return 0
    fi
    if [ "$def" = "n" ]; then hint="y/N"; else hint="Y/n"; fi
    printf '%s %s[%s]%s ' "$prompt" "$C_Y" "$hint" "$C_0" > /dev/tty
    # End of input (Ctrl-D) or an interrupt is a no, never the default.
    read -r ans < /dev/tty || return 1
    [ -z "$ans" ] && ans="$def"
    case "$ans" in
        [Yy]|[Yy][Ee][Ss]) return 0 ;;
        *) return 1 ;;
    esac
}

# consent PROMPT  ->  0 only on an explicit yes: --yes, or a typed y. A bare
# Enter is a no. With no terminal and no --yes there is nobody to ask, so the
# answer is no. Used for changes to the user's Claude Code settings, which are
# never silent.
consent() {
    if [ "$ASSUME_YES" -eq 1 ]; then return 0; fi
    have_tty || return 1
    ask "$1" n
}

# Marks $PLAYBOOK_CONFIG_DIR as a trusted workspace in ~/.claude.json, so a
# `claude` session started directly inside it (e.g. to inspect or hand-edit
# statusline.sh) does not hit the trust dialog and silently skip statusLine
# execution: Claude Code's own troubleshooting docs name this exact failure
# as "Status line command skipped: workspace trust not accepted", logged via
# `claude --debug`. This does NOT trust the project the user actually works
# in day to day; that is a separate per-cwd entry Claude Code writes itself
# the first time it opens there, and this script has no reliable way to know
# that folder, especially under the documented curl-pipe install path where
# $PWD may be unrelated to any project.
#
# Best-effort: a missing ~/.claude.json is a silent no-op (this script never
# creates Claude Code's own state file), and any failure only warns, never
# fails the install. Uses `playbook trust`, which merges atomically and
# never touches any other project's entry.
trust_playbook_config_dir() {
    local claude_json="$HOME/.claude.json" bin="$PLAYBOOK_BIN_DIR/playbook" err
    [ -f "$claude_json" ] || return 0

    if [ -x "$bin" ] && "$bin" trust --help >/dev/null 2>&1; then
        err="$("$bin" trust "$PLAYBOOK_CONFIG_DIR" 2>&1 >/dev/null || true)"
        if [ -n "$err" ]; then
            warn "could not update $claude_json to trust $PLAYBOOK_CONFIG_DIR ($err); if you ever run claude directly in that folder, accept its trust dialog manually."
        else
            log "Marked $PLAYBOOK_CONFIG_DIR as a trusted workspace"
        fi
        return 0
    fi

    warn "the installed playbook binary has no 'trust' command, so $PLAYBOOK_CONFIG_DIR was not marked trusted; upgrade playbook and run: playbook trust \"$PLAYBOOK_CONFIG_DIR\""
    return 0
}

print_help() {
    cat <<'EOF'
Install the playbook binary.

Usage:
  curl -fsSL https://raw.githubusercontent.com/pragmatic-engineer/playbook/main/install.sh | bash
  curl -fsSL .../install.sh | bash -s -- [flags]
  ./install.sh [flags]

What it does:
  - downloads the release binary, checks its SHA256 (and its build attestation
    when gh can), runs a smoke test, installs it and puts its folder on PATH;
  - when it replaces an older binary, prints the link to the release notes and
    the breaking changes of the release;
  - then asks whether to run `playbook setup`, which adds playbook's hook entries to
    ~/.claude/settings.json (your own entries stay), the status line and the shell
    integration. With no terminal and no --yes it only prints the command.

It does not install the skills. They are a separate Claude Code plugin that works
without the binary:
  claude plugin marketplace add pragmatic-engineer/marketplace
  claude plugin install playbook@pragmatic-engineer

Env:
  PLAYBOOK_REF=<tag>             release to install, such as v1.2.3 (default: latest)
  PLAYBOOK_BIN_DIR=<dir>         binary install dir (default: $HOME/.local/bin)
  PLAYBOOK_REQUIRE_ATTESTATION=1 abort unless the build attestation verifies

Flags:
  --yes              say yes to every question, including running `playbook setup`
  --system-prompt    install the custom system prompt (setup --system-prompt) without asking
  --no-system-prompt keep pb cc on Claude Code's own prompt (setup --no-system-prompt)
  --binary-only      still offers `playbook setup`, but keeps the system prompt off
  --no-init          install the binary only: never ask about `playbook setup`
  --ref <tag>        same as PLAYBOOK_REF
  -h, --help         show this help

Accepted and ignored, so older commands keep working: --skip-plugin, --no-setup,
--skip-deps, --skip-shell, --aliases.
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --yes|-y)        ASSUME_YES=1 ;;
        --skip-plugin|--no-setup|--skip-deps|--skip-shell) ;; # accepted, ignored
        --aliases)       warn "--aliases is retired: playbook cc and pb cc need no shell line" ;;
        --system-prompt) OPT_SYSTEM_PROMPT=1; OPT_NO_SYSTEM_PROMPT=0 ;;
        --no-system-prompt) OPT_NO_SYSTEM_PROMPT=1; OPT_SYSTEM_PROMPT=0 ;;
        --binary-only)   BINARY_ONLY=1 ;;
        --no-init)       NO_INIT=1 ;;
        --pbtop)         ONLY_PBTOP=1
                         warn "--pbtop goes away after the next release; use install-pbtop.sh" ;;
        --skills-only)   die "--skills-only is gone: this script installs only the binary. For the skills run: $SKILLS_INSTALL_CMD" ;;
        --ref)           shift; REF="${1:-}" ;;
        --ref=*)         REF="${1#--ref=}" ;;
        -h|--help)       print_help; exit 0 ;;
        *)               die "unknown option: $1 (try --help)" ;;
    esac
    shift
done

command -v curl >/dev/null 2>&1 || die "curl is required"
# shasum ships on macOS, sha256sum on Linux (including a bare debian:stable-slim
# container, which has no shasum); either verifies the release binary.
if command -v shasum >/dev/null 2>&1; then
    CKSUM_CMD=(shasum -a 256 -c)
elif command -v sha256sum >/dev/null 2>&1; then
    CKSUM_CMD=(sha256sum -c)
else
    die "shasum or sha256sum is required to verify the release binary"
fi

# Only a 404 ("this repo published no release") is treated as "no release". Any
# other failure means we do not know what the latest release is, and guessing
# would pick the wrong release. Hence no -f, which would empty the body and
# collapse 403, 404 and a dead network into one indistinguishable empty
# string, so each gets its own error. The 403 is routine: the unauthenticated API
# allows 60 requests per hour per IP.
#
# Sets two globals rather than printing, so the caller can read the resolved
# tag (needed to name and version the release binary). Called directly, not via
# command substitution, so die()'s exit ends the real script rather than only
# a subshell.
#   RESOLVED_TAG       the release tag to fetch, or $REF
#   RESOLVED_FROM_REF  1 when RESOLVED_TAG is not a confirmed release (a
#                      branch or commit in PLAYBOOK_REF, or no release at all),
#                      which install_release_binary refuses below
resolve_release() {
    if [ -n "$REF" ]; then
        RESOLVED_TAG="$REF"
        # A ref shaped like a release tag (v1.2.3, 1.2.3, v1.2.3-rc.1) is a
        # pin to that release, so its binary is fetched from the release. If
        # no such release exists, the binary download fails with a clear
        # error. Any other ref is a branch or commit, which has no binary.
        if printf '%s' "$REF" | grep -Eq '^v?[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$'; then
            RESOLVED_FROM_REF=0
        else
            RESOLVED_FROM_REF=1
        fi
        return
    fi
    local body code tag
    body="$(mktemp)"
    if ! code="$(curl -sSL -o "$body" -w '%{http_code}' \
        "https://api.github.com/repos/$RELEASE_REPO/releases/latest" 2>/dev/null)"; then
        code="000"
    fi
    tag="$(sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$body" 2>/dev/null || true)"
    tag="${tag%%$'\n'*}"
    rm -f "$body"

    case "$code" in
        200)
            [ -n "$tag" ] || die "the release API returned 200 with no tag_name; refusing to guess a version"
            RESOLVED_TAG="$tag"
            RESOLVED_FROM_REF=0
            ;;
        404)
            RESOLVED_TAG=""
            RESOLVED_FROM_REF=1
            ;;
        *)
            die "could not read the release API (HTTP $code); retry, or pin a version with PLAYBOOK_REF=vX.Y.Z"
            ;;
    esac
}

# One seam for the download transport, so adding a wget fallback later is a
# one-function change. curl only for now: this repo already hard-requires curl
# (see the preflight check above), and tests/install_resolve.rs's stub
# only implements curl.
_fetch() {
    curl -fsSL "$1" -o "$2"
}

# Maps `uname -s`/`uname -m` to the exact asset name a release publishes, into
# the global ASSET. macOS always reports arm64, never aarch64. Under Rosetta,
# `uname -m` still reports x86_64 and the x86_64 binary runs correctly there,
# so no Rosetta special case is needed.
release_asset_name() {
    local version="$1"
    release_target "$version"
    ASSET="playbook-${version}-${TARGET}"
}

# The release target of this machine, into the global TARGET.
release_target() {
    local version="$1" os arch
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os-$arch" in
        Darwin-arm64)  TARGET="aarch64-apple-darwin" ;;
        Darwin-x86_64) TARGET="x86_64-apple-darwin" ;;
        Linux-aarch64) TARGET="aarch64-unknown-linux-musl" ;;
        Linux-x86_64)  TARGET="x86_64-unknown-linux-musl" ;;
        *)
            die "unsupported platform: $os $arch (a Windows binary is published as playbook-${version}-x86_64-pc-windows-msvc.exe, but this is a bash installer and cannot select it)"
            ;;
    esac
}

# Copies the installed binary to playbook.<version>.bak beside it, so a bad
# release can be rolled back with one mv. $1 is the binary about to replace
# it: an identical one needs no backup. Keeps the newest 3 backups. Returns
# non-zero on any failure; the caller decides that is not fatal.
backup_previous_binary() {
    local new_bin="$1"
    local current="$PLAYBOOK_BIN_DIR/playbook"
    [ -f "$current" ] || return 0
    if cmp -s "$current" "$new_bin"; then
        return 0
    fi

    # PREVIOUS_VERSION is read just before this runs, and is a plain token or
    # nothing, so it is safe in a file name.
    local old_version="${PREVIOUS_VERSION:-unknown-$(date -u +%Y%m%dT%H%M%SZ)}"

    local bak="$PLAYBOOK_BIN_DIR/playbook.$old_version.bak"
    if [ -e "$bak" ]; then
        log "Keeping the existing backup $bak"
        return 0
    fi

    local bak_tmp
    bak_tmp="$(mktemp "$PLAYBOOK_BIN_DIR/.playbook.bak.XXXXXX")" || return 1
    if ! { cp "$current" "$bak_tmp" && chmod 0755 "$bak_tmp" && mv -f "$bak_tmp" "$bak"; }; then
        rm -f "$bak_tmp"
        return 1
    fi
    log "Kept the previous binary as $bak. To roll back: mv -f $bak $current"

    # Newest first by modification time; drop everything past the third.
    # shellcheck disable=SC2012 # names are ours: version tokens are validated above
    ls -t "$PLAYBOOK_BIN_DIR"/playbook.*.bak 2>/dev/null | tail -n +4 \
        | while IFS= read -r _old; do
            [ -n "$_old" ] && rm -f "$_old" && log "Removed old backup $_old"
        done \
        || true
}

# The version of the playbook binary already in PLAYBOOK_BIN_DIR, or nothing
# when there is none or it does not answer with a plain token. The backup file
# name uses it too.
installed_version() {
    local current="$PLAYBOOK_BIN_DIR/playbook" v
    [ -x "$current" ] || return 0
    v="$("$current" --version 2>/dev/null </dev/null | tail -n 1 | awk '{print $NF}')" || return 0
    case "$v" in
        ""|*[!A-Za-z0-9._+-]*) return 0 ;;
    esac
    printf '%s' "$v"
}

# show_release_notes OLD NEW: after this script replaced playbook OLD with
# NEW, prints one line and the release notes on stderr (ADR 0037 decision
# 5.4). It downloads release-notes.json, with a short time limit, and checks
# it against the SHA256SUMS already in STAGE. The new binary prints the
# breaking changes from that file, or only the link when the file is missing
# or does not match, and records the notes as shown, so its first run does not
# print them again. A binary older than that step gets the link from here. It
# never fails the install.
show_release_notes() {
    local old="$1" new="$2" text notes="$STAGE/release-notes.json"
    [ -n "$old" ] || return 0
    printf 'playbook updated from %s to %s.\n' "$old" "$new" >&2
    if ! curl -fsSL --connect-timeout 5 --max-time 20 \
            "https://github.com/$RELEASE_REPO/releases/download/$RESOLVED_TAG/release-notes.json" \
            -o "$notes" 2>/dev/null \
        || ! (cd "$STAGE" && grep -E '^[0-9a-f]{64}  release-notes\.json$' SHA256SUMS > release-notes.sha256 \
            && "${CKSUM_CMD[@]}" release-notes.sha256) >/dev/null 2>&1; then
        notes="$STAGE/no-release-notes.json"
    fi
    if text="$("$PLAYBOOK_BIN_DIR/playbook" update --print-notes "$notes" \
            --from "$old" 2>/dev/null </dev/null)" \
        && [ -n "$text" ]; then
        printf '%s\n' "$text" >&2
        return 0
    fi
    printf 'Release notes: https://github.com/%s/releases/tag/%s\n' "$RELEASE_REPO" "$RESOLVED_TAG" >&2
}

# Checks the GitHub build attestation for a downloaded asset. Needs a gh that
# has `attestation`; without one it says so and goes on, unless
# PLAYBOOK_REQUIRE_ATTESTATION=1, which makes a missing or failed check fatal.
verify_attestation() {
    local file="$1" strict="${PLAYBOOK_REQUIRE_ATTESTATION:-0}" err
    if command -v gh >/dev/null 2>&1 && gh attestation --help >/dev/null 2>&1; then
        if err="$(gh attestation verify "$file" --repo "$RELEASE_REPO" \
            --signer-workflow "$RELEASE_REPO/.github/workflows/release.yml" 2>&1)"; then
            log "Verified the build attestation for $ASSET"
            return 0
        fi
        # Older releases and a gh that is not logged in cannot be verified; that
        # is a gap, not a mismatch, so only strict mode treats it as fatal.
        if [ "$strict" != "1" ] && printf '%s' "$err" | grep -qiE 'no attestations found|failed to fetch|gh auth login|GH_TOKEN|HTTP 401'; then
            warn "checksum only, provenance not verified: $(printf '%s' "$err" | head -n 1)"
            return 0
        fi
        die "attestation verification failed for $ASSET: $err"
    fi
    [ "$strict" = "1" ] && die "PLAYBOOK_REQUIRE_ATTESTATION=1 but gh with 'attestation' support is not available"
    warn "checksum only, provenance not verified: install gh to check the build attestation"
}

# Refuses when the tag is not a confirmed release (a PLAYBOOK_REF that names a
# branch or commit, or the no-release-published case in resolve_release):
# those have no release and therefore no binary.
require_release() {
    if [ "$RESOLVED_FROM_REF" -eq 1 ]; then
        if [ -n "$REF" ]; then
            die "PLAYBOOK_REF=$REF pins a branch or commit, not a published release; no release binary exists for it. Unset PLAYBOOK_REF to install the latest release together with its matching binary."
        fi
        die "$RELEASE_REPO has published no release; a verified release binary cannot be fetched."
    fi
}

# Downloads $ASSET and SHA256SUMS of $RESOLVED_TAG into $STAGE, checks the
# asset against its line in SHA256SUMS, then checks its build attestation.
fetch_verified_asset() {
    _fetch "https://github.com/$RELEASE_REPO/releases/download/$RESOLVED_TAG/$ASSET" "$STAGE/$ASSET" \
        || die "could not download $ASSET from the $RESOLVED_TAG release"
    # SHA256SUMS itself is unsigned (TLS and github.com only); provenance comes
    # from the build attestation checked by verify_attestation below.
    _fetch "https://github.com/$RELEASE_REPO/releases/download/$RESOLVED_TAG/SHA256SUMS" "$STAGE/SHA256SUMS" \
        || die "could not download SHA256SUMS from the $RESOLVED_TAG release"

    # SHA256SUMS lists every published asset; shasum/sha256sum -c on the
    # whole file fails, because the others are not present locally. Filter
    # to the one line for our asset first. cd into staging so the filename in
    # that line resolves relative to it.
    (cd "$STAGE" && grep -E "^[0-9a-f]{64}  ${ASSET}$" SHA256SUMS > "$ASSET.sha256") \
        || die "no checksum line for $ASSET in SHA256SUMS; the release may be incomplete or corrupt"
    (cd "$STAGE" && "${CKSUM_CMD[@]}" "$ASSET.sha256") >/dev/null 2>&1 \
        || die "checksum mismatch for $ASSET; the download is corrupt"

    verify_attestation "$STAGE/$ASSET"
}

# Fails unless the binary $1 runs and reports $2 in its --version. The
# checksum only proves the bytes downloaded intact; it cannot prove they run.
# A binary that is intact-but-empty, or built for the wrong platform, needs an
# actual execution to catch.
check_runs() {
    local bin="$1" version="$2" out
    out="$("$bin" --version 2>/dev/null)" \
        || die "the downloaded $(basename "$bin") did not run; the $RESOLVED_TAG release may be broken for this platform"
    case "$out" in
        *"$version"*) ;;
        *) die "version mismatch: the downloaded $(basename "$bin") reports '$out', expected $version" ;;
    esac
}

# Copies the binary $1 to PLAYBOOK_BIN_DIR/$2 through a temp file in the same
# folder, so the name only ever points at a whole binary.
place_binary() {
    local src="$1" name="$2" tmp
    mkdir -p "$PLAYBOOK_BIN_DIR"
    tmp="$(mktemp "$PLAYBOOK_BIN_DIR/.$name.XXXXXX")"
    cp "$src" "$tmp"
    chmod 0755 "$tmp"
    mv -f "$tmp" "$PLAYBOOK_BIN_DIR/$name"
}

# True when version $1 (X.Y.Z) is lower than version $2.
version_lt() {
    local a b i
    IFS=. read -r -a a <<< "$1"
    IFS=. read -r -a b <<< "$2"
    for i in 0 1 2; do
        [ "${a[$i]:-0}" -lt "${b[$i]:-0}" ] && return 0
        [ "${a[$i]:-0}" -gt "${b[$i]:-0}" ] && return 1
    done
    return 1
}

# Fetches and verifies $ASSET (a tarball of $RESOLVED_TAG), unpacks the single
# file $1 from it into $STAGE/out, and checks it is a regular file that runs and
# reports version $2. Nothing is written outside $STAGE.
stage_tool_binary() {
    local name="$1" version="$2"
    fetch_verified_asset

    mkdir -p "$STAGE/out"
    tar -xzf "$STAGE/$ASSET" -C "$STAGE/out" "$name" 2>/dev/null \
        || die "$ASSET holds no $name; the $RESOLVED_TAG release may be corrupt"
    [ -f "$STAGE/out/$name" ] && [ ! -L "$STAGE/out/$name" ] \
        || die "$name in $ASSET is not a regular file"
    chmod 0755 "$STAGE/out/$name"
    check_runs "$STAGE/out/$name" "$version"
}

# Stages the pbdoc helper from pbdoc-<version>-<target>.tar.gz when the release
# lists it in the SHA256SUMS already fetched for playbook, else warns and moves
# on. A listed asset that fails any check is fatal, before anything is placed.
stage_pbdoc() {
    local version="$1"
    ASSET="pbdoc-${version}-${TARGET}.tar.gz"
    if ! grep -q -- "  ${ASSET}\$" "$STAGE/SHA256SUMS"; then
        warn "$RESOLVED_TAG has no $ASSET; skipping the pbdoc document converter. Re-run this installer once a release includes it."
        return 0
    fi
    log "Fetching $ASSET"
    stage_tool_binary pbdoc "$version"
}

# Fetches, verifies, and installs the release binary matching $RESOLVED_TAG.
install_release_binary() {
    require_release

    log "Installing release $RESOLVED_TAG"
    local version
    version="${RESOLVED_TAG#v}"
    release_asset_name "$version"

    STAGE="$(mktemp -d)"

    log "Fetching release binary $ASSET"
    fetch_verified_asset

    chmod 0755 "$STAGE/$ASSET"
    mv "$STAGE/$ASSET" "$STAGE/playbook"
    check_runs "$STAGE/playbook" "$version"
    stage_pbdoc "$version"

    PREVIOUS_VERSION="$(installed_version)"

    # ---- first durable write: everything above here leaves no trace on failure.
    mkdir -p "$PLAYBOOK_BIN_DIR"
    # A safety net, not a gate: a failed backup must never stop the install.
    backup_previous_binary "$STAGE/playbook" \
        || warn "could not back up the previous playbook binary; continuing without a backup"
    place_binary "$STAGE/playbook" playbook
    log "Installed playbook $version to $PLAYBOOK_BIN_DIR/playbook"
    # Only an upgrade shows notes: a reinstall or a downgrade shows nothing.
    if [ -n "$PREVIOUS_VERSION" ] && version_lt "$PREVIOUS_VERSION" "$version"; then
        show_release_notes "$PREVIOUS_VERSION" "$version"
    fi
    if [ -f "$STAGE/out/pbdoc" ]; then
        place_binary "$STAGE/out/pbdoc" pbdoc
        log "Installed pbdoc $version to $PLAYBOOK_BIN_DIR/pbdoc"
    fi
    install_pb_alias

    rm -rf "$STAGE"
    STAGE=""

    ensure_bin_dir_on_path
    warn_if_shadowed
}

# Fetches, verifies and installs pbtop alone from the
# pbtop-<version>-<target>.tar.gz asset of $RESOLVED_TAG, with the same checks
# as the playbook binary. It writes only PLAYBOOK_BIN_DIR/pbtop and the PATH
# block. Releases before 0.35.0 have no such asset.
install_pbtop() {
    require_release
    log "Installing pbtop from release $RESOLVED_TAG"
    local version
    version="${RESOLVED_TAG#v}"
    version="${version%%-*}"
    if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || version_lt "$version" 0.35.0; then
        die "$RESOLVED_TAG has no pbtop asset; pbtop alone ships from playbook 0.35.0 on. Until then, install playbook and run: playbook usage"
    fi
    release_target "$version"
    ASSET="pbtop-${version}-${TARGET}.tar.gz"

    STAGE="$(mktemp -d)"
    log "Fetching $ASSET"
    stage_tool_binary pbtop "$version"

    place_binary "$STAGE/out/pbtop" pbtop
    log "Installed pbtop $version to $PLAYBOOK_BIN_DIR/pbtop"

    rm -rf "$STAGE"
    STAGE=""
    ensure_bin_dir_on_path pbtop
    warn_if_shadowed pbtop
}

# Warns when another binary named $1 (default playbook) earlier on PATH hides
# the one just installed.
warn_if_shadowed() {
    local name="${1:-playbook}" first
    first="$(command -v "$name" 2>/dev/null || true)"
    [ -n "$first" ] || return 0
    [ "$first" -ef "$PLAYBOOK_BIN_DIR/$name" ] && return 0
    if [ "$name" = playbook ]; then
        warn "$first runs before the new $PLAYBOOK_BIN_DIR/playbook on PATH, so hooks keep using the old binary; remove it (Homebrew: brew uninstall playbook) or put $PLAYBOOK_BIN_DIR first on PATH"
    else
        warn "$first runs before the new $PLAYBOOK_BIN_DIR/$name on PATH; remove it (Homebrew: brew uninstall pbtop) or put $PLAYBOOK_BIN_DIR first on PATH"
    fi
}

# Links `pb` to `playbook` as a short alias. Never replaces a different `pb`:
# a file we did not make, or any `pb` already on PATH, is left alone and one
# line says so. An existing `pb` link to `playbook` is kept as it is.
install_pb_alias() {
    local link="$PLAYBOOK_BIN_DIR/pb" other
    if [ -L "$link" ] && [ "$(readlink "$link")" = "playbook" ]; then
        return 0
    fi
    if [ -e "$link" ] || [ -L "$link" ]; then
        log "Left $link alone: it is not the playbook alias. Use playbook instead of pb."
        return 0
    fi
    other="$(command -v pb 2>/dev/null || true)"
    if [ -n "$other" ]; then
        log "Left pb alone: $other is already a command. Use playbook instead of pb."
        return 0
    fi
    if ln -s playbook "$link" 2>/dev/null; then
        log "Linked $link to playbook (short alias: pb cc, pb ccd)"
    else
        log "Could not create $link; use playbook instead of pb."
    fi
}

# Puts $PLAYBOOK_BIN_DIR on PATH for future shells, mirroring the `path` step of
# `playbook setup` (src/init/path.rs) so the two never double up: same files,
# same `# playbook binary` marker, same "directory already named" guard. zsh
# gets ~/.zshenv (hooks run in non-interactive shells, which skip ~/.zshrc),
# bash gets ~/.bashrc plus its login file, fish gets a conf.d file, any other
# shell gets ~/.profile. This runs here as well as in `init` because the
# binary just installed may predate the init step.
_add_path_block() {
    local file="$1" fish="${2:-0}"
    if grep -v '^[[:space:]]*#' "$file" 2>/dev/null | grep -qF "$PLAYBOOK_BIN_DIR"; then
        log "$file already has $PLAYBOOK_BIN_DIR on PATH"
        return 0
    fi
    mkdir -p "$(dirname "$file")"
    if [ "$fish" = "1" ]; then
        printf '\n# playbook binary\nfish_add_path -g "%s"\n' "$PLAYBOOK_BIN_DIR" >> "$file"
    else
        printf '\n# playbook binary\nexport PATH="%s:$PATH"\n' "$PLAYBOOK_BIN_DIR" >> "$file"
    fi
    log "Added $PLAYBOOK_BIN_DIR to PATH in $file"
}

ensure_bin_dir_on_path() {
    local name="${1:-playbook}"
    case "$PLAYBOOK_BIN_DIR" in
        /usr/bin|/bin|/usr/sbin|/sbin) return 0 ;;
        *\"*|*\$*|*\`*|*\\*)
            warn "$PLAYBOOK_BIN_DIR has characters a shell line cannot quote; add it to PATH by hand."
            return 0 ;;
    esac
    local login f
    case "$(basename "${SHELL:-}")" in
        zsh) _add_path_block "$HOME/.zshenv" ;;
        bash)
            login="$HOME/.bash_profile"
            for f in .bash_profile .bash_login .profile; do
                if [ -e "$HOME/$f" ]; then login="$HOME/$f"; break; fi
            done
            _add_path_block "$HOME/.bashrc"
            _add_path_block "$login"
            ;;
        fish) _add_path_block "$HOME/.config/fish/conf.d/playbook.fish" 1 ;;
        *) _add_path_block "$HOME/.profile" ;;
    esac

    warn "$name is installed to $PLAYBOOK_BIN_DIR. Open a new terminal (or source your rc file) so it resolves on PATH."
}

# STAGE (the binary staging dir) is cleaned up on any exit, including an
# interrupted `curl | bash`: a trap on EXIT alone would leak the directory on
# ctrl-c during the network path.
STAGE=""
PREVIOUS_VERSION=""
_cleanup_staging() {
    [ -n "$STAGE" ] && rm -rf "$STAGE"
    return 0
}
trap _cleanup_staging EXIT INT TERM

# The pbtop mode installs pbtop and stops: nothing below runs.
if [ "$ONLY_PBTOP" -eq 1 ]; then
    resolve_release
    install_pbtop
    log "Done. Run pbtop for the Agent Budget Dashboard."
    exit 0
fi

# PLAYBOOK_SKIP_DOWNLOAD=1 is a test seam: use the binary already in
# PLAYBOOK_BIN_DIR and skip the network entirely.
if [ "${PLAYBOOK_SKIP_DOWNLOAD:-}" = "1" ]; then
    [ -x "$PLAYBOOK_BIN_DIR/playbook" ] || die "PLAYBOOK_SKIP_DOWNLOAD=1 but there is no playbook in $PLAYBOOK_BIN_DIR"
    log "Using the playbook already in $PLAYBOOK_BIN_DIR (download skipped)"
else
    resolve_release
    install_release_binary
fi

# --- playbook migrate --------------------------------------------------------

# Brings playbook's own files up to the installed version (ADR 0034). It never
# writes Claude Code's settings; items that need `playbook setup` are only
# printed. It runs with a terminal or --yes, never with --no-init (binary
# only), and is skipped on a binary too old to have the command. A failure
# here does not stop the install: the binary retries on its next run.
if [ "$NO_INIT" -eq 0 ] && { [ "$ASSUME_YES" -eq 1 ] || have_tty; } \
    && "$PLAYBOOK_BIN_DIR/playbook" migrate --help >/dev/null 2>&1; then
    log "Running playbook migrate"
    "$PLAYBOOK_BIN_DIR/playbook" migrate || \
        warn "playbook migrate did not finish; run \`playbook migrate\` to retry"
fi

# --- playbook setup ----------------------------------------------------------

# Releases before the rename only know `playbook init`, the old name of
# `playbook setup` (ADR-0040). Ask the installed binary which one it has:
# its help names the command, and an older binary prints an error.
case "$("$PLAYBOOK_BIN_DIR/playbook" setup --help 2>/dev/null || true)" in
    *"playbook setup"*) SETUP_CMD=setup ;;
    *) SETUP_CMD=init ;;
esac

# Everything above changed nothing outside the binary folder, the shell
# start-up files that put it on PATH, and playbook's own files under
# ~/.config/playbook. `playbook setup` changes Claude Code's own
# settings, so it runs only after a yes: --yes, or an answer typed at the
# prompt. With no terminal and no --yes it is skipped and the command is
# printed.
INIT_RAN=0
if [ "$NO_INIT" -eq 0 ] && consent "Run playbook $SETUP_CMD now? It adds playbook's hook entries to ~/.claude/settings.json (your own entries stay), the status line, Claude Code telemetry to a collector on this machine, the system prompt that pb cc loads and the pb link (change it later with playbook $SETUP_CMD --only telemetry or undo it with playbook uninstall)."; then
    # --binary-only keeps the prompt off. Under --yes with no prompt flag,
    # setup decides, so it can keep an earlier no.
    if [ "$BINARY_ONLY" -eq 1 ] && [ "$OPT_SYSTEM_PROMPT" -eq 0 ]; then
        OPT_NO_SYSTEM_PROMPT=1
    fi
    if [ "$ASSUME_YES" -eq 0 ] && [ "$OPT_SYSTEM_PROMPT" -eq 0 ] && [ "$OPT_NO_SYSTEM_PROMPT" -eq 0 ]; then
        if ask "Install the custom system prompt, which pb cc loads? (recommended)" Y; then
            OPT_SYSTEM_PROMPT=1
        else
            OPT_NO_SYSTEM_PROMPT=1
        fi
    fi

    # Probe for each optional flag rather than assuming the binary supports it.
    #
    # This script is fetched from the main branch by the documented curl
    # one-liner, while the binary comes from the latest RELEASE. Those two are
    # permanently allowed to drift, so install.sh must never assume a binary
    # feature newer than the last published release. Passing --system-prompt
    # to a v0.10.0 binary, which predates that flag, made `init` exit non-zero
    # with "unexpected argument", so the install finished having written no
    # settings.json at all. Probing costs one process and removes the whole
    # class of failure.
    #
    # The help text is captured first and matched with `case`, not piped into
    # `grep -q`. grep -q exits on its first match, which closes the pipe while
    # the binary is still writing; under `set -o pipefail` that SIGPIPE made
    # the probe report "unsupported" at random, so a flag was sometimes
    # dropped.
    _init_help="$("$PLAYBOOK_BIN_DIR/playbook" "$SETUP_CMD" --help 2>/dev/null || true)"
    _init_supports() {
        case "$_init_help" in
            *"$1"*) return 0 ;;
            *) return 1 ;;
        esac
    }

    _INIT_ARGS=""
    if [ "$OPT_SYSTEM_PROMPT" -eq 1 ]; then
        if _init_supports --system-prompt; then
            _INIT_ARGS="$_INIT_ARGS --system-prompt"
        else
            warn "the installed playbook $( "$PLAYBOOK_BIN_DIR/playbook" --version 2>/dev/null | awk '{print $NF}' ) does not support --system-prompt; skipping it. Re-run this installer once a release includes it."
        fi
    fi
    if [ "$OPT_NO_SYSTEM_PROMPT" -eq 1 ] && _init_supports --no-system-prompt; then
        _INIT_ARGS="$_INIT_ARGS --no-system-prompt"
    fi
    # The installer has already asked its own questions, so setup must not ask again.
    if _init_supports --yes; then
        _INIT_ARGS="$_INIT_ARGS --yes"
    fi
    log "Running playbook $SETUP_CMD"
    # Invoked by absolute path: a fresh shell's PATH may not yet include
    # PLAYBOOK_BIN_DIR, and this runs before the rc-file PATH line takes
    # effect. Setup prints one line per step, so its output is not redirected.
    # shellcheck disable=SC2086
    "$PLAYBOOK_BIN_DIR/playbook" "$SETUP_CMD" $_INIT_ARGS || \
        die "playbook $SETUP_CMD failed; see the step report above"
    INIT_RAN=1

    # Older releases have no trust step in setup, so this still runs here.
    trust_playbook_config_dir
fi

# --- summary -----------------------------------------------------------------

log "Done."
printf '\n'
printf 'Installed playbook to: %s\n' "$PLAYBOOK_BIN_DIR/playbook"
printf '\nNext steps:\n'
if [ "$INIT_RAN" -eq 0 ]; then
    INIT_CMD="playbook $SETUP_CMD"
    [ "$OPT_SYSTEM_PROMPT" -eq 1 ] && INIT_CMD="$INIT_CMD --system-prompt"
    [ "$OPT_NO_SYSTEM_PROMPT" -eq 1 ] && INIT_CMD="$INIT_CMD --no-system-prompt"
    printf '  - Run: %s   (adds the hook entries to ~/.claude/settings.json, asks first)\n' "$INIT_CMD"
fi
printf '  - Skills (optional, they work without the binary): %s\n' "$SKILLS_INSTALL_CMD"
