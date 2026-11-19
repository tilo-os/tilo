#!/usr/bin/env bash
# Fast, deterministic repository-policy suite shared by CI and the build server.
#
# Its steps are independent, and most wait on files and on the processes they start far more
# than they compute: one after another they took many minutes. They run side by side instead,
# POLICY_JOBS at a time (8 by default), first the checks of the tree, then the tests, whose
# fixtures write beside the sources; each step's output is printed whole, in the order below,
# and every failing step is named.
#
#   run.sh [--local] [--node | --no-node]
#
# This is not what a developer machine is for: the suite is dozens of checks saturating every
# core at once, which belongs on the build server. `t test policy` gives the same steps jobs
# there, one each, keyed by what they read (`t test policy/<step>` for one). So run.sh refuses on
# a developer machine, with that hint, and --local takes it here anyway: a machine with no server
# to hand it to, and tools/ci/local_affected.py when the policy stage is asked for by name.
#
# --no-node runs it without the test files that run Node.js scripts, --node just those
# (tools/policy/steps.toml, `node_tests`): the two halves for where Node.js and the Git
# checkout are on different machines (tools/ci/local_affected.py).
set -euo pipefail
usage="usage: run.sh [--local] [--node | --no-node]"
explicit_local=0
mode=
for arg in "$@"; do
  case "$arg" in
    --local) explicit_local=1 ;;
    --node|--no-node)
      [ -z "$mode" ] || { echo "$usage" >&2; exit 2; }
      mode=$arg ;;
    *) echo "$usage" >&2; exit 2 ;;
  esac
done

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$repo"

# Where the suite belongs besides a machine that asks with --local: CI's quality job, a t job (it
# gives each step a job of its own and sets T_IMAGE), and a build server's copy of the tree, which
# has no Git checkout and so no way to name itself to t.
on_server=1
if [ -z "${CI:-}${GITHUB_ACTIONS:-}${GITEA_ACTIONS:-}${FORGEJO_ACTIONS:-}${T_IMAGE:-}" ] \
  && git -C "$repo" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  on_server=0
fi
if [ "$on_server" = 0 ] && [ "$explicit_local" = 0 ] && [ "${TILO_POLICY_LOCAL:-}" != 1 ]; then
  echo "run.sh: hand the policy suite to the build server: \`t test policy\` (one step: \`t test policy/<step>\`); \`run.sh --local\` takes it here instead." >&2
  exit 1
fi

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

# Tests build Git repositories of their own: a GIT_DIR from the environment would take over
# their Git commands.
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

# The steps (tools/policy/steps.toml): first the checks of the tree, then the tests, once nothing reads the tree any more: their fixtures create
# repositories and scratch files beside the sources.
# Listed into a file first: a manifest steps.py refuses fails the suite, where a process
# substitution's failure would go unheard and leave a suite of no steps that passes.
list_args=()
[ -z "$mode" ] || list_args=("$mode")
python3 tools/policy/steps.py list "${list_args[@]}" >"$work/steps"
tests=()
while IFS= read -r -d '' kind && IFS= read -r -d '' label && IFS= read -r -d '' command; do
  if [[ $kind == tests ]]; then
    tests+=("$label" "$command")
  else
    run "$label" bash -c "$command"
  fi
done <"$work/steps"
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
