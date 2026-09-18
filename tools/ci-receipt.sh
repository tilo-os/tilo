#!/usr/bin/env bash
set -euo pipefail

notes_ref=refs/notes/tilo-check-ci

usage() {
  echo "usage: $0 record <sha> <run-id> <select-result> <affected-result>" >&2
  echo "       $0 verify <sha>" >&2
  exit 2
}

valid_sha() {
  [[ $1 =~ ^[0-9a-f]{40,64}$ ]]
}

valid_result() {
  case "$1" in
    success | failure | cancelled | skipped) return 0 ;;
    *) return 1 ;;
  esac
}

case "${1:-}" in
  record)
    [[ $# == 6 ]] || usage
    sha=$2
    run_id=$3
    select_result=$4
    affected_result=$5
    remote=$6
    valid_sha "$sha" || { echo "invalid receipt commit: $sha" >&2; exit 2; }
    [[ $run_id =~ ^[0-9]+$ ]] || { echo "invalid workflow run id: $run_id" >&2; exit 2; }
    valid_result "$select_result" || { echo "invalid select result: $select_result" >&2; exit 2; }
    valid_result "$affected_result" || { echo "invalid affected result: $affected_result" >&2; exit 2; }
    git rev-parse --verify "$sha^{commit}" >/dev/null
    git config user.name 'Tilo Forgejo CI'
    git config user.email 'tilo@noreply.code.podesta.ai'
    git fetch "$remote" "+$notes_ref:$notes_ref" 2>/dev/null || true
    git notes --ref=tilo-check-ci add -f \
      -m "select=$select_result affected=$affected_result run=$run_id sha=$sha" \
      "$sha"
    git push "$remote" "$notes_ref"
    ;;
  verify)
    [[ $# == 3 ]] || usage
    sha=$2
    remote=$3
    valid_sha "$sha" || { echo "invalid receipt commit: $sha" >&2; exit 2; }
    git fetch --no-tags "$remote" \
      "+refs/heads/main:refs/remotes/$remote/main" \
      "+$notes_ref:$notes_ref"
    git merge-base --is-ancestor "$sha" "refs/remotes/$remote/main"
    receipt=$(git notes --ref=tilo-check-ci show "$sha")
    grep -Eq '^select=success affected=success run=[0-9]+ sha='"$sha"'$' \
      <<<"$receipt"
    ;;
  *) usage ;;
esac
