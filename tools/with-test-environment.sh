#!/usr/bin/env bash
# Run a test command in the environment every test run uses, so a result
# does not depend on who runs it or where, and a test cannot touch the
# desktop of whoever runs it:
#
#   - LANG=C.UTF-8 with no other locale override, TZ=UTC, and the US
#     keyboard layout (XKB_DEFAULT_LAYOUT=us);
#   - a fresh, empty HOME and XDG base directories (config, data, state,
#     cache, runtime) that are deleted afterwards;
#   - no desktop session: no display, keyring (on macOS, no login Keychain),
#     audio or systemd socket, and
#     both D-Bus addresses pointed at a socket that never exists. Code that
#     falls back to the default session bus, the systemd user manager
#     ($XDG_RUNTIME_DIR/systemd), logind or a portal fails to connect instead
#     of pausing the caller's media, opening a file chooser or ending their
#     session.
#
# Tests then cannot read a developer's identity, preferences, installed apps
# or live session, and cannot write into them. Cargo and rustup keep using
# their existing homes, so the toolchain and download cache are unaffected.
#
# Cargo runs every test binary through here (tools/cargo-runner.sh), so a
# plain `cargo test` gets the same environment as CI. A command that already
# runs inside an intact test environment keeps it, with whatever an enclosing
# wrapper (headless audio, fixtures) added on top.
#
#   tools/with-test-environment.sh COMMAND [ARG ...]
#   tools/with-test-environment.sh --check   # exit 0 only inside one
set -euo pipefail
if [ "$#" -eq 0 ]; then
    echo "usage: $0 COMMAND [ARG ...] | --check" >&2
    exit 2
fi

# Variables that name a desktop session, its display or its services.
session_vars=(
    DISPLAY WAYLAND_DISPLAY WAYLAND_SOCKET XAUTHORITY
    DBUS_STARTER_ADDRESS DBUS_STARTER_BUS_TYPE AT_SPI_BUS_ADDRESS
    SSH_AUTH_SOCK GNOME_KEYRING_CONTROL GPG_AGENT_INFO
    PULSE_SERVER PIPEWIRE_REMOTE PIPEWIRE_RUNTIME_DIR
    NOTIFY_SOCKET LISTEN_PID LISTEN_FDS LISTEN_FDNAMES MANAGERPID
    XDG_SESSION_ID XDG_SESSION_TYPE XDG_SESSION_CLASS XDG_SESSION_DESKTOP
    XDG_CURRENT_DESKTOP XDG_SEAT XDG_VTNR XDG_ACTIVATION_TOKEN
    DESKTOP_SESSION DESKTOP_STARTUP_ID GNOME_SETUP_DISPLAY
    SWAYSOCK I3SOCK NIRI_SOCKET HYPRLAND_INSTANCE_SIGNATURE
)

isolated() {
    local root=${TILO_TEST_ENVIRONMENT:-} name
    [ -n "$root" ] && [ -d "$root/runtime" ] || return 1
    [ "${HOME:-}" = "$root/home" ] || return 1
    [ "${XDG_RUNTIME_DIR:-}" = "$root/runtime" ] || return 1
    [ "${DBUS_SESSION_BUS_ADDRESS:-}" = "unix:path=$root/no-session-bus" ] || return 1
    [ "${DBUS_SYSTEM_BUS_ADDRESS:-}" = "unix:path=$root/no-system-bus" ] || return 1
    for name in DISPLAY WAYLAND_DISPLAY WAYLAND_SOCKET XDG_SESSION_ID NOTIFY_SOCKET; do
        [ -z "${!name:-}" ] || return 1
    done
}
if [ "$1" = --check ]; then
    isolated
    exit
fi
if isolated; then
    exec "$@"
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

export TILO_TEST_ENVIRONMENT="$root"
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
# The machine's keyboard layout is a default too (an unset input section
# types it), so pin it like the locale: US, no variant, no options.
export XKB_DEFAULT_LAYOUT=us
unset XKB_DEFAULT_VARIANT XKB_DEFAULT_MODEL XKB_DEFAULT_OPTIONS XKB_DEFAULT_RULES

unset "${session_vars[@]}"
# Nothing ever listens on these paths. A test that needs a bus brings its own
# (a p2p socket pair, or a private dbus-daemon) and hands its address to the
# code under test.
export DBUS_SESSION_BUS_ADDRESS="unix:path=$root/no-session-bus"
export DBUS_SYSTEM_BUS_ADDRESS="unix:path=$root/no-system-bus"
# macOS: tilo-secrets answers "no store" here instead of reaching the login
# Keychain, and identity secrets, which never fall back there, stay in files.
if [ "$(uname -s)" = Darwin ]; then
    export TILO_IDENTITY_FILE_SECRETS=1
fi

if "$@"; then status=0; else status=$?; fi
exit "$status"
