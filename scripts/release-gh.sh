#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Igor Santos
# SPDX-License-Identifier: Apache-2.0
#
# release-gh.sh: the GitHub steps of a release (ADR 0039), each safe to run
# again. GH_TOKEN must hold a token for <repo>; the release workflow passes
# the igorjs app token. Every step prints key=value lines on stdout.
#
#   release-gh.sh pr <repo> <branch> <title> <body-file> <dir> <file>...
#     Opens a pull request to the default branch of <repo> with one commit
#     that sets each <file> to its content under <dir>, and turns on
#     auto-merge. <dir> is a checkout of <repo>, and the commit starts from
#     its HEAD, which must still be the head of the default branch, so the
#     commit cannot undo a change that landed after the checkout. The commit
#     is made with createCommitOnBranch, so GitHub signs it. <branch> must be
#     release/vX.Y.Z (the flake: release/nix-vX.Y.Z), and each <file> a regular file inside <dir>. An open
#     pull request from <branch> in <repo> by the app is reused when its
#     files match <dir> (auto-merge is turned on again), and a merged one is
#     reported as merged. Any other open pull request on <branch> stops the
#     run. Prints pr=<number> and state=open|merged.
#
#   release-gh.sh wait <repo> <pr> <seconds>
#     Waits until the pull request merges, then prints sha=<merge commit>.
#     Fails when it closes without a merge or the time runs out. A failed
#     poll is retried. With RELEASE_GH_UPDATE_BRANCH=1, a pull request whose
#     mergeStateStatus is BEHIND gets `gh pr update-branch`, at most once
#     every RELEASE_GH_UPDATE_EVERY seconds (default 240), because strict
#     checks keep a pull request that fell behind from merging. Leave it
#     unset for a repository with a merge queue.
#
#   release-gh.sh tag <repo> <tag> <sha>
#     Creates the lightweight tag <tag>, a vX.Y.Z, on <sha>. A tag that
#     already points at <sha>, an annotated one included, is fine, and one
#     that points elsewhere fails. When the create fails, the tag is read again, since
#     another run may have made it. Exits 3 when it is still missing, so the
#     caller can print the manual step.
#
#   release-gh.sh release <repo> <tag> <title> <notes-file>
#     Creates the published GitHub release for <tag> as the latest release,
#     unless a release for <tag> exists. For the downstream repositories,
#     whose release has no assets. Prints url=<release page>.
#
#   release-gh.sh find <repo> <branch>
#     Prints pr=<number> and state=open|merged for the pull request from
#     <branch> by the app into the default branch of <repo>, or nothing when
#     there is none.
#
#   release-gh.sh state <repo> <tag>
#     Prints release=none, draft or published for the release of <tag>.
#     Only "release not found" means none. Any other error fails, so a
#     failed read is never taken for a missing release.
set -euo pipefail

