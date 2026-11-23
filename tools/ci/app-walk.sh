#!/usr/bin/env bash
# Walk one part of a person's first hour in every version its step file names that is still in
# the tree — the Rust shell and the tilolang one, a Rust app and its port — on the session build
# TILO_NESTED_SESSION_BUILD names, which this never builds (dist/dev/nested-session-build.sh). One
# step file per walk says what a person does (scripts/headless-examples/app-walks/); the walk is
# scripts/headless-examples/app-walk.py. Where the walk has a baseline from main
# (app-walks/baseline/WALK.json), what it found is held to it, and a regression fails.
#
#   tools/ci/app-walk.sh OUT WALK|FAMILY [app-walk.py args...]
#
# T_INPUT_ONLY (rust|tl) walks one version, T_INPUT_DISCOVER=1 keeps every step's controls.
#
# OUT gets the walk's page (walk.md), what it found (walk.json, summary.json), the comparison
# (compare.md, compare.json), every snapshot and each session's log — what the `app-walk/WALK`
# check keeps. Each version's session state is
# temporary: it is the sessions' installed apps, not what a person reads the walk by.
set -euo pipefail

out=${1:?usage: $0 OUT_DIR APP [args...]}
app=${2:?usage: $0 OUT_DIR APP [args...]}
shift 2
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
cd "$repo_root"

target=$(realpath -m "${CARGO_TARGET_DIR:-target}")
build=$(realpath -m "${TILO_NESTED_SESSION_BUILD:-$target/nested-session}")
[[ -x $build/bin/tilo-session ]] || {
    echo "no session build in $build (dist/dev/nested-session-build.sh)" >&2
    exit 1
}
export TILO_NESTED_SESSION_BUILD=$build
# The walks are scripts, not modules: nothing may drop bytecode into the tree.
export PYTHONDONTWRITEBYTECODE=1

out=$(realpath -m "$out")
run=$(mktemp -d)
rm -rf -- "$out"
mkdir -p -- "$out"
# What a person reads a walk by: its page, its verdict, the snapshots and the logs.
keep() {
    local from=$run/walk
    [[ -d $from ]] || from=$run
    (cd "$from" && find . -maxdepth 6 -type f \
        \( -name '*.json' -o -name '*.png' -o -name '*.log' -o -name '*.md' \) \
        -size -20M -exec cp --parents -t "$out" {} +)
    echo "app-walk: kept $(find "$out" -name '*.png' | wc -l) of $(find "$from" -name '*.png' | wc -l) snapshots" >&2
    # Each session's own logs say what an app did, and why one did not open: the walk's logs
    # are only the launcher's word for the session, which ends with it. Each keeps its own name
    # (state-tl-session.log, state-tl-pipewire.log): the session's and its sound's are apart.
    local log
    for log in "$run"/states/*/logs/*.log; do
        [[ -f $log ]] || continue
        cp "$log" "$out/$(basename "$(dirname "$(dirname "$log")")")-$(basename "$log")"
    done
    # A crash report says why an app stopped: it is the proof a walk that saw one needs.
    local state report
    for report in "$run"/states/*/cache/tilo/crashes/*; do
        [[ -f $report ]] || continue
        state=$(basename "$(dirname "$(dirname "$(dirname "$(dirname "$report")")")")")
        cp "$report" "$out/crashes-$state-$(basename "$report")"
    done
    rm -rf -- "$run"
}
trap keep EXIT

args=()
[[ -n ${T_INPUT_ONLY:-} ]] && args+=(--only "$T_INPUT_ONLY")
[[ -n ${T_INPUT_DISCOVER:-} ]] && args+=(--discover)
# Each walk is held to its baseline from main where it has one.
args+=(--compare)
python3 scripts/headless-examples/app-walk.py "$app" --out "$run/walk" --state "$run/states" \
    ${args[@]+"${args[@]}"} "$@"
