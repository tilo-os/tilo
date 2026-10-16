#!/usr/bin/env bash
# End-to-end Tilo app install demo — via tilo-install.
#
# Requires: distrobox `my-distrobox` (for cargo builds), bubblewrap on host.
#
# What this proves:
#   1. A Tilo-native app is a plain ELF linked against `tilo-runtime`.
#   2. `tilo-app-package pack` produces a content-addressed, signed .tapp.
#   3. `tilo-install install` verifies + extracts + registers in one shot.
#   4. `tilo-install list / inspect` show a working registry.
#   5. `tilo-install run` execs tilo-launcher with the right args, which:
#        * spawns bwrap with seccomp installed via `--seccomp <fd>`,
#        * bind-mounts this launcher into the sandbox at /tilo-launcher,
#        * exec()s the __sandbox-exec shim which installs Landlock,
#        * finally execvp()s the entrypoint binary.
#   6. The app's sandbox probes prove Landlock blocks /proc/self/status
#      reads that bwrap alone would allow.
#   7. Services degrade gracefully when broker / fs / identity absent.

set -euo pipefail

INTEG=$(cd "$(dirname "$0")/../../.." && pwd)
DEMO=${DEMO:-/tmp/tilo-hello-demo}
export XDG_DATA_HOME=${XDG_DATA_HOME:-/tmp/tilo-install-demo-data}

rm -rf "$DEMO" "$XDG_DATA_HOME"
mkdir -p "$DEMO/payload/bin"

export LC_ALL=C
hr() { printf '\n─── %s ───\n' "$*"; }
step() { local start; start=$(date +%s.%N); "$@"; local rc=$?; local end; end=$(date +%s.%N); LC_ALL=C awk -v s="$start" -v e="$end" 'BEGIN{printf "  ✓ %.3fs\n", e - s}'; return $rc; }

hr "1. build tilo-hello + tools (release)"
# Skip the build step if the binaries are already present — makes the
# demo idempotent + fast to re-run without needing distrobox from every
# shell (some sub-shells can't launch nested distrobox containers).
missing=0
for b in tilo-hello tilo-app-package tilo-launcher tilo-install tilo-runtime-bundle; do
  if [ ! -x "$INTEG/target/release/$b" ]; then missing=1; break; fi
done
if [ "$missing" -eq 1 ]; then
  step distrobox enter my-distrobox -- bash -c "cd $INTEG && cargo build -p hello -p tilo-app-package-cli -p launcher -p install -p runtime-bundle --release >/dev/null 2>&1"
else
  echo "  ✓ all release binaries already built"
fi
cp "$INTEG/target/release/tilo-hello" "$DEMO/payload/bin/tilo-hello"
ls -la "$DEMO/payload/bin/tilo-hello" | awk '{print "  binary size:", $5, "bytes"}'

hr "1b. install runtime bundle (tilo-runtime/2026.07) from host"
"$INTEG/target/release/tilo-install" runtime install --version 2026.07 --from-host --force
"$INTEG/target/release/tilo-install" runtime list

hr "2. write manifest"
cat > "$DEMO/manifest.toml" <<'EOF'
[package]
id       = "so.tilo.hello"
version  = "0.1.0"
kind     = "app"
runtime  = "tilo-runtime/2026.07"

[publisher]
identity = "ed25519:0000000000000000000000000000000000000000000000000000000000000000"
signature = ""

[[trust.attested_by]]
attestor = "tilo-demo/2026.07"
claim    = "deadbeef"

[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount", "net"]
seccomp    = "tilo-default"
landlock   = "strict"

[sandbox.net]
mode = "off"

[sandbox.portals]

[sandbox.services]
collab     = "optional@1"
tilo-fs     = "optional@1"

[entrypoint]
bin             = "bin/tilo-hello"
splash-segments = ["bin/tilo-hello[0x0:0x100000]"]
splash-budget   = 100

[providers]
mime = []
ext  = []
EOF
echo "  manifest bytes: $(wc -c < "$DEMO/manifest.toml")"

hr "3. pack (tilo-app-package)"
step "$INTEG/target/release/tilo-app-package" pack \
  --manifest "$DEMO/manifest.toml" \
  --payload  "$DEMO/payload" \
  --output   "$DEMO/tilo-hello.tapp"
ls -la "$DEMO/tilo-hello.tapp" | awk '{print "  tapp size:  ", $5, "bytes"}'

hr "4. tilo-install install"
step "$INTEG/target/release/tilo-install" install "$DEMO/tilo-hello.tapp"

hr "5. tilo-install list"
"$INTEG/target/release/tilo-install" list

hr "6. tilo-install inspect"
"$INTEG/target/release/tilo-install" inspect so.tilo.hello

hr "7. tilo-install run — sandbox + Landlock enforcement + graceful services"
# NOTE: no --minimal-libs. The manifest declares `runtime = "tilo-runtime/2026.07"`,
# and step 1b installed that bundle; the launcher mounts it automatically.
"$INTEG/target/release/tilo-install" run so.tilo.hello --no-services

hr "8. tilo-install remove"
"$INTEG/target/release/tilo-install" remove so.tilo.hello
"$INTEG/target/release/tilo-install" list

hr "done"
echo "  tapp:          $(sha256sum "$DEMO/tilo-hello.tapp" | awk '{print $1}')"
echo "  registry file: $XDG_DATA_HOME/tilo-apps/registry.toml (removed with the package)"
