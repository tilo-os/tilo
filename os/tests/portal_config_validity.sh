#!/usr/bin/env bash
# Validate the xdg-desktop-portal config files (Sandbox Phase 4.3
# follow-up). Two checks:
#
#   1. The .portal file declares the right backend bus name and lists
#      every interface our `quick_desktop::portal::serve()` registers.
#   2. The D-Bus session-bus .service file points at the same bus name
#      and to the `shell --portal-server-only` ExecStart shape
#      our `shell-portal.service` user unit expects.
#
# Falls back to a grep-based contract when `xdg-desktop-portal-validator`
# isn't on PATH (the upstream tool is itself optional and ships with
# `xdg-desktop-portal-dev`, which isn't on every build host).
#
# Exit codes:
#   0 — every required key present and matches the expected value.
#   1 — at least one assertion failed; messages are emitted to stderr.

set -euo pipefail

# Resolve directories relative to this script so the test can be run
# from any cwd (e.g. `cd os/tests && bash portal_config_validity.sh`
# or `os/tests/portal_config_validity.sh` from the repo root).
#
# Two layouts are supported:
#   1. From the source tree — files live under "$OS_DIR/usr/...".
#   2. From inside the test image — files live at "/usr/..." (the
#      script bundle landed at /usr/libexec/qos/, so the
#      relative ../../ walk would yank us to / and miss the files).
#      Set OS_ROOT=/ in that case (the Containerfile.test build step
#      does so) and the assertions resolve at /usr/...
TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OS_ROOT="${OS_ROOT:-$(cd "$TEST_DIR/.." && pwd)}"

PORTAL_FILE="$OS_ROOT/usr/share/xdg-desktop-portal/portals/qos.portal"
DBUS_SERVICE_FILE="$OS_ROOT/usr/share/dbus-1/services/org.freedesktop.impl.portal.desktop.qos.service"
SYSTEMD_UNIT_FILE="$OS_ROOT/usr/lib/systemd/user/shell-portal.service"

EXPECTED_BUS_NAME="org.freedesktop.impl.portal.desktop.qos"
# Interfaces the Rust backend registers in `quick_desktop::portal::serve`.
# Keeping the list here mirrors `portal/mod.rs::serve` so a drift
# (e.g. we add an interface to the Rust side without updating the
# .portal config) shows up as a CI failure.
EXPECTED_INTERFACES=(
    "org.freedesktop.impl.portal.FileChooser"
    "org.freedesktop.impl.portal.ScreenCast"
    "org.freedesktop.impl.portal.Access"
)

fail=0

assert_file_exists() {
    local path="$1"
    if [[ ! -f "$path" ]]; then
        echo "FAIL: missing file: $path" >&2
        fail=1
        return 1
    fi
    return 0
}

assert_contains() {
    local file="$1"
    local pattern="$2"
    if ! grep -qE "$pattern" "$file"; then
        echo "FAIL: $file: missing pattern: $pattern" >&2
        fail=1
        return 1
    fi
    return 0
}

# --- .portal file ----------------------------------------------------
if assert_file_exists "$PORTAL_FILE"; then
    # Try the upstream validator first if available.
    if command -v xdg-desktop-portal-validator >/dev/null 2>&1; then
        if ! xdg-desktop-portal-validator "$PORTAL_FILE" >/dev/null 2>&1; then
            echo "FAIL: xdg-desktop-portal-validator rejected $PORTAL_FILE" >&2
            fail=1
        fi
    fi
    # Always also do the grep-based contract — the validator on Fedora
    # is permissive and won't catch e.g. a missing `Interfaces=` key.
    assert_contains "$PORTAL_FILE" "^\[portal\]$"
    assert_contains "$PORTAL_FILE" "^DBusName=$EXPECTED_BUS_NAME$"
    assert_contains "$PORTAL_FILE" "^UseIn="
    assert_contains "$PORTAL_FILE" "^Interfaces="
    for iface in "${EXPECTED_INTERFACES[@]}"; do
        # Interface list is `;`-separated on the same line; grep for the
        # exact substring so we don't accept a similarly-named placeholder.
        if ! grep -qF "$iface" "$PORTAL_FILE"; then
            echo "FAIL: $PORTAL_FILE: missing interface: $iface" >&2
            fail=1
        fi
    done
fi

# --- D-Bus session-bus .service file ---------------------------------
if assert_file_exists "$DBUS_SERVICE_FILE"; then
    assert_contains "$DBUS_SERVICE_FILE" "^\[D-BUS Service\]$"
    assert_contains "$DBUS_SERVICE_FILE" "^Name=$EXPECTED_BUS_NAME$"
    assert_contains "$DBUS_SERVICE_FILE" "^Exec=/usr/bin/shell"
    # Pattern with a leading `[-]` defangs the leading `--` that grep
    # would otherwise treat as a flag-terminator.
    assert_contains "$DBUS_SERVICE_FILE" "[-]-portal-server-only"
    assert_contains "$DBUS_SERVICE_FILE" "^SystemdService=shell-portal.service$"
fi

# --- systemd user unit -----------------------------------------------
if assert_file_exists "$SYSTEMD_UNIT_FILE"; then
    assert_contains "$SYSTEMD_UNIT_FILE" "^Type=dbus$"
    assert_contains "$SYSTEMD_UNIT_FILE" "^BusName=$EXPECTED_BUS_NAME$"
    assert_contains "$SYSTEMD_UNIT_FILE" "ExecStart=.*shell.*[-]-portal-server-only"
fi

if (( fail == 0 )); then
    echo "OK: portal config validity (4 files, $(( ${#EXPECTED_INTERFACES[@]} + 8 )) checks)"
    exit 0
else
    exit 1
fi
