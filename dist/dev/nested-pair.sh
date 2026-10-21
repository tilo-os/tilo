#!/usr/bin/env bash
# Two nested Tilo sessions on this machine, two people together in one
# community workspace: for trying collaboration (presence, shared apps, voice)
# without a second computer and without touching the desktop you work in.
#
# It runs a local site the way tilo.coffee runs: the room service in
# persistent mode with its workspace host (tilo-relay), and the site's
# entry.json naming that host's room as the site's home workspace, "Lounge",
# with the room service's door admitting people (crates/hosting/
# tilo-room-service/dev/run.py --no-bundle). Sessions A and B are nested
# sessions (nested-session.sh), each with its own throwaway identity, whose
# community site is that local site: each joins the Lounge through the door
# exactly as a Tilo computer joins tilo.coffee's, and holds it over the host's
# routes. Documents, presence (the roster, pointers, carets, following) and
# calls flow between them through the host, as between two computers. Each
# session keeps its own state directory, runtime directory and D-Bus, so either
# can be driven on its own with tilo-control.
#
# Usage:
#   dist/dev/nested-pair.sh [options]
#
# Options:
#   --state DIR     parent of the pair's state (default:
#                   $XDG_STATE_HOME/tilo-nested-pair): A in DIR/a, B in DIR/b,
#                   the site in DIR/site
#   --apps "..."    apps to install in both (see nested-session.sh --apps)
#   --headless      both sessions without windows, driven through tilo-control
#   --no-build      use the last build (see nested-session.sh --no-build)
#   --fresh         new throwaway identities for both: two new people
#   --fresh-site    a new Lounge: forget the site's room and its documents
#   --ports BASE    the site's ports: the web entry on BASE, the room API on
#                   BASE+1, the workspace host on BASE+10 (TLS, live plane)
#                   and BASE+74 (plain HTTP) (default: 8680)
#   --ready FILE    once both sessions hold the Lounge, write their control
#                   sockets, the site and the room to FILE as JSON (what
#                   nested-pair-acceptance.py reads)
#
# Both windows open side by side at 960x800 logical pixels unless
# TILO_WINIT_SIZE says otherwise. The script prints the tilo-control line for
# each session. Ctrl-C (or closing either window) stops both and the site.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
session=$repo_root/dist/dev/nested-session.sh
base=${XDG_STATE_HOME:-$HOME/.local/state}/tilo-nested-pair
apps=
headless=0
build=1
fresh=0
fresh_site=0
ports=8680
ready_out=

die() { printf 'nested-pair: %s\n' "$*" >&2; exit 1; }
say() { printf '==> %s\n' "$*" >&2; }

while (($#)); do
    case $1 in
        --state) base=${2:?--state needs a directory}; shift 2 ;;
        --apps) apps=${2:?--apps needs a list}; shift 2 ;;
        --headless) headless=1; shift ;;
        --no-build) build=0; shift ;;
        --fresh) fresh=1; shift ;;
        --fresh-site) fresh_site=1; shift ;;
        --ports) ports=${2:?--ports needs a port}; shift 2 ;;
        --ready) ready_out=$(realpath -m "${2:?--ready needs a file}"); shift 2 ;;
        -h|--help) sed -n '2,/^set -euo/{/^set -euo/d;s/^# \{0,1\}//;p}' "$0"; exit 0 ;;
        *) die "unknown option: $1 (see --help)" ;;
    esac
done
[[ $ports =~ ^[0-9]+$ ]] && ((ports >= 1024 && ports + 74 <= 65535)) \
    || die "--ports needs a port from 1024 that leaves room for 74 more"

base=$(realpath -m "$base")
a=$base/a
b=$base/b
site_dir=$base/site
mkdir -p "$a" "$b" "$site_dir"
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
    # A second signal must not cut the cleanup short and leave a session up.
    trap '' INT TERM HUP
    for pid in "${pids[@]}"; do kill -TERM "$pid" 2>/dev/null || true; done
    for pid in "${pids[@]}"; do wait "$pid" 2>/dev/null || true; done
    [[ -z $ready_out ]] || rm -f "$ready_out"
}
trap cleanup EXIT
trap 'exit 130' INT TERM HUP

# --- Build -------------------------------------------------------------------
# One build serves the site and both sessions; A's apps are installed with it.
if ((build)); then
    "$session" --state "$a" --prepare-only "${common[@]}"
fi
for name in tilo-relay tilo-room-service tilo-control; do
    [[ -x $bin/$name || -f $bin/$name ]] || die "missing $bin/$name (run without --no-build)"
    chmod +x "$bin/$name"
done

