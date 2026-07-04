#!/usr/bin/env bash
# End-to-end qOS app install demo — via qos-install.
#
# Requires: distrobox `my-distrobox` (for cargo builds), bubblewrap on host.
#
# What this proves:
#   1. A qOS-native app is a plain ELF linked against `qos-runtime`.
#   2. `qos-pkg pack` produces a content-addressed, signed .qpkg.
#   3. `qos-install install` verifies + extracts + registers in one shot.
#   4. `qos-install list / inspect` show a working registry.
#   5. `qos-install run` execs qos-launcher with the right args, which:
#        * spawns bwrap with seccomp installed via `--seccomp <fd>`,
#        * bind-mounts this launcher into the sandbox at /qos-launcher,
#        * exec()s the __sandbox-exec shim which installs Landlock,
#        * finally execvp()s the entrypoint binary.
#   6. The app's sandbox probes prove Landlock blocks /proc/self/status
#      reads that bwrap alone would allow.
#   7. Services degrade gracefully when broker / fs / identity absent.

set -euo pipefail

# Workspace root — cargo puts the target dir here (workspace target), not
# at crates/packaging/target/. Walk two levels up from crates/packaging/hello/.
INTEG=$(cd "$(dirname "$0")/../../.." && pwd)
DEMO=${DEMO:-/tmp/qos-hello-demo}
export XDG_DATA_HOME=${XDG_DATA_HOME:-/tmp/qos-install-demo-data}

rm -rf "$DEMO" "$XDG_DATA_HOME"
mkdir -p "$DEMO/payload/bin"

export LC_ALL=C
hr() { printf '\n─── %s ───\n' "$*"; }
step() { local start; start=$(date +%s.%N); "$@"; local rc=$?; local end; end=$(date +%s.%N); LC_ALL=C awk -v s="$start" -v e="$end" 'BEGIN{printf "  ✓ %.3fs\n", e - s}'; return $rc; }

hr "1. build qos-hello + tools (release)"
# Skip the build step if the binaries are already present — makes the
# demo idempotent + fast to re-run without needing distrobox from every
# shell (some sub-shells can't launch nested distrobox containers).
missing=0
for b in qos-hello qos-pkg qos-launcher qos-install qos-runtime-bundle; do
  if [ ! -x "$INTEG/target/release/$b" ]; then missing=1; break; fi
done
if [ "$missing" -eq 1 ]; then
  step distrobox enter my-distrobox -- bash -c "cd $INTEG && cargo build -p hello -p pkg-cli -p launcher -p install -p runtime-bundle --release >/dev/null 2>&1"
else
  echo "  ✓ all release binaries already built"
fi
cp "$INTEG/target/release/qos-hello" "$DEMO/payload/bin/qos-hello"
ls -la "$DEMO/payload/bin/qos-hello" | awk '{print "  binary size:", $5, "bytes"}'

hr "1b. install runtime bundle (qos-runtime/2026.07) from host"
# The launcher refuses to mount unsigned runtime bundles by default —
# `qos-install run` doesn't forward --allow-unsigned-runtime, so mint an
# ephemeral signing identity here and sign the bundle. The identity is
# throwaway (nothing else ever signs as this key), matching the same
# ephemeral-key policy `os/Containerfile` already uses at image-build time.
RUNTIME_IDENTITY="$DEMO/runtime-identity.json"
"$INTEG/target/release/qos-runtime-bundle" key generate --out "$RUNTIME_IDENTITY" >/dev/null
"$INTEG/target/release/qos-install" runtime install \
    --version 2026.07 --from-host --force \
    --identity "$RUNTIME_IDENTITY"
"$INTEG/target/release/qos-install" runtime list

hr "2. write manifest"
cat > "$DEMO/manifest.toml" <<'EOF'
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

hr "4. qos-install install"
step "$INTEG/target/release/qos-install" install "$DEMO/qos-hello.qpkg"

hr "5. qos-install list"
"$INTEG/target/release/qos-install" list

hr "6. qos-install inspect"
"$INTEG/target/release/qos-install" inspect dev.qos.hello

hr "7. qos-install run — sandbox + Landlock enforcement + graceful services"
# NOTE: no --minimal-libs. The manifest declares `runtime = "qos-runtime/2026.07"`,
# and step 1b installed that bundle; the launcher mounts it automatically.
"$INTEG/target/release/qos-install" run dev.qos.hello --no-services

hr "8. qos-install remove"
"$INTEG/target/release/qos-install" remove dev.qos.hello
"$INTEG/target/release/qos-install" list

hr "done"
echo "  qpkg:          $(sha256sum "$DEMO/qos-hello.qpkg" | awk '{print $1}')"
echo "  registry file: $XDG_DATA_HOME/qos-apps/registry.toml (removed with the package)"
