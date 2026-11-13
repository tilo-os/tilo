#!/usr/bin/env bash
# Run a test command in the environment every test run uses, so a result
# does not depend on who runs it or where, and a test cannot touch the
# desktop of whoever runs it:
#
#   - LANG=C.UTF-8 with no other locale override, TZ=UTC, and the US
#     keyboard layout (XKB_DEFAULT_LAYOUT=us);
#   - Mesa's CPU Vulkan driver (lavapipe) as the only GPU, the renderer the
#     visual goldens are recorded with, so no test reaches a real GPU and a
#     picture does not depend on the machine's;
#   - a fresh, empty HOME, TMPDIR and XDG base directories (config, data,
#     state, cache, runtime) that are deleted afterwards, all below /tmp;
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
#
# tools/cargo-runner.sh reads this file into its own shell (`source`) and
# calls the functions below, so a test start is one shell and not a chain of
# them. A fresh environment starts a program only for what the shell cannot
# do itself: making the directories, handing them to the test user where
# there is one, and removing them.
set -euo pipefail

case ${BASH_SOURCE[0]} in
    */*) tilo_test_tools=${BASH_SOURCE[0]%/*} ;;
    *) tilo_test_tools=. ;;
esac
# The tests run as the test user where the run started as root.
# shellcheck source=tools/as-test-user.sh
source "$tilo_test_tools/as-test-user.sh"

# Variables that name a desktop session, its display or its services.
tilo_test_session_vars=(
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

# Whether this shell already runs inside an intact test environment.
tilo_test_environment_intact() {
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

# The kernel's name, as `uname -s` gives it. bash knows it (OSTYPE) and never
# exports it: a value the environment brought instead is exported, and is not
# believed; nor is the OSTYPE of a bash too old to tell the two apart.
tilo_test_find_kernel() {
    if [ "${BASH_VERSINFO[0]}" -ge 5 ] && [[ ${OSTYPE@a} != *x* ]]; then
        case $OSTYPE in
            linux*) tilo_test_kernel=Linux; return ;;
            darwin*) tilo_test_kernel=Darwin; return ;;
        esac
    fi
    tilo_test_kernel=$(uname -s)
}

# Anything mounted under the root goes before the tree: a runtime a test
# started can leave its FUSE mount behind, and a mount point cannot be
# removed while it is mounted. Deepest first, Linux only (mount points below
# the root carry no spaces: the template has none).
tilo_test_environment_detach_mounts() {
    [ -r /proc/self/mountinfo ] || return 0
    local root=$tilo_test_environment_root point mounted=
    # Nearly every test leaves none: the shell reads the table itself, and the
    # programs that order the mount points start only where it names one.
    while read -r _ _ _ _ point _; do
        case $point in
            "$root"/*) mounted=1; break ;;
        esac
    done </proc/self/mountinfo
    [ -n "$mounted" ] || return 0
    while IFS= read -r point; do
        fusermount3 -u -z -- "$point" 2>/dev/null ||
            umount -l -- "$point" 2>/dev/null || true
    done < <(
        awk -v root="$root/" 'index($5, root) == 1 { print $5 }' /proc/self/mountinfo |
            awk '{ print length($0), $0 }' | sort -rn | cut -d' ' -f2-
    )
}

tilo_test_environment_cleanup() {
    local root=$tilo_test_environment_root
    tilo_test_environment_detach_mounts
    # Tests may leave read-only fixture trees behind, which a removal cannot
    # enter: only where one is left is the tree made writable and removed
    # again. What the root still holds after that is not the command's
    # result: the run reports its own status, never the cleanup's.
    rm -rf -- "$root" 2>/dev/null || true
    if [ -e "$root" ]; then
        chmod -R u+rwX -- "$root" 2>/dev/null || true
        rm -rf -- "$root" 2>/dev/null || true
    fi
}

# Make a fresh environment the environment of this shell, and so of what it
# starts next; the shell removes it when it exits.
tilo_test_environment_enter() {
    local root lavapipe test_audio

    # rustup and Cargo locate themselves relative to HOME unless told otherwise.
    # Resolve both before HOME moves; CI images already set them explicitly.
    export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
    export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"

    # A short /tmp path keeps Unix socket paths below the 108-byte limit, also
    # those of sockets tests create in their temporary directories: the root is
    # their TMPDIR too.
    root=$(mktemp -d /tmp/tilo-test-env.XXXXXXXX)
    tilo_test_environment_root=$root
    trap tilo_test_environment_cleanup EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM

    mkdir -p "$root"/{home,config,data,state,cache,runtime}
    chmod 700 "$root" "$root/runtime"
    # The tests run as the test user where the run started as root.
    tilo_test_user_own "$root" "$root"/{home,config,data,state,cache,runtime}

    export TILO_TEST_ENVIRONMENT="$root"
    export HOME="$root/home"
    export XDG_CONFIG_HOME="$root/config"
    export XDG_DATA_HOME="$root/data"
    export XDG_STATE_HOME="$root/state"
    export XDG_CACHE_HOME="$root/cache"
    export XDG_RUNTIME_DIR="$root/runtime"
    export TMPDIR="$root"
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

    tilo_test_find_kernel
    # The GPU is lavapipe, as tools/test-env-setup.sh runtime gives CI: the only
    # Vulkan driver the loader reads, and the adapter wgpu must pick. A run that
    # named a driver itself keeps it. Where lavapipe is missing the loader gets an
    # empty driver list rather than the machine's: tests that need a GPU then skip
    # (or fail where TILO_REQUIRE_GPU=1) instead of drawing on a real one.
    if [ "$tilo_test_kernel" = Linux ] && [ -z "${VK_DRIVER_FILES:-}${VK_ICD_FILENAMES:-}" ]; then
        lavapipe="/usr/share/vulkan/icd.d/lvp_icd.$(uname -m).json"
        export WGPU_BACKEND="${WGPU_BACKEND:-vulkan}"
        if [ -r "$lavapipe" ]; then
            export VK_DRIVER_FILES="$lavapipe"
            export WGPU_ADAPTER_NAME="${WGPU_ADAPTER_NAME:-llvmpipe}"
        else
            export VK_DRIVER_FILES="$root/no-vulkan-driver.json"
        fi
    fi

    # The run's private audio endpoint (tools/test-env-setup.sh audio) is the
    # test environment's own, not a desktop's: keep its address.
    test_audio=
    if [ -n "${TILO_HEADLESS_AUDIO_PID:-}" ] && [ -n "${PULSE_RUNTIME_PATH:-}" ] &&
        [ "${PULSE_SERVER:-}" = "unix:$PULSE_RUNTIME_PATH/native" ]; then
        test_audio=$PULSE_SERVER
    fi
    unset "${tilo_test_session_vars[@]}"
    [ -z "$test_audio" ] || export PULSE_SERVER="$test_audio"
    # Nothing ever listens on these paths. A test that needs a bus brings its own
    # (a p2p socket pair, or a private dbus-daemon) and hands its address to the
    # code under test.
    export DBUS_SESSION_BUS_ADDRESS="unix:path=$root/no-session-bus"
    export DBUS_SYSTEM_BUS_ADDRESS="unix:path=$root/no-system-bus"
    # macOS: tilo-secrets answers "no store" here instead of reaching the login
    # Keychain, and identity secrets, which never fall back there, stay in files.
    if [ "$tilo_test_kernel" = Darwin ]; then
        export TILO_IDENTITY_FILE_SECRETS=1
    fi
}

# Read into another script's shell, this file is the functions above.
if [ "${BASH_SOURCE[0]}" != "$0" ]; then
    return 0
fi

if [ "$#" -eq 0 ]; then
    echo "usage: $0 COMMAND [ARG ...] | --check" >&2
    exit 2
fi
if [ "$1" = --check ]; then
    tilo_test_environment_intact
    exit
fi
if tilo_test_environment_intact; then
    exec "$@"
fi
tilo_test_environment_enter
if "$@"; then status=0; else status=$?; fi
exit "$status"
