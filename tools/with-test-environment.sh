#!/usr/bin/env bash
# Run a test command in the environment every CI test job uses, so a result
# does not depend on who runs it or where:
#
#   - LANG=C.UTF-8 with no other locale override, and TZ=UTC;
#   - a fresh, empty HOME and XDG base directories (config, data, state,
#     cache, runtime) that are deleted afterwards;
#   - no desktop session: no D-Bus session bus, display or keyring socket.
#
# Tests then cannot read a developer's identity, preferences, installed apps
# or live session, and cannot write into them. Cargo and rustup keep using
# their existing homes, so the toolchain and download cache are unaffected.
#
#   tools/with-test-environment.sh COMMAND [ARG ...]
set -euo pipefail
if [ "$#" -eq 0 ]; then
    echo "usage: $0 COMMAND [ARG ...]" >&2
    exit 2
fi

# rustup and Cargo locate themselves relative to HOME unless told otherwise.
# Resolve both before HOME moves; CI images already set them explicitly.
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"

# A short /tmp path keeps Unix socket paths below the 108-byte limit even when
# TMPDIR points into a deeply nested build directory.
root=$(mktemp -d /tmp/tilo-test-env.XXXXXXXX)
cleanup() {
    # Tests may leave read-only fixture trees behind.
    chmod -R u+rwX -- "$root" 2>/dev/null || true
    rm -rf -- "$root"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

mkdir -p "$root"/{home,config,data,state,cache,runtime}
chmod 700 "$root" "$root/runtime"

export HOME="$root/home"
export XDG_CONFIG_HOME="$root/config"
export XDG_DATA_HOME="$root/data"
export XDG_STATE_HOME="$root/state"
export XDG_CACHE_HOME="$root/cache"
export XDG_RUNTIME_DIR="$root/runtime"
# Fall back to the specification defaults, not a desktop's search path.
unset XDG_CONFIG_DIRS XDG_DATA_DIRS

export LANG=C.UTF-8
unset LANGUAGE LC_ALL LC_CTYPE LC_NUMERIC LC_TIME LC_COLLATE LC_MONETARY \
    LC_MESSAGES LC_PAPER LC_NAME LC_ADDRESS LC_TELEPHONE LC_MEASUREMENT \
    LC_IDENTIFICATION
export TZ=UTC

unset DBUS_SESSION_BUS_ADDRESS DISPLAY WAYLAND_DISPLAY XAUTHORITY \
    SSH_AUTH_SOCK GNOME_KEYRING_CONTROL XDG_SESSION_ID XDG_SESSION_TYPE \
    XDG_SESSION_CLASS XDG_SESSION_DESKTOP XDG_CURRENT_DESKTOP DESKTOP_SESSION

if "$@"; then status=0; else status=$?; fi
exit "$status"
