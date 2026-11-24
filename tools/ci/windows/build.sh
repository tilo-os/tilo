#!/usr/bin/env bash
# Cross-compile the imager for Windows on Linux with cargo-xwin (clang-cl and
# lld-link against the MSVC CRT and Windows SDK), the same target and features
# a native Windows build has (apps/imager/packaging/package-host.ps1).
#
#   build.sh sdk <arch> <out>     download the pinned CRT and SDK into <out>/sdk-<arch>.tar.zst
#   build.sh imager <arch> <sdk> <out>   the release imager.exe
#   build.sh tests <arch> <sdk> <out>    the imager's unit-test executable, for Wine or a Windows host
#   build.sh runtime <arch> <sdk> <out>  the device daemon and the tilolang runtime programs
#
# On x86_64 the runtime links GStreamer's MSVC build, through the import libraries
# build/windows-gstreamer-x86_64 staged (gstreamer_runtime.py).
#
# <sdk> is the archive `build.sh sdk` made. Every output has a SHA-256 sidecar.
set -euo pipefail

here="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
. "$here/xwin-env.sh"
mode="${1:?sdk, imager or tests}"
arch="${2:?x86_64 or aarch64}"
target="$(windows_target "$arch")"

digest() {
  (cd "$(dirname -- "$1")" && sha256sum -- "$(basename -- "$1")" > "$(basename -- "$1").sha256")
}

# ring compiles its Windows ARM64 sources with plain `clang` whatever cc-rs
# is told, but with the target's CFLAGS, which cargo-xwin writes for clang-cl
# (`/imsvc <dir>` for the CRT and SDK headers). A `clang` ahead on PATH turns
# those into the GNU driver's `-isystem`, so ring sees the same headers.
if [ "$arch" = aarch64 ]; then
  wrappers="${TMPDIR:-/tmp}/xwin-clang-$arch"
  mkdir -p "$wrappers"
  real_clang="$(command -v clang)"
  cat > "$wrappers/clang" <<SH
#!/usr/bin/env bash
args=()
while [ "\$#" -gt 0 ]; do
  case "\$1" in
    /imsvc) args+=(-isystem "\$2"); shift 2 ;;
    /imsvc*) args+=(-isystem "\${1#/imsvc}"); shift ;;
    /EHsc) shift ;;
    *) args+=("\$1"); shift ;;
  esac
done
exec "$real_clang" "\${args[@]}"
SH
  chmod 0755 "$wrappers/clang"
  export PATH="$wrappers:$PATH"
fi

case "$mode" in
  sdk)
    out="${3:?output directory}"
    mkdir -p "$out"
    export XWIN_CACHE_DIR="${TMPDIR:-/tmp}/xwin-download-$arch" XWIN_ARCH="$arch"
    rm -rf -- "$XWIN_CACHE_DIR"
    # `cargo xwin env` sets up the toolchain (downloading the CRT and SDK
    # into XWIN_CACHE_DIR) and prints the environment it would build with.
    cargo xwin env --target "$target" >/dev/null
    test -f "$XWIN_CACHE_DIR/xwin/DONE"
    tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner -C "$XWIN_CACHE_DIR" -cf - . \
      | zstd -T4 -10 --quiet -o "$out/sdk-$arch.tar.zst" -f
    digest "$out/sdk-$arch.tar.zst"
    du -sh "$XWIN_CACHE_DIR" "$out/sdk-$arch.tar.zst"
    rm -rf -- "$XWIN_CACHE_DIR"
    ;;
  imager)
    sdk="${3:?SDK archive}" out="${4:?output directory}"
    use_windows_sdk "$arch" "$sdk"
    cargo xwin build --locked --release -p imager --bin imager --features host-media-writer --target "$target"
    mkdir -p "$out"
    install -m 0644 "${CARGO_TARGET_DIR:-target}/$target/release/imager.exe" "$out/imager.exe"
    digest "$out/imager.exe"
    ;;
  tests)
    sdk="${3:?SDK archive}" out="${4:?output directory}"
    use_windows_sdk "$arch" "$sdk"
    # The test executable cargo would run: the library's unit tests with the
    # packaged host writer enabled, as the Windows release job runs them.
    cargo xwin test --locked --no-run -p imager --lib --features host-media-writer --target "$target" \
      --message-format json-render-diagnostics > "${TMPDIR:-/tmp}/imager-tests.json"
    executable="$(python3 -I -c '
