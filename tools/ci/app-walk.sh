#!/usr/bin/env bash
# Walk one app's first hour twice — in its Rust version and in its tilolang one — on the
# session build TILO_NESTED_SESSION_BUILD names, which this never builds
# (dist/dev/nested-session-build.sh). One step file per app says what a person does
# (scripts/headless-examples/app-walks/); the walk is scripts/headless-examples/app-walk.py.
#
#   tools/ci/app-walk.sh OUT APP [app-walk.py args...]
#
# OUT gets the walk's page (walk.md), its verdict (walk.json), every snapshot and each
# session's log — what the `app-walk/APP` check keeps. Each version's session state is
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
    (cd "$from" && find . -maxdepth 3 -type f \
        \( -name '*.json' -o -name '*.png' -o -name '*.log' -o -name '*.md' \) \
        -size -20M -exec cp --parents -t "$out" {} +)
    rm -rf -- "$run"
}
trap keep EXIT

python3 scripts/headless-examples/app-walk.py "$app" --out "$run/walk" --state "$run/states" "$@"
