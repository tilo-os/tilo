#!/usr/bin/env bash
# Cross-build exact-source Windows ARM64 binaries on Linux ARM64, for hosts without a Rust build
# tree or a Windows build machine (t.toml: windows-arm64-tilo-vm, windows-arm64-imager):
#
#   cross-arm64.sh tilo-vm OUT  the development CLI, to iterate with WHPX, and its unit tests
#   cross-arm64.sh imager OUT   the imager and its test executable, which runs on the target
#                               host with file-backed firmware variables before any physical write
#
# The builder image carries the MSVC targets, cargo-xwin, clang-cl and lld-link; cargo-xwin
# downloads the MSVC CRT and Windows SDK, which may not be redistributed in the image.
set -euo pipefail

what=${1:?usage: cross-arm64.sh tilo-vm|imager OUT} out=${2:?usage: cross-arm64.sh tilo-vm|imager OUT}
[[ $(uname -m) == aarch64 ]] || { printf 'cross-arm64.sh runs on ARM64 Linux\n' >&2; exit 1; }
export XWIN_CACHE_DIR=${XWIN_CACHE_DIR:-${TMPDIR:-/tmp}/xwin}
# ring forces clang for Windows ARM64, bypassing cc-rs' CC setting. Use cargo-xwin's clang
# backend so its SDK flags match that driver, not clang-cl's (/imsvc).
export XWIN_CROSS_COMPILER=clang
target=$(realpath -m "${CARGO_TARGET_DIR:-target}")/aarch64-pc-windows-msvc/release
commit=${T_COMMIT:-$(git rev-parse HEAD 2>/dev/null || echo unknown)}
rm -rf "$out"
mkdir -p "$out"

case $what in
  tilo-vm)
    cargo xwin build -p tilo-vm --locked --release --target aarch64-pc-windows-msvc
    cargo test -p tilo-vm --locked --lib
    cp "$target/tilo-vm.exe" "$out/tilo-vm.exe"
    (cd "$out" && sha256sum tilo-vm.exe > tilo-vm.exe.sha256)
    ;;
  imager)
    # A file-backed firmware handoff test binary, not the optimized public imager release: skip
    # workspace fat LTO, which links for minutes.
    export CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
    # Clang treats the SDK's __prefetch declaration as a conflicting builtin in zstd-sys, so
    # rename that unused declaration while compiling its vendored sources. Zstd's NEON path emits
    # unresolved helper calls with this MSVC cross toolchain; use its portable decoder for the
    # Windows ARM64 host helper.
    TILO_REAL_CLANG=$(command -v clang-18 || command -v clang)
    export TILO_REAL_CLANG
    wrapper=$(mktemp -d "${TMPDIR:-/tmp}/tilo-clang.XXXXXX")
    cat > "$wrapper/clang" <<'SH'
#!/bin/sh
case " $* " in
  *zstd/lib/*) exec "$TILO_REAL_CLANG" -D__prefetch=__tilo_sdk_prefetch -DZSTD_NO_INTRINSICS "$@" ;;
  *) exec "$TILO_REAL_CLANG" "$@" ;;
esac
SH
    chmod +x "$wrapper/clang"
    export PATH="$wrapper:$PATH"
    cargo xwin test -p imager --lib --locked --release --target aarch64-pc-windows-msvc --no-run
    cargo xwin build -p imager --bin imager --locked --release --target aarch64-pc-windows-msvc
    cp "$target/imager.exe" "$out/imager.exe"
    mapfile -t tests < <(find "$target/deps" -maxdepth 1 -name 'imager-*.exe' | sort)
    [[ ${#tests[@]} == 1 ]] || { printf 'expected one imager test executable, found %s\n' "${#tests[@]}" >&2; exit 1; }
    cp "${tests[0]}" "$out/imager-tests.exe"
    (cd "$out" && sha256sum imager.exe imager-tests.exe > sha256.txt)
    ;;
  *)
    printf 'usage: cross-arm64.sh tilo-vm|imager OUT\n' >&2
    exit 2
    ;;
esac
printf '%s\n' "$commit" > "$out/source-commit.txt"
printf 'Windows ARM64 %s from %s: t outputs windows-arm64-%s\n' "$what" "$commit" "$what"
