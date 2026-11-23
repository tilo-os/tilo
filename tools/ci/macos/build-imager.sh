#!/usr/bin/env bash
# The macOS build of the installer: the imager, packaged for one architecture,
# built on a Mac. Apple silicon builds its own architecture; x86_64 is
# cross-built on it, and the install check starts that binary through Rosetta
# as it runs on an Intel Mac. The native architecture also runs the library's
# tests, which is where the macOS-only half of the code is exercised.
#
#   build-imager.sh <x86_64|aarch64> <out-dir>
#
# OUT-DIR holds imager and imager.sha256.
set -euo pipefail

arch="${1:?architecture: x86_64 or aarch64}"
out="${2:?output directory}"
[ "$(uname -s)" = Darwin ] \
  || { printf 'build-imager.sh runs on macOS only\n' >&2; exit 2; }
case "$arch" in
  x86_64) triple=x86_64-apple-darwin ;;
  aarch64) triple=aarch64-apple-darwin ;;
  *) printf 'unsupported architecture: %s\n' "$arch" >&2; exit 2 ;;
esac
host_triple="$(rustc -vV | sed -n 's/^host: //p')"

rustup target add "$triple"
cargo build --locked --release -p imager --features host-media-writer --target "$triple"

target_dir="${CARGO_TARGET_DIR:-$PWD/target}"
built="$target_dir/$triple/release/imager"
[ -f "$built" ] || { printf '%s was not built\n' "$built" >&2; exit 1; }
wanted_arch=x86_64
if [ "$arch" = aarch64 ]; then wanted_arch=arm64; fi
carried="$(lipo -archs "$built")"
[ "$carried" = "$wanted_arch" ] \
  || { printf '%s is built for %s, not %s\n' "$built" "$carried" "$wanted_arch" >&2; exit 1; }

mkdir -p "$out"
out="$(cd -- "$out" && pwd)"
cp "$built" "$out/imager"
chmod +x "$out/imager"
(cd "$out" && shasum -a 256 imager > imager.sha256)

# Only the native architecture: the cross build's tests would run translated
# on this Mac and say nothing about an Intel Mac, while the executable it
# produces is proven by the packaged DMG started under Rosetta. The tests run
# with the features the shipped binary is built with, so they exercise that
# configuration and no other.
if [ "$triple" = "$host_triple" ]; then
  cargo test --locked -p imager --lib --features host-media-writer
fi

printf 'built %s for %s\n' "$out/imager" "$triple"
