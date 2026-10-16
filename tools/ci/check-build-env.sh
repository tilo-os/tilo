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
  gstreamer-1.0 gstreamer-app-1.0 gstreamer-video-1.0 gstreamer-allocators-1.0
)
# Integration tests require bwrap for launcher confinement, rust-analyzer for
# the code editor's language-server checks, and a software TPM plus LUKS tools
# for the rollback mechanism suite. The image also supplies sccache so workflows
# do not download or compile it. The NTFS helper bootstraps its pinned reader
# with autotools. The X11 clipboard tests drive a real xwayland-satellite and
# Xwayland through the compositor's per-workspace bridge.
BINARIES=(
  autoreconf libtoolize bwrap rust-analyzer sccache fc-match gst-inspect-1.0 jq rg yt-dlp
  cryptsetup swtpm swtpm_setup tpm2_getcap xwayland-satellite Xwayland
)
FONTS=(
  "Noto Sans Arabic"
  "Noto Sans Hebrew"
  "Noto Sans Devanagari"
  "Noto Sans Bengali"
  "Noto Sans Thai"
  "Noto Sans CJK JP"
  "Noto Color Emoji"
)
# The NTFS helper's configure needs libgcrypt's m4 macro even with crypto off.
ACLOCAL_MACROS=(libgcrypt.m4)
# The real-time media tests round-trip through the royalty-free software
# codecs, which must be present without a GPU or Cisco's openh264 binary.
GSTREAMER_ELEMENTS=(h264parse vp9enc vp9dec av1enc dav1ddec)

missing=0
command -v pkg-config >/dev/null 2>&1 || { echo "pkg-config not installed — nothing here can be checked" >&2; exit 2; }

for m in "${MODULES[@]}"; do
  if v=$(pkg-config --modversion "$m" 2>/dev/null); then
    printf '  ok      %-20s %s\n' "$m" "$v"
  else
    printf '  MISSING %-20s\n' "$m"; missing=$((missing + 1))
  fi
done

# PipeWire bindings load libclang at build time. A clang executable alone
# does not prove the shared library is installed and loadable by bindgen.
if p=$(python3 -c 'import ctypes, ctypes.util; path = ctypes.util.find_library("clang"); assert path; ctypes.CDLL(path); print(path)' 2>/dev/null); then
  printf '  ok      %-20s %s\n' libclang "$p"
else
  printf '  MISSING %-20s (shared library; clang-libs on Fedora)\n' libclang
  missing=$((missing + 1))
fi

# tools/ci/measure.sh deliberately uses GNU time's stable machine-readable format;
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

aclocal_dir=$(aclocal --print-ac-dir 2>/dev/null || echo /usr/share/aclocal)
for m in "${ACLOCAL_MACROS[@]}"; do
  if [ -f "$aclocal_dir/$m" ]; then
    printf '  ok      %-20s %s\n' "$m" "$aclocal_dir/$m"
  else
    printf '  MISSING %-20s (autoconf macro)\n' "$m"; missing=$((missing + 1))
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
  echo "all ${#MODULES[@]} pkg-config libraries, libclang, ${#BINARIES[@]} binaries, ${#FONTS[@]} fonts, and ${#GSTREAMER_ELEMENTS[@]} media elements present"
else
  echo "$missing missing — see DEVELOPMENT.md for the package that provides each"
fi
exit $(( missing > 0 ))
