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
#   --peers N       how many sessions (people) meet in the Lounge (default 2,
#                   at most 26): A, B, C, ...
#   --state DIR     parent of the pair's state (default:
#                   $XDG_STATE_HOME/tilo-nested-pair): A in DIR/a, B in DIR/b,
#                   and so on, the site in DIR/site
#   --apps "..."    apps to install in each (see nested-session.sh --apps)
#   --install-tapp FILE  also install this signed package in each (repeatable;
#                   see nested-session.sh --install-tapp)
#   --headless      both sessions without windows, driven through tilo-control
#   --software      render with lavapipe only (see nested-session.sh --software)
#   --no-build      use the last build (see nested-session.sh --no-build)
#   --fresh         new throwaway identities for both: two new people
#   --fresh-site    a new Lounge: forget the site's room and its documents
#   --ports BASE    the site's ports: the web entry on BASE, the room API on
#                   BASE+1, the workspace host on BASE+10 (TLS, live plane)
#                   and BASE+74 (plain HTTP) (default: 8680)
#   --ready FILE    once every session holds the Lounge, write their control
#                   sockets (A's and B's as `a` and `b`, all as `peers`), the
#                   site and the room to FILE as JSON (what
#                   nested-pair-acceptance.py and `tilolang run` read)
#
# Both windows open side by side at 960x800 logical pixels unless
# TILO_WINIT_SIZE says otherwise. The script prints the tilo-control line for
# each session. Ctrl-C (or closing either window) stops both and the site.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
session=$repo_root/dist/dev/nested-session.sh
base=${XDG_STATE_HOME:-$HOME/.local/state}/tilo-nested-pair
apps=
apps_given=0
peers=2
install_tapps=()
headless=0
software=0
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
        --apps) apps=${2?--apps needs a list}; apps_given=1; shift 2 ;;
        --peers) peers=${2:?--peers needs a number}; shift 2 ;;
        --install-tapp) install_tapps+=(--install-tapp "$(realpath -m "${2:?--install-tapp needs a file}")"); shift 2 ;;
        --headless) headless=1; shift ;;
        --software) software=1; shift ;;
        --no-build) build=0; shift ;;
        --fresh) fresh=1; shift ;;
        --fresh-site) fresh_site=1; shift ;;
        --ports) ports=${2:?--ports needs a port}; shift 2 ;;
        --ready) ready_out=$(realpath -m "${2:?--ready needs a file}"); shift 2 ;;
        -h|--help) sed -n '2,/^set -euo/{/^set -euo/d;s/^# \{0,1\}//;p}' "$0"; exit 0 ;;
        *) die "unknown option: $1 (see --help)" ;;
    esac
done
[[ $peers =~ ^[0-9]+$ ]] && ((peers >= 1 && peers <= 26)) || die "--peers needs a number from 1 to 26"
[[ $ports =~ ^[0-9]+$ ]] && ((ports >= 1024 && ports + 74 <= 65535)) \
    || die "--ports needs a port from 1024 that leaves room for 74 more"

base=$(realpath -m "$base")
names=()
for letter in {a..z}; do ((${#names[@]} < peers)) && names+=("$letter"); done
site_dir=$base/site
for name in "${names[@]}"; do mkdir -p "$base/$name"; done
mkdir -p "$site_dir"
a=$base/a
chmod 700 "$base"
host_runtime=${XDG_RUNTIME_DIR:?XDG_RUNTIME_DIR is unset}
# The runtime directory and control socket nested-session.sh gives a state
# directory.
control_of() {
    printf '%s/tilo-nested-%s/control.sock' "$host_runtime" \
        "$(printf '%s' "$1" | sha256sum | cut -c1-12)"
}
controls=()
for name in "${names[@]}"; do controls+=("$(control_of "$base/$name")"); done
bin=${TILO_NESTED_SESSION_BUILD:-$repo_root/target/nested-session}/bin

common=()
((headless)) && common+=(--headless)
((software)) && common+=(--software)
((apps_given)) && common+=(--apps "$apps")
common+=("${install_tapps[@]}")
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

# --- The people ---------------------------------------------------------------
# A arrives first and each next one once the one before is in, as people do:
# the others' apps are packaged meanwhile, so each starts at once.
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
            kill -0 "$pid" 2>/dev/null || die "session $1 or the site stopped (logs: $base/*/logs, $site_dir/site.log)"
        done
        ((tries--)) || die "session $1 did not join the Lounge (see its logs/shell.log)"
        sleep 0.2
    done
    say "session $1 is in the Lounge"
}
session_pids=()
pid_prepare=
for index in "${!names[@]}"; do
    name=${names[$index]}
    upper=${name^^}
    if [[ -n $pid_prepare ]]; then
        wait "$pid_prepare" || die "session $upper could not be prepared (see $base/$name/prepare.log)"
        pid_prepare=
    fi
    say "starting session $upper ($base/$name)"
    "$session" --state "$base/$name" --no-build "${common[@]}" "${reset[@]}" --community "$site" &
    pids+=("$!")
    pid=$!
    session_pids+=("$pid")
    if ((index + 1 < ${#names[@]})); then
        next=$base/${names[$((index + 1))]}
        "$session" --state "$next" --no-build "${common[@]}" --prepare-only > "$next/prepare.log" 2>&1 &
        pid_prepare=$!
        pids+=("$pid_prepare")
    fi
    wait_in_lounge "$upper" "$pid" "${controls[$index]}"
done
for index in "${!names[@]}"; do
    control=${controls[$index]}
    printf '    drive %s:  XDG_RUNTIME_DIR=%s TILO_TEST_CONTROL=%s %s state-dump\n' \
        "${names[$index]^^}" "$(dirname "$control")" "$control" "$bin/tilo-control" >&2
done

# Each person goes to the Lounge and shows where they are there, as one
# does from the avatar menu: a public workspace starts at "that I'm here",
# which shares no pointer.
for control in "${controls[@]}"; do
    lounge=$(TILO_TEST_CONTROL=$control "$bin/tilo-control" state-dump \
        | jq -r --arg room "$room" '.workspace_tree as $t | $t.workspaces[]
            | select((.room | tostring) == $room) | select(.id != $t.active) | .room')
    if [[ -n $lounge ]]; then
        TILO_TEST_CONTROL=$control "$bin/tilo-control" actions invoke actions \
            "workspace.select.$lounge" >/dev/null || say "could not go to the Lounge"
    fi
    TILO_TEST_CONTROL=$control "$bin/tilo-control" raw \
        '{"op":"actions_invoke","provider":"actions","id":"presence.visibility.set","args":{"visibility":"where"}}' \
        >/dev/null || say "could not show where the person is"
done
say "everyone is in the Lounge, showing where they are"
if [[ -n $ready_out ]]; then
    printf '%s\n' "${controls[@]}" | jq -R . | jq -s --arg site "$site" --arg room "$room" \
        --arg control "$bin/tilo-control" \
        '{a: .[0], b: .[1], peers: ., site: $site, room: $room, tilo_control: $control}' \
        > "$ready_out.partial"
    mv "$ready_out.partial" "$ready_out"
fi

# Any session or the site ending ends them all.
status=0
wait -n "$pid_site" "${session_pids[@]}" || status=$?
exit "$status"
