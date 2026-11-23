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

# The walk's session runs as an ordinary account, as a person's desktop does on
# their own machine: Chromium refuses its sandbox as root, and the shell runtime
# is the person's user service. This script itself runs as root in the check's
# VM, where the walk stages the mail provider's host names, trust bundles and
# mail ports; the session and everything it starts are the account's
# (scripts/headless-examples/real_apps.py, $TILO_SESSION_USER).
if [[ $(id -u) == 0 ]]; then
    walk_account=${TILO_SESSION_USER:-walker}
    id "$walk_account" >/dev/null 2>&1 \
        || useradd --create-home --shell /bin/bash "$walk_account"
    export TILO_SESSION_USER=$walk_account
    # The seat's runtime directory, which logind gives a login at /run/user/<uid>: the launcher's
    # system grant, the compositor's display snapshot and the session's own sockets are all named
    # from it, as on a device.
    walk_uid=$(id -u "$walk_account")
    install -d -m 0700 -o "$walk_account" -g "$(id -g "$walk_account")" "/run/user/$walk_uid"
    export TILO_SESSION_RUNTIME="/run/user/$walk_uid"
    # The machine's authorities Settings reads: a system bus of the session's own, with the power
    # services a device's systemd starts on it.
    export TILO_SESSION_SYSTEM_BUS=1
fi

out=$(realpath -m "$out")
# The walk's scratch, on a tree the session account can read all the way up:
# the job's own temporary directory belongs to this caller alone (0700), and an
# app's state is synced to every directory above it, so a session's state under
# it fails to be written (the shell runtime's environment record, an app's
# records). A person's state lives under their home, readable by them.
base=$(mktemp -d -p /var/tmp tilo-app-walk.XXXXXXXX)
chmod 0755 "$base"
run=$base/run
states=$base/states
mkdir -m 0755 "$run" "$states"
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
    for log in "$states"/*/logs/*.log; do
        [[ -f $log ]] || continue
        cp "$log" "$out/$(basename "$(dirname "$(dirname "$log")")")-$(basename "$log")"
    done
    # A crash report says why an app stopped: it is the proof a walk that saw one needs.
    local state report
    for report in "$states"/*/cache/tilo/crashes/*; do
        [[ -f $report ]] || continue
        state=$(basename "$(dirname "$(dirname "$(dirname "$(dirname "$report")")")")")
        cp "$report" "$out/crashes-$state-$(basename "$report")"
    done
    rm -rf -- "$base"
}
trap keep EXIT

args=()
[[ -n ${T_INPUT_ONLY:-} ]] && args+=(--only "$T_INPUT_ONLY")
[[ -n ${T_INPUT_DISCOVER:-} ]] && args+=(--discover)
# Each walk is held to its baseline from main where it has one.
args+=(--compare)
python3 scripts/headless-examples/app-walk.py "$app" --out "$run/walk" --state "$states" \
    ${args[@]+"${args[@]}"} "$@"