import json, sys
paths = [m["executable"] for m in map(json.loads, open(sys.argv[1]))
         if m.get("reason") == "compiler-artifact" and m.get("executable")
         and m["target"]["name"] == "imager" and m["profile"]["test"]]
if len(paths) != 1:
    sys.exit(f"expected one imager test executable, found {paths}")
print(paths[0])' "${TMPDIR:-/tmp}/imager-tests.json")"
    mkdir -p "$out"
    install -m 0644 "$executable" "$out/imager-tests.exe"
    digest "$out/imager-tests.exe"
    # The fonts the tests set text in: the build publishes them in the profile directory's
    # font-catalogs, which a test program finds above itself, so they go beside the copy.
    profile="$(dirname -- "$(dirname -- "$executable")")"
    [ -d "$profile/font-catalogs" ] || { printf 'the build published no font catalogs in %s\n' "$profile" >&2; exit 1; }
    rm -rf "$out/font-catalogs"
    cp -a "$profile/font-catalogs" "$out/font-catalogs"
    ;;
  runtime)
    sdk="${3:?SDK archive}" out="${4:?output directory}"
    windows_platform_tests="tilo-socket tilo-shm tilo-render-protocol tilo-runtime-portal launcher-windows tilo-fs-client tilo-identity"
    use_windows_sdk "$arch" "$sdk"
    if [ "$arch" = x86_64 ]; then
      gstreamer="${TMPDIR:-/tmp}/gstreamer-sdk-$arch"
      archive=/target/windows/x86_64/gstreamer/gstreamer-sdk-x86_64.tar.zst
      (cd "$(dirname -- "$archive")" && sha256sum --check --strict --quiet "$(basename -- "$archive").sha256")
      rm -rf -- "$gstreamer" && mkdir -p "$gstreamer"
      zstd -dc --quiet "$archive" | tar -xf - -C "$gstreamer"
      # For the Windows target alone: the host's build scripts keep the host's pkg-config.
      export PKG_CONFIG_ALLOW_CROSS_x86_64_pc_windows_msvc=1 \
        PKG_CONFIG_LIBDIR_x86_64_pc_windows_msvc="$gstreamer/lib/pkgconfig" \
        PKG_CONFIG_PATH_x86_64_pc_windows_msvc=
    fi
    # The unit tests of the platform layer the daemon stands on, for Wine to run:
    # one executable per crate, named by it. First, so a layer that stops compiling
    # is named even while something above it does too.
    for crate in $windows_platform_tests; do
      cargo xwin test --locked --no-run -p "$crate" --all-features --lib --target "$target" \
        --message-format json-render-diagnostics > "${TMPDIR:-/tmp}/$crate-tests.json"
      executable="$(python3 -I -c '
import json, sys
paths = [m["executable"] for m in map(json.loads, open(sys.argv[1]))
         if m.get("reason") == "compiler-artifact" and m.get("executable") and m["profile"]["test"]]
if len(paths) != 1:
    sys.exit(f"expected one test executable, found {paths}")
print(paths[0])' "${TMPDIR:-/tmp}/$crate-tests.json")"
      mkdir -p "$out/platform-tests"
      install -m 0644 "$executable" "$out/platform-tests/$crate.exe"
    done
    # The daemon and the tilolang runtime programs of a Tilo desktop: cross-built
    # so that a platform arm which stops compiling is a failed check, and so the
    # x86_64 daemon can be run under Wine to prove it refuses legibly. The
    # daemon's Windows port is partial: it compiles, and its CLI answers the
    # refusal until its services have Windows bodies.
    cargo xwin build --locked -p tilo-daemon -p tilolang-process --target "$target"
    install -m 0644 "${CARGO_TARGET_DIR:-target}/$target/debug/tilo-runtime.exe" "$out/tilo-runtime.exe"
    digest "$out/tilo-runtime.exe"
    ;;
  *) printf 'unknown mode: %s\n' "$mode" >&2; exit 2 ;;
esac
