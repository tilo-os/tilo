#!/usr/bin/env bash
# Report which native dependencies are missing.
#
# Rust surfaces an absent system library as `pkg-config exited with status code 1`
# inside a -sys crate without naming it. This asks pkg-config the same questions.
#
# Checks pkg-config modules, not package names: the module is what the build asks for,
# the package providing it differs per distribution (DEVELOPMENT.md maps them).

set -uo pipefail

MODULES=(
  xkbcommon libseat libinput libudev libdrm gbm gl egl
  wayland-client libpipewire-0.3 fuse3 dbus-1
  libavcodec libavformat libavutil
  gstreamer-1.0 gstreamer-app-1.0 openssl
)
# Test subjects rather than build tools, but a missing one fails a test instead
# of naming itself: bwrap runs the launcher's confinement suites and
# rust-analyzer is the real language server the code editor's suite drives;
# sccache is baked into the image so workflows never download or compile it.
BINARIES=(capnp bwrap rust-analyzer sccache)

missing=0
command -v pkg-config >/dev/null 2>&1 || { echo "pkg-config not installed — nothing here can be checked" >&2; exit 2; }

for m in "${MODULES[@]}"; do
  if v=$(pkg-config --modversion "$m" 2>/dev/null); then
    printf '  ok      %-20s %s\n' "$m" "$v"
  else
    printf '  MISSING %-20s\n' "$m"; missing=$((missing + 1))
  fi
done
for b in "${BINARIES[@]}"; do
  if p=$(command -v "$b" 2>/dev/null); then
    printf '  ok      %-20s %s\n' "$b" "$p"
  else
    printf '  MISSING %-20s (binary)\n' "$b"; missing=$((missing + 1))
  fi
done

echo
if [ "$missing" -eq 0 ]; then
  echo "all ${#MODULES[@]} libraries and ${#BINARIES[@]} binaries present"
else
  echo "$missing missing — see DEVELOPMENT.md for the package that provides each"
fi
exit $(( missing > 0 ))