fail() { echo "FAIL $*" >&2; exit 1; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

usage() {
  echo "usage: release-gh.sh pr <repo> <branch> <title> <body-file> <dir> <file>... | wait <repo> <pr> <seconds> | tag <repo> <tag> <sha> | release <repo> <tag> <title> <notes-file> | find <repo> <branch> | state <repo> <tag>" >&2
  exit 2
}

SHA='^[0-9a-f]{40}$'
TAG='^v[0-9]+\.[0-9]+\.[0-9]+$'
# The GitHub App that opens release pull requests.
APP="${RELEASE_GH_APP:-igorjs}"

# The number of the pull request from <branch> into <base> in <repo> itself,
# opened by the release app, in <state>. A pull request from a fork, from
# another author or into another branch does not count.
find_pr() {
  gh pr list --repo "$1" --head "$2" --base "$3" --state "$4" --app "$APP" \
    --json number,isCrossRepository \
    --jq '[.[] | select(.isCrossRepository | not)][0].number // empty'
}

# Turns on auto-merge. A repository with a merge queue takes no merge
# method, and one without takes squash, its only method.
auto_merge() {
  local repo="$1" number="$2" err
  if err="$(gh pr merge "$number" --repo "$repo" --auto --squash 2>&1 >/dev/null)"; then return 0; fi
  gh pr merge "$number" --repo "$repo" --auto >/dev/null \
    || fail "could not turn on auto-merge for $repo#$number: $err"
}

pr() {
  [ "$#" -ge 6 ] || usage
  local repo="$1" branch="$2" title="$3" body="$4" dir="$5"
  shift 5
  [[ "$branch" =~ ^release/(nix-)?v[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "branch '$branch' is not release/vX.Y.Z or release/nix-vX.Y.Z"
  local number base head want f root parent open
  # Each file is a regular file inside <dir>, checked before anything is
  # written. A symlink, the file or a folder on its path, could make jq read
  # a file outside the checkout.
  root="$(cd "$dir" && pwd -P)" || fail "$dir is not a folder"
  for f in "$@"; do
    case "/$f/" in
      //* | */../* | */./*) fail "'$f' is not a plain path inside the checkout" ;;
    esac
    [ ! -L "$dir/$f" ] || fail "$dir/$f is a symlink"
    [ -f "$dir/$f" ] || fail "$dir/$f is missing"
    parent="$(cd "$(dirname "$dir/$f")" && pwd -P)"
    case "$parent/" in
      "$root"/*) ;;
      *) fail "$dir/$f is outside the checkout" ;;
    esac
  done
  base="$(gh api "repos/$repo" --jq .default_branch)"

  number="$(find_pr "$repo" "$branch" "$base" open)"
  if [ -n "$number" ]; then
    # A pull request left by an earlier run must carry the same changes, or
    # it would merge content the checkout no longer has.
    for f in "$@"; do
      gh api -H "Accept: application/vnd.github.raw" "repos/$repo/contents/$f?ref=$branch" > "$TMP/remote" \
        || fail "could not read $f on $branch in $repo"
      cmp -s "$TMP/remote" "$dir/$f" \
        || fail "$f on $repo#$number differs from the checkout. Close the pull request, then run again."
    done
    auto_merge "$repo" "$number"
    printf 'pr=%s\nstate=open\n' "$number"
    return 0
  fi
  number="$(find_pr "$repo" "$branch" "$base" merged)"
  if [ -n "$number" ]; then
    printf 'pr=%s\nstate=merged\n' "$number"
    return 0
  fi

  want="$(git -C "$dir" rev-parse HEAD)"
  head="$(gh api "repos/$repo/git/ref/heads/$base" --jq .object.sha)"
  [ "$head" = "$want" ] || fail "$repo $base moved from $want to $head since the checkout. Run again."
  # A branch left by a failed run is dropped, so the commit starts from the
  # checkout. The list above comes from search, which can lag and only
  # counts the app, so an open pull request of any author on the branch,
  # read directly, stops the run before its branch is deleted.
  if gh api "repos/$repo/git/ref/heads/$branch" >/dev/null 2>&1; then
    open="$(gh api "repos/$repo/pulls?head=${repo%%/*}:$branch&state=open" --jq length)"
    [ "$open" = 0 ] || fail "an open pull request in $repo uses $branch. Close it or let it merge, then run again."
    gh api -X DELETE "repos/$repo/git/refs/heads/$branch" >/dev/null
  fi
  gh api -X POST "repos/$repo/git/refs" -f "ref=refs/heads/$branch" -f "sha=$head" >/dev/null

  # jq reads each file itself, so a large file never goes through argv.
  : > "$TMP/files.jsonl"
  for f in "$@"; do
    jq -n -c --arg path "$f" --rawfile c "$dir/$f" '{path: $path, contents: ($c | @base64)}' >> "$TMP/files.jsonl"
  done
  jq -s --arg repo "$repo" --arg branch "$branch" --arg head "$head" --arg msg "$title" \
    '{query: "mutation($input: CreateCommitOnBranchInput!) { createCommitOnBranch(input: $input) { commit { oid } } }",
      variables: {input: {
        branch: {repositoryNameWithOwner: $repo, branchName: $branch},
        message: {headline: $msg},
        expectedHeadOid: $head,
        fileChanges: {additions: .}
      }}}' "$TMP/files.jsonl" > "$TMP/commit.json"
  local oid
  oid="$(gh api graphql --input "$TMP/commit.json" --jq '.data.createCommitOnBranch.commit.oid // empty')"
  [ -n "$oid" ] || fail "GitHub did not create the commit on $repo $branch"

  number="$(gh api -X POST "repos/$repo/pulls" -f "title=$title" -f "head=$branch" -f "base=$base" \
    -F "body=@$body" --jq .number)"
  auto_merge "$repo" "$number"
  printf 'pr=%s\nstate=open\n' "$number"
}

wait_merged() {
  [ "$#" -eq 3 ] || usage
  local repo="$1" number="$2" seconds="$3" end state sha behind next=0 now
  [[ "$number" =~ ^[0-9]+$ ]] || usage
  [[ "$seconds" =~ ^[0-9]+$ ]] || usage
  end=$(( $(date +%s) + seconds ))
  while :; do
    state="" behind="" sha=""
    read -r state behind sha < <(gh pr view "$number" --repo "$repo" --json state,mergeStateStatus,mergeCommit \
      --jq '"\(.state) \(.mergeStateStatus) \(.mergeCommit.oid // "")"' 2>/dev/null) || true
    case "$state" in
      MERGED)
        if [[ "$sha" =~ $SHA ]]; then printf 'sha=%s\n' "$sha"; return 0; fi ;;
      CLOSED) fail "pull request $repo#$number was closed without a merge" ;;
      OPEN)
        now="$(date +%s)"
        if [ "${RELEASE_GH_UPDATE_BRANCH:-}" = 1 ] && [ "$behind" = BEHIND ] && [ "$now" -ge "$next" ]; then
          # A failed update is not fatal: the next round looks again.
          gh pr update-branch "$number" --repo "$repo" >/dev/null 2>&1 \
            || echo "could not update the branch of $repo#$number" >&2
          next=$(( now + ${RELEASE_GH_UPDATE_EVERY:-240} ))
        fi ;;
    esac
    [ "$(date +%s)" -lt "$end" ] || fail "pull request $repo#$number did not merge within $seconds seconds"
    sleep "${RELEASE_GH_POLL:-20}"
  done
}

# Prints the commit <tag> in <repo> points at, an annotated tag followed, or
# nothing when the tag is missing. gh prints the error body on stdout, so the
# exit status decides, and any error but a 404 fails.
tag_commit() {
  local repo="$1" tag="$2" ref type have
  if ref="$(gh api "repos/$repo/git/ref/tags/$tag" 2>/dev/null)"; then
    type="$(jq -r '.object.type' <<<"$ref")"
    have="$(jq -r '.object.sha' <<<"$ref")"
    if [ "$type" = tag ]; then
      have="$(gh api "repos/$repo/git/tags/$have" --jq .object.sha)"
    fi
    printf '%s\n' "$have"
    return 0
  fi
  [ "$(jq -r '.status // empty' <<<"$ref" 2>/dev/null)" = 404 ] \
    || fail "could not read tag $tag in $repo: $ref"
}

tag() {
  [ "$#" -eq 3 ] || usage
  local repo="$1" tag="$2" sha="$3" have err
  [[ "$tag" =~ $TAG ]] || fail "'$tag' is not a release tag vX.Y.Z"
  [[ "$sha" =~ $SHA ]] || fail "'$sha' is not a commit sha"
  have="$(tag_commit "$repo" "$tag")"
  if [ -z "$have" ]; then
    if err="$(gh api -X POST "repos/$repo/git/refs" -f "ref=refs/tags/$tag" -f "sha=$sha" 2>&1 >/dev/null)"; then
      printf 'tag=%s\nstate=created\n' "$tag"
      return 0
    fi
    # Another run may have made the tag in the meantime.
    have="$(tag_commit "$repo" "$tag")"
    if [ -z "$have" ]; then
      echo "GitHub refused to create $tag in $repo: $err" >&2
      exit 3
    fi
  fi
  [ "$have" = "$sha" ] || fail "tag $tag in $repo points at $have, not $sha. Not moving it."
  printf 'tag=%s\nstate=exists\n' "$tag"
}

# Prints the page of the release for <tag> in <repo>.
release_url() {
  gh release view "$2" --repo "$1" --json url --jq .url
}

release() {
  [ "$#" -eq 4 ] || usage
  local repo="$1" tag="$2" title="$3" notes="$4" url
  [[ "$tag" =~ $TAG ]] || fail "'$tag' is not a release tag vX.Y.Z"
  url="$(release_url "$repo" "$tag" 2>/dev/null || true)"
  if [ -z "$url" ]; then
    # gh prints the page of the new release. Another run may have made it
    # in the meantime, so a failed create looks again.
    url="$(gh release create "$tag" --repo "$repo" --verify-tag --latest --title "$title" --notes-file "$notes")" \
      || url="$(release_url "$repo" "$tag")"
  fi
  printf 'url=%s\n' "$url"
}

find_cmd() {
  [ "$#" -eq 2 ] || usage
  local repo="$1" branch="$2" base number state
  [[ "$branch" =~ ^release/(nix-)?v[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "branch '$branch' is not release/vX.Y.Z or release/nix-vX.Y.Z"
  base="$(gh api "repos/$repo" --jq .default_branch)"
  for state in open merged; do
    number="$(find_pr "$repo" "$branch" "$base" "$state")"
    if [ -n "$number" ]; then
      printf 'pr=%s\nstate=%s\n' "$number" "$state"
      return 0
    fi
  done
}

state() {
  [ "$#" -eq 2 ] || usage
  local repo="$1" tag="$2" out
  [[ "$tag" =~ $TAG ]] || fail "'$tag' is not a release tag vX.Y.Z"
  if out="$(gh release view "$tag" --repo "$repo" --json isDraft --jq .isDraft 2>"$TMP/err")"; then
    case "$out" in
      true) echo "release=draft" ;;
      false) echo "release=published" ;;
      *) fail "could not read the release $tag in $repo: $out" ;;
    esac
  elif grep -qx 'release not found' "$TMP/err"; then
    echo "release=none"
  else
    fail "could not read the release $tag in $repo: $(tr '\n' ' ' < "$TMP/err")"
  fi
}

cmd="${1:-}"
[ "$#" -gt 0 ] && shift
case "$cmd" in
  pr) pr "$@" ;;
  wait) wait_merged "$@" ;;
  tag) tag "$@" ;;
  release) release "$@" ;;
  state) state "$@" ;;
  find) find_cmd "$@" ;;
  *) usage ;;
esac
