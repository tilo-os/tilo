#!/usr/bin/env bash
# Prove an installed tilo-runtime serves: the daemon starts on a private
# profile, hands its service connections to an attached process, answers MCP
# over its native endpoint, and the packaged Tilo app renders headless.
#
# By default it runs as root and drops to a throwaway user with a cleared
# environment, so it can neither see nor create a developer's real runtime
# profile. TILO_RUNTIME_SMOKE_USER=current keeps the calling user (a Nix build
# userspace has no useradd) with a throwaway HOME, passing VK_ICD_FILENAMES.
# Paths default to the FHS package; TILO_RUNTIME_DIR names another private
# directory with its layout, and TILO_RUNTIME_BIN another entry point.
set -euo pipefail

[[ $# == 0 ]] || { echo "usage: $0" >&2; exit 2; }
private=${TILO_RUNTIME_DIR:-/usr/libexec/tilo-runtime}
runtime=${TILO_RUNTIME_BIN:-/usr/bin/tilo-runtime}
app=$private/store
mode=${TILO_RUNTIME_SMOKE_USER:-throwaway}
test -x "$runtime"
test -x "$app"

work=$(mktemp -d)
profile=$work/home/.local/state/tilo/runtime
smoke_user=tilort$$
as_user=()
daemon_pid=
cleanup() {
    [[ -z $daemon_pid ]] || kill "$daemon_pid" 2>/dev/null || true
    [[ -z $daemon_pid ]] || wait "$daemon_pid" 2>/dev/null || true
    [[ $mode != throwaway ]] || userdel "$smoke_user" >/dev/null 2>&1 || true
    rm -rf -- "$work"
}
trap cleanup EXIT
fail() {
    echo "tilo-runtime smoke test failed: $1" >&2
    for log in daemon attach mcp shots; do
        [[ -s $work/$log.log ]] || continue
        echo "--- $log.log ---" >&2
        cat "$work/$log.log" >&2
    done
    if [[ -e $profile/startup.stages ]]; then
        echo "--- startup stages ---" >&2
        cat "$profile/startup.stages" >&2
    fi
    echo "--- profile path ---" >&2
    namei -l "$profile" >&2 || true
    exit 1
}
# The daemon names no path with its errors; where strace is installed, a
# failed start is rerun under it to show the refused calls. The tracer runs
# outside the user switch: the daemon makes itself non-dumpable before it
# loads key material, and only a privileged tracer can still read its paths.
trace_daemon() {
    command -v strace >/dev/null || return 0
    rm -rf -- "$profile"
    local trace=$work/daemon.strace
    strace -f -y -s 256 -o "$trace" "${as_user[@]}" "$runtime" --daemon \
        >/dev/null 2>&1 &
    local traced=$!
    sleep 20
    kill "$traced" 2>/dev/null || true
    wait "$traced" 2>/dev/null || true
    echo "--- refused calls ---" >&2
    grep -E 'E(ACCES|PERM)' "$trace" | tail -n 20 >&2 || true
}

case $mode in
throwaway)
    [[ $(id -u) == 0 ]] || { echo "the throwaway-user smoke test must run as root" >&2; exit 2; }
    useradd --home-dir "$work/home" --create-home --shell /bin/bash "$smoke_user"
    # The home's parent is readable, as /home is on every packaged distribution.
    chmod 0755 "$work"
    install -d -m 0700 -o "$smoke_user" -g "$(id -g "$smoke_user")" "$work/runtime"
    as_user=(runuser -u "$smoke_user" -- env -i
        HOME="$work/home" USER="$smoke_user" LOGNAME="$smoke_user"
        PATH=/usr/local/bin:/usr/bin:/bin XDG_RUNTIME_DIR="$work/runtime"
        WGPU_BACKEND=vulkan)
    ;;
current)
    install -d -m 0700 "$work/home" "$work/runtime"
    as_user=(env -i HOME="$work/home" PATH="$PATH" XDG_RUNTIME_DIR="$work/runtime"
        ${VK_ICD_FILENAMES:+VK_ICD_FILENAMES="$VK_ICD_FILENAMES"} WGPU_BACKEND=vulkan)
    ;;
*) echo "TILO_RUNTIME_SMOKE_USER must be throwaway or current" >&2; exit 2 ;;
esac
# A working directory the smoke user may enter, as a login session has.
cd "$work/home"

# The daemon the systemd user unit starts, on the default profile.
"${as_user[@]}" "$runtime" --daemon >"$work/daemon.log" 2>&1 &
daemon_pid=$!
for _ in $(seq 120); do
    [[ -S $profile/native.sock ]] && break
    kill -0 "$daemon_pid" 2>/dev/null || { trace_daemon; fail 'the daemon exited during startup'; }
    sleep 0.5
done
[[ -S $profile/native.sock ]] || fail 'the daemon did not bind its native endpoint'
[[ $(stat -c %a "$profile") == 700 ]] || fail 'the profile is not private'

# An attached process receives the collaboration and identity connections.
# shellcheck disable=SC2016 # expanded by the attached shell, not this one
"${as_user[@]}" "$runtime" --run-app so.tilo.smoke -- /bin/sh -c \
    'test -n "$TILO_SVC_COLLAB_FD" && test -n "$TILO_SVC_IDENTITY_FD" && echo "attached $TILO_PACKAGE_ID"' \
    >"$work/attach.log" 2>&1 || fail 'attaching a process failed'
grep -Fxq 'attached so.tilo.smoke' "$work/attach.log" || fail 'the attached process got no service connections'

# The same daemon answers MCP through the stdio proxy an assistant configures.
printf '%s\n' \
    '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke","version":"1"}}}' \
    '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
    '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
    | "${as_user[@]}" timeout 60 "$runtime" --connect >"$work/mcp.log" 2>&1 || true
grep -q '"id":2' "$work/mcp.log" || fail 'MCP tools/list got no reply'
grep -q '"tools"' "$work/mcp.log" || fail 'MCP tools/list listed no tools'

# The packaged SDK app starts and renders without a display.
"${as_user[@]}" "$app" --shots "$work/runtime/store.png" >"$work/shots.log" 2>&1 \
    || fail 'the Tilo app could not render headless'
[[ -s $work/runtime/store.png ]] || fail 'the Tilo app rendered no image'

echo 'installed tilo-runtime daemon, attachment, MCP and headless app: PASS'
