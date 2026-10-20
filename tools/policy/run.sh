#!/usr/bin/env bash
# Fast, deterministic repository-policy suite shared by local development and CI.
#
# Its steps are independent, and most wait on files and on the processes they start far more
# than they compute: one after another they took many minutes. They run side by side instead,
# POLICY_JOBS at a time (8 by default), first the checks of the tree, then the tests, whose
# fixtures write beside the sources; each step's output is printed whole, in the order below,
# and every failing step is named.
set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$repo"

jobs_max=${POLICY_JOBS:-8}
work=$(mktemp -d "${TMPDIR:-/tmp}/policy.XXXXXX")
trap 'rm -rf "$work"' EXIT
labels=()
shown=0
failed=()

# Start step $1, the command after it, once fewer than jobs_max steps run.
run() {
  local label=$1 index=${#labels[@]}
  shift
  labels+=("$label")
  while (( $(jobs -rp | wc -l) >= jobs_max )); do
    wait -n || true
  done
  (
    started=$SECONDS status=0
    "$@" >"$work/$index.log" 2>&1 </dev/null || status=$?
    printf '%s %s\n' "$status" "$((SECONDS - started))" >"$work/$index.status"
  ) &
}

# Tests build Git repositories of their own: a GIT_DIR from the environment (a caller's
# tools/ci/t-git-index.sh) would take over their Git commands.
run_tests() {
  local label=$1
  shift
  run "$label" env -u GIT_DIR -u GIT_WORK_TREE "$@"
}

# Wait for the steps started so far and print what each said.
settle() {
  local index status took
  wait
  for (( index = shown; index < ${#labels[@]}; index++ )); do
    read -r status took <"$work/$index.status"
    printf '\n==> %s (%ss)\n' "${labels[$index]}" "$took"
    cat "$work/$index.log"
    if (( status != 0 )); then
      printf '::error::repository policy check failed: %s (exit %s)\n' "${labels[$index]}" "$status"
      failed+=("${labels[$index]}")
    fi
  done
  shown=${#labels[@]}
}

# The steps (tools/policy/steps.toml), which t runs one check each: first the checks of the
# tree, then the tests, once nothing reads the tree any more: their fixtures create
# repositories and scratch files beside the sources.
tests=()
while IFS= read -r -d '' kind && IFS= read -r -d '' label && IFS= read -r -d '' command; do
  if [[ $kind == tests ]]; then
    tests+=("$label" "$command")
  else
    run "$label" bash -c "$command"
  fi
done < <(python3 tools/policy/steps.py list)
settle

for (( index = 0; index < ${#tests[@]}; index += 2 )); do
  run_tests "${tests[$index]}" bash -c "${tests[$index + 1]}"
done
settle

if (( ${#failed[@]} )); then
  printf '\nRepository policy suite failed: %s\n' "$(IFS=';'; printf '%s' "${failed[*]}")"
  exit 1
fi
printf '\nRepository policy suite OK\n'
