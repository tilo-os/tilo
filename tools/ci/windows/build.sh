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

# The tilolang runtime package a Windows daemon carries beside it and installs on its first start
# (tilo-runtime's apps/carried.rs), as Tilo.app carries a Mac's: tilolang-process.exe with the
# reviewed GStreamer closure beside it (its DLLs in bin/, its plugins in lib/gstreamer-1.0, the
# plugin scanner and the notices), sealed, signed and packed as the registry package
# so.tilo.runtime.tilolang for Windows. The key that signs it is made for the build unless
# TILO_RUNTIME_PACKAGE_IDENTITY names one; its public key goes beside the package.
runtime_package() {
  local binary="$1" runtimes="$2" work closure tools identity version root
  work="$(mktemp -d "${TMPDIR:-/tmp}/tilolang-runtime.XXXXXX")"
  closure="$work/closure"
  mkdir -p "$closure"
  archive=/target/windows/x86_64/gstreamer/gstreamer-runtime-x86_64.tar.zst
  (cd "$(dirname -- "$archive")" && sha256sum --check --strict --quiet "$(basename -- "$archive").sha256")
  zstd -dc --quiet "$archive" | tar -xf - -C "$closure"
  # The tools that seal and pack it, and compile the standard library it carries, for this
  # machine: the library is wasm, the same for every device.
  cargo build --locked --release -p tilo-runtime-bundle --bin tilo-runtime-bundle \
    -p tilo-app-package-cli --bin tilo-app-package -p tilolang --bin tilolang-std
  tools="${CARGO_TARGET_DIR:-target}/release"
  identity="${TILO_RUNTIME_PACKAGE_IDENTITY:-$work/runtime-identity.json}"
  [ -f "$identity" ] || "$tools/tilo-runtime-bundle" key generate --out "$identity"
  # shellcheck source=/dev/null
  . tools/apps/tilolang_packages.sh
  version="$(tilolang_runtime_version "$PWD")"
  local digest
  digest="$("$tools/tilolang-std" crates/lang/tilolang/std --out "$work/tilo.std.wasm")"
  [ "${version##*+tilolang.}" = "$digest" ] \
    || { printf 'the runtime %s is keyed by another standard library (%s)\n' "$version" "$digest" >&2; exit 1; }
  local binaries=(--include-bin "$binary")
  for dll in "$closure"/bin/*.dll; do binaries+=(--include-bin "$dll"); done
  "$tools/tilo-runtime-bundle" build --binaries-only --version "$version" --arch x86_64 \
    --out "$work/bundle" --identity "$identity" "${binaries[@]}" \
    --include "$closure/lib/gstreamer-1.0" --include "$closure/libexec" \
    --include "$closure/licenses" --include "$work/tilo.std.wasm" >/dev/null
  # What it carries of GStreamer is the reviewed closure, byte for byte: no GPL code, no encoder.
  python3 -I "$here/gstreamer_runtime.py" audit "$work/bundle" \
    /target/windows/x86_64/gstreamer/gstreamer-runtime-x86_64.json
  tools/apps/pack-runtime-package.sh "$work/bundle" so.tilo.runtime.tilolang "tilolang runtime" \
    "$version" "$identity" "$tools" "$work/package" windows >/dev/null
  local whole=("$work"/package/whole-*.tapp)
  [ "${#whole[@]}" = 1 ] && [ -f "${whole[0]}" ] || { printf 'no runtime package was packed\n' >&2; exit 1; }
  root="$("$tools/tilo-app-package" inspect "${whole[0]}" | sed -n 's/^package-id: tapp://p')"
  [[ "$root" =~ ^[0-9a-f]{64}$ ]] || { printf 'unexpected runtime package id %s\n' "$root" >&2; exit 1; }
  rm -rf -- "$runtimes" && mkdir -p "$runtimes"
  install -m 0644 "${whole[0]}" "$runtimes/$root.tapp"
  digest "$runtimes/$root.tapp"
  "$tools/tilo-runtime-bundle" key pubkey --identity "$identity" >"$runtimes/trusted-publishers.txt"
  rm -rf -- "$work"
}

case "$mode" in
  sdk)
    out="${3:?output directory}"
    mkdir -p "$out"
    export XWIN_CACHE_DIR="${TMPDIR:-/tmp}/xwin-download-$arch" XWIN_ARCH="$arch"
    rm -rf -- "$XWIN_CACHE_DIR"
    mkdir -p "$XWIN_CACHE_DIR"
    seed_windows_sdk_manifests "$XWIN_CACHE_DIR"
    # `cargo xwin env` sets up the toolchain (downloading the CRT and SDK
    # into XWIN_CACHE_DIR, from the manifests seeded above) and prints the
    # environment it would build with. It runs in an empty crate of its own:
    # in the checkout cargo would read every member of the workspace, and the
    # download would then depend on all of them rather than on the pins alone.
    probe="${TMPDIR:-/tmp}/xwin-probe-$arch"
    rm -rf -- "$probe"
    mkdir -p "$probe/src"
    printf '[package]\nname = "xwin-probe"\nversion = "0.0.0"\nedition = "2021"\n\n[workspace]\n' >"$probe/Cargo.toml"
    : >"$probe/src/lib.rs"
    # The same toolchain the checkout builds with, which is all of it the probe reads.
    cp "$here/../../../rust-toolchain.toml" "$probe/"
    (cd "$probe" && cargo xwin env --target "$target" >/dev/null)
    rm -rf -- "$probe"
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
    # The SDK an app is written against, with the native runner every app gets by default: an app
    # host that runs its windows on Windows needs it to compile there.
    cargo xwin check --locked --keep-going -p tilo-app --target "$target"
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
    cargo xwin build --locked -p tilo-daemon -p tilolang-process \
      --features tilolang-process/app-launch --target "$target"
    install -m 0644 "${CARGO_TARGET_DIR:-target}/$target/debug/tilo-runtime.exe" "$out/tilo-runtime.exe"
    digest "$out/tilo-runtime.exe"
    if [ "$arch" = x86_64 ]; then
      runtime_package "${CARGO_TARGET_DIR:-target}/$target/debug/tilolang-process.exe" "$out/runtimes"
    fi
    ;;
  *) printf 'unknown mode: %s\n' "$mode" >&2; exit 2 ;;
esac
