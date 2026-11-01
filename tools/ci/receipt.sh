#!/usr/bin/env bash
set -euo pipefail

notes_ref=refs/notes/tilo-check-ci

usage() {
  echo "usage: $0 record <sha> <run-id> <select-result> <security-result> <affected-result> <remote>" >&2
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

# 2 from ls-remote means the remote has no such ref yet, which is the normal
# state before the first receipt is recorded.
remote_has_ref() {
  git ls-remote --exit-code --refs "$1" "$2" >/dev/null 2>&1
}

case "${1:-}" in
  record)
    [[ $# == 7 ]] || usage
    sha=$2
    run_id=$3
    select_result=$4
    security_result=$5
    affected_result=$6
    remote=$7
    valid_sha "$sha" || { echo "invalid receipt commit: $sha" >&2; exit 2; }
    [[ $run_id =~ ^[0-9]+$ ]] || { echo "invalid workflow run id: $run_id" >&2; exit 2; }
    valid_result "$select_result" || { echo "invalid select result: $select_result" >&2; exit 2; }
    valid_result "$security_result" || { echo "invalid security result: $security_result" >&2; exit 2; }
    valid_result "$affected_result" || { echo "invalid affected result: $affected_result" >&2; exit 2; }
    git rev-parse --verify "$sha^{commit}" >/dev/null
    git config user.name 'Tilo Forgejo CI'
    git config user.email 'tilo@noreply.code.tilo.so'
    # Runs recording the receipts of different commits publish the same notes
    # ref at the same time, so the ref a push replaces can move under this run.
    # Every attempt starts from the notes ref as the forge has it and puts this
    # run's note on top, so the receipt that won the race is kept and this one
    # is published beside it. Only a push the ref moved under is another run's
    # receipt; any other refusal, and a fetch of a notes ref that exists, end
    # here with git's own message.
    note="select=$select_result security=$security_result affected=$affected_result run=$run_id sha=$sha"
    published=false
    for attempt in {1..20}; do
      if fetch_error=$(git fetch "$remote" "+$notes_ref:$notes_ref" 2>&1); then
        base=$(git rev-parse "$notes_ref")
      elif remote_has_ref "$remote" "$notes_ref"; then
        printf '%s\n' "$fetch_error" >&2
        echo "could not fetch $notes_ref from $remote" >&2
        exit 1
      else
        base=
      fi
      git notes --ref=tilo-check-ci add -f -m "$note" "$sha"
      if push_error=$(git push "$remote" "$notes_ref" 2>&1); then
        published=true
        break
      fi
      printf '%s\n' "$push_error" >&2
      if ! remote_tip=$(git ls-remote "$remote" "$notes_ref" | cut -f1); then
        echo "could not read $notes_ref from $remote" >&2
        exit 1
      fi
      if [ -z "$remote_tip" ] || [ "$remote_tip" = "$base" ]; then
        echo "could not publish the receipt of $sha" >&2
        exit 1
      fi
      # Wait longer with every lost round, spread by a little jitter, so runs
      # that collided do not all fetch and push again at the same moment.
      delay_ms=$(( (RANDOM % 200) + (attempt - 1) * 100 ))
      if (( delay_ms > 2000 )); then
        delay_ms=2000
      fi
      sleep "$(( delay_ms / 1000 )).$(printf '%03d' $(( delay_ms % 1000 )))"
    done
    if [ "$published" != true ]; then
      echo "other runs kept publishing $notes_ref; no receipt for $sha" >&2
      exit 1
    fi
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
    grep -Eq '^select=success security=success affected=success run=[0-9]+ sha='"$sha"'$' \
      <<<"$receipt"
    ;;
  *) usage ;;
esac
