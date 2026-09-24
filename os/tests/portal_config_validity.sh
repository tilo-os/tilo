#!/usr/bin/env bash
# Validate the xdg-desktop-portal config files. The checks cover:
#
#   1. The .portal file declares the right backend bus name and lists
#      every interface our `tilo_desktop::portal::serve()` registers.
#   2. The tilo desktop preference selects that backend.
#   3. No obsolete D-Bus activation or standalone portal unit remains;
#      the live compositor is the sole backend owner.
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
#      script bundle landed at /usr/libexec/tilo/, so the
#      relative ../../ walk would yank us to / and miss the files).
#      Set OS_ROOT=/ in that case (the Containerfile.test build step
#      does so) and the assertions resolve at /usr/...
TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OS_ROOT="${OS_ROOT:-$(cd "$TEST_DIR/.." && pwd)}"

PORTAL_FILE="$OS_ROOT/usr/share/xdg-desktop-portal/portals/tilo.portal"
PORTAL_PREFERENCE_FILE="$OS_ROOT/usr/share/xdg-desktop-portal/tilo-portals.conf"
DBUS_SERVICE_FILE="$OS_ROOT/usr/share/dbus-1/services/org.freedesktop.impl.portal.desktop.tilo.service"
SYSTEMD_UNIT_FILE="$OS_ROOT/usr/lib/systemd/user/shell-portal.service"
CONTAINERFILE="$OS_ROOT/Containerfile"

EXPECTED_BUS_NAME="org.freedesktop.impl.portal.desktop.tilo"
# Interfaces the Rust backend registers in `tilo_desktop::portal::serve`.
# Keeping the list here mirrors `portal/mod.rs::serve` so a drift
# (e.g. we add an interface to the Rust side without updating the
# .portal config) shows up as a CI failure.
EXPECTED_INTERFACES=(
    "org.freedesktop.impl.portal.Access"
    "org.freedesktop.impl.portal.Account"
    "org.freedesktop.impl.portal.AppChooser"
    "org.freedesktop.impl.portal.Background"
    "org.freedesktop.impl.portal.Clipboard"
    "org.freedesktop.impl.portal.DynamicLauncher"
    "org.freedesktop.impl.portal.Email"
    "org.freedesktop.impl.portal.FileChooser"
    "org.freedesktop.impl.portal.GlobalShortcuts"
    "org.freedesktop.impl.portal.Inhibit"
    "org.freedesktop.impl.portal.InputCapture"
    "org.freedesktop.impl.portal.Lockdown"
    "org.freedesktop.impl.portal.Notification"
    "org.freedesktop.impl.portal.PermissionStore"
    "org.freedesktop.impl.portal.Print"
    "org.freedesktop.impl.portal.RemoteDesktop"
    "org.freedesktop.impl.portal.ScreenCast"
    "org.freedesktop.impl.portal.Screenshot"
    "org.freedesktop.impl.portal.Secret"
    "org.freedesktop.impl.portal.Settings"
    "org.freedesktop.impl.portal.Usb"
    "org.freedesktop.impl.portal.Wallpaper"
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
    interface_value="$(sed -n 's/^Interfaces=//p' "$PORTAL_FILE")"
    IFS=';' read -r -a declared_interfaces <<< "$interface_value"
    if (( ${#declared_interfaces[@]} != ${#EXPECTED_INTERFACES[@]} )); then
        echo "FAIL: $PORTAL_FILE: interface count does not match the Rust backend" >&2
        fail=1
    fi
fi

# --- desktop preference ---------------------------------------------
if assert_file_exists "$PORTAL_PREFERENCE_FILE"; then
    assert_contains "$PORTAL_PREFERENCE_FILE" '^\[preferred\]$'
    assert_contains "$PORTAL_PREFERENCE_FILE" '^default=tilo$'
fi

# The source tree contains the preference file even when the production image
# does not. Assert its exact rootfs mapping whenever this check runs from the
# source layout; Containerfile.test independently runs this script against the
# assembled rootfs with OS_ROOT=/.
if [[ -f "$CONTAINERFILE" ]]; then
    # shellcheck source=lib/containerfile.sh
    . "$OS_ROOT/tests/lib/containerfile.sh"
    if ! containerfile_has_copy "$CONTAINERFILE" \
        'os/usr/share/xdg-desktop-portal/portals/tilo.portal' \
        '/usr/share/xdg-desktop-portal/portals/tilo.portal'; then
        echo "FAIL: $CONTAINERFILE does not install the Tilo portal definition" >&2
        fail=1
    fi
    if ! containerfile_has_copy "$CONTAINERFILE" \
        'os/usr/share/xdg-desktop-portal/tilo-portals.conf' \
        '/usr/share/xdg-desktop-portal/tilo-portals.conf'; then
        echo "FAIL: $CONTAINERFILE does not install the Tilo portal preference" >&2
        fail=1
    fi
fi

# --- no second claimant for the bus name -----------------------------
#
# The compositor owns $EXPECTED_BUS_NAME in-process
# (`compositor::native::start_portal_backend`). D-Bus permits exactly one
# owner per name, so anything else on the image that claims it is not a
# redundant backup — it is a coin flip over which of the two answers the
# session's file dialogs, and the loser looks healthy while serving
# nobody.
#
# These two files used to start `shell --portal-server-only`, whose
# portal context was built on an empty WorkspaceTree. Their absence is
# the invariant now, so re-adding either one fails here rather than in a
# user's session.
assert_file_absent() {
    local path="$1"
    local why="$2"
    if [[ -e "$path" ]]; then
        echo "FAIL: $path must not exist: $why" >&2
        fail=1
    fi
}

assert_file_absent "$DBUS_SERVICE_FILE" \
    "D-Bus activation would start a second owner of $EXPECTED_BUS_NAME"
assert_file_absent "$SYSTEMD_UNIT_FILE" \
    "the unit would start a second owner of $EXPECTED_BUS_NAME"

# The image must also not enable such a unit by name anywhere.
if [[ -d "$OS_ROOT/usr/lib/systemd" ]]; then
    if grep -rqF "shell-portal.service" "$OS_ROOT/usr/lib/systemd" 2>/dev/null; then
        echo "FAIL: shell-portal.service is still referenced under /usr/lib/systemd" >&2
        fail=1
    fi
fi

if (( fail == 0 )); then
    echo "OK: portal config validity ($(( ${#EXPECTED_INTERFACES[@]} + 10 )) checks)"
    exit 0
else
    exit 1
fi