# --- The site ----------------------------------------------------------------
# The room service in persistent mode and its relay, as tilo.coffee runs them,
# with the site's entry.json in front. Every session on this machine joins from
# one address, so the door's per-address guest limits are an operator's
# setting here, not a visitor's.
ready_site=$site_dir/ready.json
rm -f "$ready_site"
site_args=(--scratch "$site_dir" --name lounge --shown-name Lounge --home-app so.tilo.chat
           --no-bundle --relay-bin "$bin/tilo-relay" --room-service-bin "$bin/tilo-room-service"
           --web-port "$ports" --api-port "$((ports + 1))" --port-base "$((ports + 10))"
           --ready-file "$ready_site")
((fresh_site)) && site_args+=(--fresh)
say "starting the local site (log: $site_dir/site.log)"
TILO_ROOMS_GUEST_JOINS_PER_10_MIN=1000 TILO_ROOMS_GUEST_JOINS_PER_DAY=100000 \
    python3 -I "$repo_root/crates/hosting/tilo-room-service/dev/run.py" "${site_args[@]}" \
    > "$site_dir/site.log" 2>&1 &
pids+=("$!")
pid_site=$!
tries=1200
until [[ -s $ready_site ]]; do
    kill -0 "$pid_site" 2>/dev/null || die "the local site stopped (see $site_dir/site.log)"
    ((tries--)) || die "the local site did not come up (see $site_dir/site.log)"
    sleep 0.1
done
site=$(jq -er .site "$ready_site")
room=$(jq -er .room "$ready_site")
say "the Lounge is up at $site (room $room)"

# --- A and B -------------------------------------------------------------------
# A arrives first and B once A is in, as two people do: B's apps are
# packaged meanwhile, so B starts at once.
holds_lounge() {
    TILO_TEST_CONTROL=$1 "$bin/tilo-control" state-dump 2>/dev/null \
        | jq -e --arg room "$room" 'any(.workspace_tree.workspaces[]?; (.room | tostring) == $room)' \
        >/dev/null 2>&1
}
# Wait until session $1 (pid $2, control socket $3) holds the Lounge: it
# joined through the door and its enrollment installed.
wait_in_lounge() {
    local tries=1800
    until [[ -S $3 ]] && holds_lounge "$3"; do
        for pid in "$pid_site" "$2"; do
            kill -0 "$pid" 2>/dev/null || die "session $1 or the site stopped (logs: $a/logs, $b/logs, $site_dir/site.log)"
        done
        ((tries--)) || die "session $1 did not join the Lounge (see its logs/shell.log)"
        sleep 0.2
    done
    say "session $1 is in the Lounge"
}
say "starting session A ($a)"
"$session" --state "$a" --no-build "${common[@]}" "${reset[@]}" --community "$site" &
pids+=("$!")
pid_a=$!
"$session" --state "$b" --no-build "${common[@]}" --prepare-only > "$b/prepare.log" 2>&1 &
pid_prepare=$!
pids+=("$pid_prepare")
wait_in_lounge A "$pid_a" "$control_a"
wait "$pid_prepare" || die "session B could not be prepared (see $b/prepare.log)"
say "starting session B ($b)"
"$session" --state "$b" --no-build "${common[@]}" "${reset[@]}" --community "$site" &
pids+=("$!")
pid_b=$!
wait_in_lounge B "$pid_b" "$control_b"
cat >&2 <<EOF
    drive A:  XDG_RUNTIME_DIR=$(dirname "$control_a") TILO_TEST_CONTROL=$control_a $bin/tilo-control state-dump
    drive B:  XDG_RUNTIME_DIR=$(dirname "$control_b") TILO_TEST_CONTROL=$control_b $bin/tilo-control state-dump
EOF

# Each person goes to the Lounge and shows where they are there, as one
# does from the avatar menu: a public workspace starts at "that I'm here",
# which shares no pointer.
for control in "$control_a" "$control_b"; do
    lounge=$(TILO_TEST_CONTROL=$control "$bin/tilo-control" state-dump \
        | jq -r --arg room "$room" '.workspace_tree as $t | $t.workspaces[]
            | select((.room | tostring) == $room) | select(.id != $t.active) | .id')
    if [[ -n $lounge ]]; then
        TILO_TEST_CONTROL=$control "$bin/tilo-control" actions invoke actions \
            "workspace.select.$lounge" >/dev/null || say "could not go to the Lounge"
    fi
    TILO_TEST_CONTROL=$control "$bin/tilo-control" raw \
        '{"op":"actions_invoke","provider":"actions","id":"presence.visibility.set","args":{"visibility":"where"}}' \
        >/dev/null || say "could not show where the person is"
done
say "both people are in the Lounge, showing where they are"
if [[ -n $ready_out ]]; then
    jq -n --arg a "$control_a" --arg b "$control_b" --arg site "$site" --arg room "$room" \
        --arg control "$bin/tilo-control" \
        '{a: $a, b: $b, site: $site, room: $room, tilo_control: $control}' > "$ready_out.partial"
    mv "$ready_out.partial" "$ready_out"
fi

# Either session or the site ending ends the pair.
status=0
wait -n "$pid_site" "$pid_a" "$pid_b" || status=$?
exit "$status"
