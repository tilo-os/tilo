#!/usr/bin/env bash
# Resolve this checkout's frozen RPM snapshot, waiting for a build-rpms.yml
# run on main that is building the same package-set fingerprint.
#
# A commit that changes the snapshot's inputs starts build-rpms.yml and the
# image builds together, and later commits with the same fingerprint do not
# start it again, so the snapshot is often still being built when a build
# asks for it. This waits while a queued or running build-rpms.yml main run,
# at any commit, has this checkout's fingerprint, and fails when none does or
# the one it waited for ended without publishing.
#
#   await-rpm-snapshot.sh           wait, then print the verified RPMS_REF
#                                   (resolve-rpm-snapshot.sh)
#   await-rpm-snapshot.sh --exists  wait only; exit 0 once the tag resolves
#
# Exit 1: no snapshot and nothing building it. Exit 3: the registry or the
# forge could not be asked. Reads the public repository's runs anonymously.
# AWAIT_RPM_SNAPSHOT_SECONDS bounds the wait (default 170 minutes, inside the
# 3-hour job limit).
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo"
resolve="$repo/os/dev/resolve-rpm-snapshot.sh"

mode=resolve
case "${1:-}" in
  "") ;;
  --exists) mode=exists ;;
  -h|--help)
    sed -n '2,19p' "$0"
    exit 0
    ;;
  *)
    printf 'await-rpm-snapshot: unknown option %s\n' "$1" >&2
    exit 2
    ;;
esac

forge="${GITHUB_SERVER_URL:-https://code.tilo.so}/${GITHUB_REPOSITORY:-tilo/tilo}"
api="${GITHUB_SERVER_URL:-https://code.tilo.so}/api/v1/repos/${GITHUB_REPOSITORY:-tilo/tilo}"
deadline=$((SECONDS + ${AWAIT_RPM_SNAPSHOT_SECONDS:-10200}))
fingerprint="$(python3 "$repo/os/dev/rpm-snapshot-fingerprint.py")"
tag="set-$fingerprint"
head="$(git -C "$repo" rev-parse HEAD)"
declare -A fingerprints=(["$head"]="$fingerprint")
work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT

done_waiting() {
  [ "$mode" = exists ] && exit 0
  exec "$resolve"
}

exists() {
  local status=0
  "$resolve" --exists 2>"$work/exists.err" || status=$?
  case $status in
    0) return 0 ;;
    1) return 1 ;;
    *) cat "$work/exists.err" >&2; exit 3 ;;
  esac
}

# Sets fp to the package-set fingerprint of a main commit, from its sources.
# Other commits go into a scratch object store, never the caller's checkout.
fingerprint_at() {
  local sha="$1" tree
  if [[ -z ${fingerprints[$sha]:-} ]]; then
    [[ -d $work/git ]] || git init -q --bare "$work/git"
    git -C "$work/git" fetch -q --depth=1 --filter=blob:none "$forge.git" "$sha" \
      || { printf 'await-rpm-snapshot: cannot fetch %s\n' "$sha" >&2; exit 3; }
    tree="$work/$sha"
    mkdir -p "$tree"
    git -C "$work/git" archive "$sha" os | tar -x -C "$tree"
    fingerprints[$sha]="$(python3 "$tree/os/dev/rpm-snapshot-fingerprint.py")" \
      || { printf 'await-rpm-snapshot: cannot fingerprint %s\n' "$sha" >&2; exit 3; }
    rm -rf -- "$tree"
  fi
  fp="${fingerprints[$sha]}"
}

# Sets runs to the queued and running build-rpms.yml main runs ("id #number")
# building this fingerprint.
building() {
  local status id number sha
  runs=()
  for status in waiting blocked running; do
    curl -fsS -o "$work/runs.json" \
      "$api/actions/runs?workflow_id=build-rpms.yml&status=$status&limit=50" \
      || { printf 'await-rpm-snapshot: cannot list build-rpms.yml runs\n' >&2; exit 3; }
    jq -r '.workflow_runs[] | select(.prettyref == "main")
      | "\(.id) \(.index_in_repo) \(.commit_sha)"' "$work/runs.json" > "$work/runs.txt"
    while read -r id number sha; do
      fingerprint_at "$sha"
      if [[ $fp == "$fingerprint" ]]; then
        runs+=("$id #$number")
      fi
    done < "$work/runs.txt"
  done
}

waited=()
while :; do
  exists && done_waiting
  building
  if ((${#runs[@]} == 0)); then
    # It may have published between the lookup and the listing.
    exists && done_waiting
    if ((${#waited[@]})); then
      for run in "${waited[@]}"; do
        conclusion="$(curl -fsS "$api/actions/runs/${run%% *}" | jq -r .status)" \
          || conclusion=unknown
        printf 'await-rpm-snapshot: build-rpms.yml run %s ended %s without publishing %s\n' \
          "${run#* }" "$conclusion" "$tag" >&2
      done
    else
      printf 'await-rpm-snapshot: no published snapshot for %s and no build-rpms.yml run on main is building it\n' \
        "$tag" >&2
    fi
    exit 1
  fi
  if ((SECONDS >= deadline)); then
    printf 'await-rpm-snapshot: gave up waiting for %s\n' "$tag" >&2
    exit 1
  fi
  if ((${#waited[@]} == 0)); then
    printf 'await-rpm-snapshot: waiting for build-rpms.yml run %s to publish %s\n' \
      "${runs[*]#* }" "$tag" >&2
  fi
  waited=("${runs[@]}")
  sleep 30
done
