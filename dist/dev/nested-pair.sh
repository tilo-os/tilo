#!/usr/bin/env bash
# Two nested Tilo sessions on this machine, two people in one workspace: for
# trying collaboration (presence, shared apps, roles) without a second
# computer and without touching the desktop you work in.
#
# Session A is a nested session (nested-session.sh) with its own throwaway
# identity. Each run it enrols a new Home workspace through the repository's
# governor fixture, which also issues an invitation into that workspace
# (`--invite-guest`). Session B, a second nested session with another
# throwaway identity, redeems it at startup (`--join`): B's active workspace
# is A's Home workspace, with B as an editor. Each session keeps its own state
# directory, runtime directory and D-Bus, so either can be driven on its own.
#
# Live sync and presence between the two do not flow yet: the invitations
# carry the governor fixture's address, which answers joins only.
#
# Usage:
#   dist/dev/nested-pair.sh [options]
#
# Options:
#   --state DIR     parent of the two state directories (default:
#                   $XDG_STATE_HOME/tilo-nested-pair; A in DIR/a, B in DIR/b)
#   --apps "..."    apps to install in both (see nested-session.sh --apps)
#   --headless      both sessions without windows, driven through tilo-control
#   --no-build      use the last build (see nested-session.sh --no-build)
#   --fresh         new throwaway identities for both
#
# Both windows open side by side at 960x800 logical pixels unless
# TILO_WINIT_SIZE says otherwise. The script prints the tilo-control line
# for each session. Ctrl-C (or closing either window) stops both.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
session=$repo_root/dist/dev/nested-session.sh
base=${XDG_STATE_HOME:-$HOME/.local/state}/tilo-nested-pair
apps=
headless=0
build=1
fresh=0

die() { printf 'nested-pair: %s\n' "$*" >&2; exit 1; }
say() { printf '==> %s\n' "$*" >&2; }

while (($#)); do
    case $1 in
        --state) base=${2:?--state needs a directory}; shift 2 ;;
        --apps) apps=${2:?--apps needs a list}; shift 2 ;;
        --headless) headless=1; shift ;;
        --no-build) build=0; shift ;;
        --fresh) fresh=1; shift ;;
        -h|--help) sed -n '2,/^set -euo/{/^set -euo/d;s/^# \{0,1\}//;p}' "$0"; exit 0 ;;
        *) die "unknown option: $1 (see --help)" ;;
    esac
done

base=$(realpath -m "$base")
a=$base/a
b=$base/b
mkdir -p "$a" "$b"
chmod 700 "$base"
host_runtime=${XDG_RUNTIME_DIR:?XDG_RUNTIME_DIR is unset}
# The runtime directory and control socket nested-session.sh gives a state
# directory.
control_of() {
    printf '%s/tilo-nested-%s/control.sock' "$host_runtime" \
        "$(printf '%s' "$1" | sha256sum | cut -c1-12)"
}
control_a=$(control_of "$a")
control_b=$(control_of "$b")
bin=$repo_root/target/nested-session/bin

common=()
((headless)) && common+=(--headless)
[[ -n $apps ]] && common+=(--apps "$apps")
reset=()
((fresh)) && reset+=(--reset-identity)
if ((!headless)) && [[ -z ${TILO_WINIT_SIZE:-} ]]; then
    # Two windows side by side on a laptop screen.
    host_scale=$(busctl --user --json=short call org.gnome.Mutter.DisplayConfig \
        /org/gnome/Mutter/DisplayConfig org.gnome.Mutter.DisplayConfig GetCurrentState \
        2>/dev/null | jq -r '[.data[2][] | select(.[4]) | .[2]][0] // empty' 2>/dev/null) || true
    export TILO_WINIT_SIZE=$(awk -v s="${host_scale:-1}" \
        'BEGIN { if (s + 0 <= 0) s = 1; printf "%dx%d", 960 * s, 800 * s }')
fi

pids=()
cleanup() {
    local pid
    for pid in "${pids[@]}"; do kill -TERM "$pid" 2>/dev/null || true; done
    for pid in "${pids[@]}"; do wait "$pid" 2>/dev/null || true; done
}
trap cleanup EXIT
trap 'exit 130' INT TERM HUP

# --- A ---------------------------------------------------------------------
# A enrols a new Home workspace through the governor fixture, which also
# issues the invitation B joins it with, and keeps answering joins while A runs.
build_flag=()
((build)) || build_flag=(--no-build)
invitation=$base/invitation
rm -f "$invitation"
say "starting session A ($a)"
"$session" --state "$a" "${build_flag[@]}" "${common[@]}" "${reset[@]}" --fresh-desktop \
    --invite-guest "$invitation" &
pids+=("$!")
pid_a=$!

# A serves its control socket once the shell is up, and its Home workspace
# is shared once the boot join made it a native room.
tries=1800
until [[ -S $control_a && -s $invitation && -f $a/native-workspace ]]; do
    kill -0 "$pid_a" 2>/dev/null || die "session A stopped (see $a/logs/shell.log)"
    ((tries--)) || die "session A did not become ready (see $a/logs/native.log)"
    sleep 0.2
done

# --- B ------------------------------------------------------------------------
say "starting session B ($b), joining A's workspace"
"$session" --state "$b" --no-build "${common[@]}" "${reset[@]}" --fresh-desktop --join "$invitation" &
pids+=("$!")
pid_b=$!

cat >&2 <<EOF
    drive A:  XDG_RUNTIME_DIR=$(dirname "$control_a") TILO_TEST_CONTROL=$control_a $bin/tilo-control state-dump
    drive B:  XDG_RUNTIME_DIR=$(dirname "$control_b") TILO_TEST_CONTROL=$control_b $bin/tilo-control state-dump
EOF

# Either session ending ends the pair.
status=0
wait -n "$pid_a" "$pid_b" || status=$?
exit "$status"
