#!/usr/bin/env bash
# End-to-end qOS app install demo: build → pack → verify → install → launch.
#
# Requires: distrobox `my-distrobox` (for cargo), bubblewrap installed.
#
# What this proves:
#   1. A qOS-native app is a plain ELF linked against `qos-runtime` (the SDK).
#   2. `qos-pkg pack` produces a content-addressed, signed .qpkg deterministically.
#   3. `qos-pkg verify` accepts the freshly-signed package.
#   4. `qos-pkg unpack` restores an installable payload + manifest.
#   5. `qos-launcher run` sets up bwrap + Landlock + seccomp and execs the entrypoint.
#   6. Services degrade gracefully when no broker / fs / identity are present.

set -euo pipefail

INTEG=$(cd "$(dirname "$0")/.." && pwd)
DEMO=${DEMO:-/tmp/qos-hello-demo}
rm -rf "$DEMO"
mkdir -p "$DEMO/payload/bin"

export LC_ALL=C
hr() { printf '\n─── %s ───\n' "$*"; }
step() { local start; start=$(date +%s.%N); "$@"; local rc=$?; local end; end=$(date +%s.%N); LC_ALL=C awk -v s="$start" -v e="$end" 'BEGIN{printf "  ✓ %.3fs\n", e - s}'; return $rc; }

hr "1. build qos-hello (release)"
step distrobox enter my-distrobox -- bash -c "cd $INTEG && cargo build -p qos-hello -p qos-pkg-cli -p qos-launcher --release >/dev/null 2>&1"
cp "$INTEG/target/release/qos-hello" "$DEMO/payload/bin/qos-hello"
ls -la "$DEMO/payload/bin/qos-hello" | awk '{print "  binary size:", $5, "bytes"}'

hr "2. write manifest"
cat > "$DEMO/manifest.toml" <<'EOF'
package_id = "dev.qos.hello"
version = "0.1.0"

[package]
id       = "dev.qos.hello"
version  = "0.1.0"
kind     = "app"
runtime  = "qos-runtime/2026.07"

[publisher]
identity = "ed25519:0000000000000000000000000000000000000000000000000000000000000000"
signature = ""

[[trust.attested_by]]
attestor = "qos-demo/2026.07"
claim    = "deadbeef"

[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount", "net"]
seccomp    = "qos-default"
landlock   = "strict"

[sandbox.fs]
read  = []
write = []

[sandbox.net]
mode = "off"

[sandbox.portals]

[sandbox.services]
collab     = "optional@1"
qos-fs     = "optional@1"
spellcheck = "optional@1"

[entrypoint]
bin             = "bin/qos-hello"
splash-segments = ["bin/qos-hello[0x0:0x100000]"]
splash-budget   = 100

[providers]
mime = []
ext  = []
EOF
echo "  manifest bytes: $(wc -c < "$DEMO/manifest.toml")"

hr "3. pack (qos-pkg)"
step "$INTEG/target/release/qos-pkg" pack \
  --manifest "$DEMO/manifest.toml" \
  --payload  "$DEMO/payload" \
  --output   "$DEMO/qos-hello.qpkg"
ls -la "$DEMO/qos-hello.qpkg" | awk '{print "  qpkg size:  ", $5, "bytes"}'

hr "4. verify signature"
step "$INTEG/target/release/qos-pkg" verify "$DEMO/qos-hello.qpkg"

hr "5. inspect metadata"
"$INTEG/target/release/qos-pkg" inspect "$DEMO/qos-hello.qpkg" 2>&1 | sed -n '1,6p'

hr "6. unpack (install)"
step "$INTEG/target/release/qos-pkg" unpack "$DEMO/qos-hello.qpkg" --dest "$DEMO/installed"
ls -la "$DEMO/installed/bin/qos-hello" | awk '{print "  installed:", $1, $5, "bytes"}'

hr "7. launch via qos-launcher (bwrap + seccomp + minimal loader libs)"
distrobox enter my-distrobox -- bash -c \
  "$INTEG/target/release/qos-launcher run \
     --manifest    $DEMO/installed/manifest.toml \
     --payload-root $DEMO/installed \
     --minimal-libs \
     --no-services"

hr "done"
echo "  qpkg:              $(sha256sum "$DEMO/qos-hello.qpkg" | awk '{print $1}')"
echo "  payload dir:       $DEMO/installed"
echo "  entry executed in: sandbox mount /var/qos-apps/dev.qos.hello/bin/qos-hello"
