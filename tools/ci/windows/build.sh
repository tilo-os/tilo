#!/usr/bin/env bash
# Cross-compile the imager for Windows on Linux with cargo-xwin (clang-cl and
# lld-link against the MSVC CRT and Windows SDK), the same target and features
# a native Windows build has (apps/imager/packaging/package-host.ps1).
#
#   build.sh sdk <arch> <out>     download the pinned CRT and SDK into <out>/sdk-<arch>.tar.zst
#   build.sh imager <arch> <sdk> <out>   the release imager.exe
#   build.sh tests <arch> <sdk> <out>    the imager's unit-test executable, for Wine or a Windows host
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
    ;;
  *) printf 'unknown mode: %s\n' "$mode" >&2; exit 2 ;;
esac
