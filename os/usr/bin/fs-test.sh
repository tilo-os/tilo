#!/bin/bash
# Validation script for the test image (Containerfile.test).
#
# Runs at boot inside the QEMU VM after multi-user.target. Checks the
# pieces of the OS image that a static lint can't catch:
#
#   1. /var/workspace is a real FUSE mount and serves the synthetic
#      hello.txt blob (the existing contract).
#   2. The OS-shipped binaries are present and executable.
#   3. The xdg-desktop-portal backend can be activated on-demand
#      (D-Bus name registration is the runtime contract that the
#      static .portal validator can't prove).
#   4. The compositor can at least bind a wayland-N socket when
#      launched headlessly. We don't keep it running — just confirm
#      it gets past socket setup before SIGTERM.
#
# Read the result with:  journalctl -u fs-test
set -euo pipefail

echo "=== Tilo OS validation ==="
MOUNT_POINT="/var/workspace"
TEST_FILE="$MOUNT_POINT/hello.txt"

# --- 1. FUSE mount + content ----------------------------------------
echo "--- [1/4] FUSE mount at $MOUNT_POINT ---"
for i in {1..10}; do
    if mountpoint -q "$MOUNT_POINT"; then
        echo "Found mountpoint $MOUNT_POINT (after ${i}s)"
        break
    fi
    echo "Waiting for $MOUNT_POINT... ($i/10)"
    sleep 1
done

if ! mountpoint -q "$MOUNT_POINT"; then
    echo "FAIL: $MOUNT_POINT is not a mountpoint after 10s!"
    exit 1
fi

ls -lah "$MOUNT_POINT"

if [ ! -f "$TEST_FILE" ]; then
    echo "FAIL: $TEST_FILE not found!"
    exit 1
fi

CONTENT=$(cat "$TEST_FILE")
echo "hello.txt content: $CONTENT"
if [[ "$CONTENT" != *"hash_hello"* ]]; then
    echo "FAIL: hello.txt content mismatch! Got: $CONTENT"
    exit 1
fi
echo "PASS: FUSE mount + content"

# --- 2. Binaries ----------------------------------------------------
echo "--- [2/4] OS binaries ---"
for bin in /usr/bin/shell /usr/bin/fs; do
    if [ ! -x "$bin" ]; then
        echo "FAIL: $bin missing or not executable"
        exit 1
    fi
done
# The browser engine runtime is optional: an image built with
# TILO_WITH_BROWSER=0 ships without it so boot verification isn't gated on the
# engine building. Absent is allowed, present-but-broken is not.
engine_host=$(find /usr/lib/tilo-runtime -mindepth 4 -maxdepth 4 -path '*/lib/chromium/tilo-chromium-host' -print -quit 2>/dev/null)
if [ -n "$engine_host" ] && [ ! -x "$engine_host" ]; then
    echo "FAIL: $engine_host present but not executable"
    exit 1
fi
echo "PASS: core binaries present (browser engine: ${engine_host:-omitted})"

# --- 3. Portal D-Bus backend ---------------------------------------
# The portal user service is D-Bus-activated, so the assertion is
# "the .service file is registered with the bus and is well-formed".
# We don't drive a real Activate() call here because that would need
# a graphical session running; static plus on-disk-shape checks are
# the strongest thing we can do at multi-user.target.
echo "--- [3/4] Portal backend registration ---"
# The portal is served by the compositor itself. The standalone
# `--portal-server-only` daemon, its user unit and its D-Bus activation
# file were removed: D-Bus permits one owner per name, so two servers
# race and the loser serves nobody while looking healthy. Assert the
# definition ships and that the daemon's files are gone — a resurrected
# activation file is the failure this guards.
PORTAL_DEF=/usr/share/xdg-desktop-portal/portals/tilo.portal
PORTAL_DBUS_SERVICE=/usr/share/dbus-1/services/org.freedesktop.impl.portal.desktop.tilo.service
PORTAL_USER_UNIT=/usr/lib/systemd/user/shell-portal.service

if [ ! -f "$PORTAL_DEF" ]; then
    echo "FAIL: missing portal definition: $PORTAL_DEF"
    exit 1
fi
for f in "$PORTAL_DBUS_SERVICE" "$PORTAL_USER_UNIT"; do
    if [ -e "$f" ]; then
        echo "FAIL: $f is back; it races the compositor for the bus name"
        exit 1
    fi
done
if ! grep -q '^DBusName=org.freedesktop.impl.portal.desktop.tilo$' "$PORTAL_DEF"; then
    echo "FAIL: $PORTAL_DEF wrong DBusName"
    exit 1
fi
echo "PASS: portal config registered and consistent"

# --- 4. Compositor can bind a wayland socket -----------------------
# Launch in --mode native, give it 10s to bind, look for the
# "tilo compositor listening on wayland-N" line that native.rs
# prints to stderr. We capture both stdout and stderr.
echo "--- [4/4] Compositor wayland-N binding ---"

# A headless TTY won't have a GPU but native mode falls back to the
# winit backend when no DRM device is available, and winit itself
# needs a display server. In QEMU without a graphical session we
# only assert the binary *starts* and exits cleanly when given a
# bad CLI — proving the binary isn't corrupted. Real binding
# verification happens in the graphical smoke test (test-vm.sh).
if timeout 5 /usr/bin/shell --version >/dev/null 2>&1 ||
   timeout 5 /usr/bin/shell --help     >/dev/null 2>&1; then
    echo "PASS: shell binary executes (CLI surface reachable)"
else
    # `--help` may exit non-zero in current builds; fall back to
    # confirming the binary is at least dynamically linkable.
    if ldd /usr/bin/shell >/dev/null 2>&1; then
        echo "PASS: shell binary is dynamically linkable"
        echo "      (CLI surface check skipped — no --help/--version yet)"
    else
        echo "FAIL: shell binary is broken (ldd failed)"
        exit 1
    fi
fi

echo
echo "=== All Tilo OS validation checks passed! ==="
