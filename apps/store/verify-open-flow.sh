#!/usr/bin/env bash
# Driven end-to-end proof of the store's qpkg "add, don't install" path.
#
# Stands up a REAL local HTTP registry serving a REAL signed hello.qpkg,
# then drives the store's real click pipeline (CRDT edge → StoreApp::sync
# → pending action → actions::execute) via the `drive_open` example:
#
#   1. Click **Open** on the qOS card → the store downloads the .qpkg on
#      demand and hands it to `qos-install`, which verifies + caches it.
#      No separate "Install" step is ever issued.
#   2. Click **Mark installed** → the store pins it via `qos-install pin`.
#
# Assertions are on the on-disk `qos-install` registry (the production
# state), not on the driver's own return value.
#
# Requires: distrobox `my-distrobox` (cargo), python3 (static file server).
set -euo pipefail

INTEG=$(cd "$(dirname "$0")/../.." && pwd)
WORK=${WORK:-/tmp/qos-store-openflow}
export QOS_APPS_DATA_DIR="$WORK/apps-data"
export QOS_INSTALL_BIN="$INTEG/target/debug/qos-install"

rm -rf "$WORK"
mkdir -p "$WORK/serve" "$WORK/payload/bin" "$QOS_APPS_DATA_DIR"

hr() { printf '\n─── %s ───\n' "$*"; }

hr "1. build store example + hello + pkg-cli + install (debug)"
# The cd is what selects the target directory; naming it as CARGO_TARGET_DIR
# would only cost cache hits, since sccache hashes that variable into its Rust
# key. The cargo shim on PATH selects the container's sccache server.
distrobox enter my-distrobox -- bash -c "cd '$INTEG' && \
  cargo build -p store --example drive_open -p hello -p pkg-cli -p install 2>&1 | tail -3"

cp "$INTEG/target/debug/qos-hello" "$WORK/payload/bin/qos-hello"
BIN_SIZE=$(stat -c%s "$WORK/payload/bin/qos-hello")
echo "  qos-hello payload: $BIN_SIZE bytes"

hr "2. write manifest + pack a real hello.qpkg"
cat > "$WORK/manifest.toml" <<'EOF'
[package]
id      = "dev.qos.hello"
version = "0.1.0"
kind    = "app"

[publisher]
identity  = "ed25519:0000000000000000000000000000000000000000000000000000000000000000"
signature = ""

[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount"]
seccomp    = "qos-default"
landlock   = "strict"

[sandbox.net]
mode = "off"

[sandbox.portals]

[sandbox.services]

[entrypoint]
bin = "bin/qos-hello"

[providers]
mime = []
ext  = []
EOF

"$INTEG/target/debug/qos-pkg" pack \
  --manifest "$WORK/manifest.toml" \
  --payload  "$WORK/payload" \
  --output   "$WORK/serve/hello.qpkg"
QPKG_SIZE=$(stat -c%s "$WORK/serve/hello.qpkg")
echo "  hello.qpkg: $QPKG_SIZE bytes"

hr "3. serve the qpkg over HTTP"
PORT=8917
( cd "$WORK/serve" && python3 -m http.server "$PORT" >/dev/null 2>&1 & echo $! > "$WORK/server.pid" )
SERVER_PID=$(cat "$WORK/server.pid")
trap 'kill "$SERVER_PID" 2>/dev/null || true' EXIT
sleep 1
URL="http://127.0.0.1:$PORT/hello.qpkg"
echo "  serving $URL (pid $SERVER_PID)"

hr "4. DRIVE: click Open on the qOS card"
export QOS_STORE_ITEM_ID="dev.qos.hello"
export QOS_STORE_ITEM_NAME="Hello"
export QOS_STORE_ITEM_URL="$URL"
set +e
"$INTEG/target/debug/examples/drive_open" open
OPEN_RC=$?
set -e
echo "  drive_open(open) exit: $OPEN_RC (launch may fail if this host has no bwrap/user-ns; fetch+cache is asserted below)"

hr "5. ASSERT the app was fetched + cached (installed) by qos-install"
"$QOS_INSTALL_BIN" list
if "$QOS_INSTALL_BIN" list | grep -q "dev.qos.hello"; then
  echo "  ✓ dev.qos.hello is present in the qos-install registry — Open fetched + cached it"
else
  echo "  ✗ dev.qos.hello NOT installed — Open pipeline failed" >&2
  exit 1
fi

hr "6. DRIVE: click Mark installed (pin)"
unset QOS_STORE_ITEM_URL   # pin needs no download
"$INTEG/target/debug/examples/drive_open" mark-installed || true

hr "7. ASSERT it is now pinned"
grep -A8 'dev.qos.hello' "$QOS_APPS_DATA_DIR/registry.toml" | sed 's/^/    /'
if grep -q 'pinned = true' "$QOS_APPS_DATA_DIR/registry.toml"; then
  echo "  ✓ marked installed — pinned = true in the registry"
else
  echo "  ✗ pin did not take" >&2
  exit 1
fi

hr "done — Open fetched+cached, Mark installed pinned. No Install step was issued."
