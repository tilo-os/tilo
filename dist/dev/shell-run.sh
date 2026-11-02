#!/usr/bin/env bash
# Run the tilolang shell as the whole screen of a headless tilo-session, the way a
# person uses it, and see it: rendered by Mesa's software Vulkan driver (lavapipe),
# never this machine's GPU; with the tilolang notes, tasks, chat and mail apps
# filled with their seed data (`--seed-apps`); with the agent door admitting
# `tilolang repl` to act as the person (`--admit-repl`); and with the development
# test-control socket `tilo-control snapshot PATH` writes a PNG of the screen over
# (`--test-control`).
#
# Nothing touches the person's own desktop: the session gets a scratch HOME and XDG
# tree, a private XDG_RUNTIME_DIR, no D-Bus and no X11. It prints where its agent
# door and control socket are and how to drive it, then runs until it ends or this
# script is stopped (Ctrl+C, SIGTERM, SIGHUP). Either way the session, everything
# it started, its FUSE mount and the scratch directory go with it.
#
# Usage:
#   dist/dev/shell-run.sh [--no-build] [--size WxH] [--no-apps]
#
# Options:
#   --no-build    use the binaries already in target/debug
#   --no-apps     start without the seeded apps, as a bare session does
#   --size WxH    the headless output's size (default 1280x800)
#
# Drive it from another terminal with the commands it prints:
#   target/debug/tilolang repl --door "$TILO_AGENT_DOOR"
#   TILO_TEST_CONTROL=... target/debug/tilo-control snapshot /tmp/shot.png
set -euo pipefail

repo_root=$(cd "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
say() { printf '==> %s\n' "$*" >&2; }
die() { printf 'shell-run: %s\n' "$*" >&2; exit 1; }

build=1
size=1280x800
apps=(--seed-apps)
while (($#)); do
    case $1 in
        --no-build) build=0; shift ;;
        --no-apps) apps=(); shift ;;
        --size) size=${2:?--size needs WxH}; shift 2 ;;
        -h|--help) sed -n '2,/^set -euo/{/^set -euo/d;s/^# \{0,1\}//;p}' "$0"; exit 0 ;;
        *) die "unknown option $1 (see --help)" ;;
    esac
done

bin=$repo_root/target/debug
if ((build)); then
    say "building tilo-session, tilolang and tilo-control"
    (cd "$repo_root" && cargo build --locked -p tilo-session --bin tilo-session \
        -p tilolang-cli --bin tilolang -p tilo-control --bin tilo-control)
fi
for tool in tilo-session tilolang tilo-control; do
    [[ -x $bin/$tool ]] || die "no $bin/$tool (run without --no-build)"
done

# Lavapipe's loader manifest: the session, and everything it starts, loads this
# driver alone, and an empty TILO_DMABUF_DEVICE advertises no render node, so
# nothing opens /dev/dri.
lavapipe=
for dir in /etc/vulkan/icd.d /usr/share/vulkan/icd.d /usr/local/share/vulkan/icd.d; do
    for icd in "$dir/lvp_icd.$(uname -m).json" "$dir/lvp_icd.json"; do
        [[ -z $lavapipe && -f $icd ]] && lavapipe=$icd
    done
done
[[ -n $lavapipe ]] || die "Mesa's lavapipe is not installed (no lvp_icd.json under */vulkan/icd.d)"
export VK_DRIVER_FILES=$lavapipe VK_ICD_FILENAMES=$lavapipe WGPU_ADAPTER_NAME=llvmpipe
export TILO_DMABUF_DEVICE=

# One scratch directory holds everything; its runtime directory is short, as a
# socket path must be.
scratch=$(mktemp -d /tmp/tilo-shell-run.XXXXXX)
chmod 700 "$scratch"
home=$scratch/home
runtime=$scratch/run
mkdir -m 700 "$home" "$runtime"
log=$scratch/session.log

session_pid=
group=
documents=$runtime/tilo/documents
mounted() { grep -qF " $1 " /proc/self/mountinfo; }
# Stop the session started last: it ends on SIGTERM as it does on its own,
# unmounting what it mounted; one that does not is ended outright, and so is
# whatever else of its process group still runs.
stop_session() {
    if [[ -n $session_pid ]]; then
        kill -TERM "$session_pid" 2>/dev/null || true
        local tries=100
        while ((tries--)) && kill -0 "$session_pid" 2>/dev/null; do sleep 0.1; done
        kill -KILL -- "-$group" 2>/dev/null || true
        wait "$session_pid" 2>/dev/null || true
        session_pid=
    fi
    if [[ -n $group ]]; then
        kill -KILL -- "-$group" 2>/dev/null || true
        group=
    fi
    # A session that could not end on its own leaves its mount behind.
    if mounted "$documents"; then
        say "removing the mount the session left at $documents"
        fusermount3 -u -z "$documents" 2>/dev/null || true
    fi
}
cleanup() {
    # A second signal must not cut the cleanup short and leave the session up.
    trap '' INT TERM HUP
    stop_session
    rm -rf "$scratch" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

door=$runtime/agent.sock
control=$runtime/test-control.sock
# Start tilo-session headless with the arguments given, its output in $log, and
# wait until it printed its control socket, which it does last.
start_session() {
    env -u WAYLAND_DISPLAY -u DISPLAY -u DBUS_SESSION_BUS_ADDRESS \
        HOME="$home" XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share" \
        XDG_STATE_HOME="$home/.local/state" XDG_CACHE_HOME="$home/.cache" \
        XDG_RUNTIME_DIR="$runtime" TILO_IDENTITY_ROOT="$scratch/identity" \
        TILO_AGENT_DOOR="$door" \
        setsid "$bin/tilo-session" --backend headless --size "$size" --no-x11 \
        --session-bus none --system-bus none --test-control "$control" "$@" \
        > "$log" 2>&1 &
    session_pid=$!
    group=$session_pid
    local tries=600
    until grep -q '^TILO_TEST_CONTROL=' "$log" 2>/dev/null; do
        kill -0 "$session_pid" 2>/dev/null || { cat "$log" >&2; die "the session ended as it started"; }
        ((tries--)) || die "the session did not come up (see $log)"
        sleep 0.1
    done
}

# Nobody completes first-use setup here: a first pass mints the throwaway
# identity a development session opens its desk with, as a dev image's does.
say "minting a throwaway identity"
mkdir -m 700 "$scratch/identity"
start_session
minted=$(TILO_TEST_CONTROL=$control "$bin/tilo-control" raw '{"op":"provision_test_identity"}') \
    || die "no throwaway identity: $minted (see $log)"
[[ $minted == *'"throwaway":true'* ]] || die "no throwaway identity: $minted"
stop_session

say "starting the session headless at $size (log: $log)"
start_session --shell screen --admit-repl "$bin/tilolang" "${apps[@]}"
grep -q "^TILO_AGENT_DOOR=$door\$" "$log" || die "the session serves no agent door (see $log)"

cat <<EOF
TILO_AGENT_DOOR=$door
TILO_TEST_CONTROL=$control
XDG_RUNTIME_DIR=$runtime
The session runs (pid $session_pid). From another terminal:
  the REPL:      $bin/tilolang repl --door $door
  a screenshot:  TILO_TEST_CONTROL=$control $bin/tilo-control snapshot /tmp/shot.png
Stop it with Ctrl+C (or SIGTERM to pid $$); everything it started goes with it.
EOF

status=0
wait "$session_pid" || status=$?
session_pid=
exit "$status"
