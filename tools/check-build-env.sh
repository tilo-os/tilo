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
BINARIES=(capnp bwrap rust-analyzer sccache fc-match gst-inspect-1.0 jq yt-dlp)
FONTS=(
  "Noto Sans Arabic"
  "Noto Sans Hebrew"
  "Noto Sans Devanagari"
  "Noto Sans Bengali"
  "Noto Sans Thai"
  "Noto Sans CJK JP"
  "Noto Color Emoji"
)
GSTREAMER_ELEMENTS=(h264parse openh264dec)

missing=0
command -v pkg-config >/dev/null 2>&1 || { echo "pkg-config not installed — nothing here can be checked" >&2; exit 2; }

for m in "${MODULES[@]}"; do
  if v=$(pkg-config --modversion "$m" 2>/dev/null); then
    printf '  ok      %-20s %s\n' "$m" "$v"
  else
    printf '  MISSING %-20s\n' "$m"; missing=$((missing + 1))
  fi
done

# ci-measure.sh deliberately uses GNU time's stable machine-readable format;
# the shell keyword cannot report peak RSS and is not a compatible fallback.
if [ ! -x /usr/bin/time ]; then
  echo "missing required build tool: /usr/bin/time" >&2
  exit 1
fi
for b in "${BINARIES[@]}"; do
  if p=$(command -v "$b" 2>/dev/null); then
    printf '  ok      %-20s %s\n' "$b" "$p"
  else
    printf '  MISSING %-20s (binary)\n' "$b"; missing=$((missing + 1))
  fi
done

for family in "${FONTS[@]}"; do
  if fc-match -f '%{family}\n' "$family" 2>/dev/null | head -n 1 | grep -Fqi 'Noto'; then
    printf '  ok      font %-15s\n' "$family"
  else
    printf '  MISSING font %-15s\n' "$family"
    missing=$((missing + 1))
  fi
done

for element in "${GSTREAMER_ELEMENTS[@]}"; do
  if gst-inspect-1.0 "$element" >/dev/null 2>&1; then
    printf '  ok      gstreamer %-10s\n' "$element"
  else
    printf '  MISSING gstreamer %-10s\n' "$element"
    missing=$((missing + 1))
  fi
done

echo
if [ "$missing" -eq 0 ]; then
  echo "all ${#MODULES[@]} libraries, ${#BINARIES[@]} binaries, ${#FONTS[@]} fonts, and ${#GSTREAMER_ELEMENTS[@]} media elements present"
else
  echo "$missing missing — see DEVELOPMENT.md for the package that provides each"
fi
exit $(( missing > 0 ))
