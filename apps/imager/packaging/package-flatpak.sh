#!/usr/bin/env bash
# Build the Imager on its native release runner, export it against the pinned
# Flatpak runtime, and produce the single-file bundle linked by the release
# page. The final sandbox smoke-load is the ABI compatibility check.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd -P)"
out="${1:-$repo/dist/tilo-imager}"
arch="$(flatpak --default-arch)"
case "$arch" in
  x86_64|aarch64) ;;
  *) printf 'unsupported Flatpak architecture: %s\n' "$arch" >&2; exit 2 ;;
esac
for tool in flatpak flatpak-builder; do
  command -v "$tool" >/dev/null \
    || { printf '%s is required to build the Flatpak\n' "$tool" >&2; exit 1; }
done
if flatpak info --system "org.freedesktop.Sdk//25.08" >/dev/null 2>&1; then
  flatpak_scope="--system"
elif flatpak info --user "org.freedesktop.Sdk//25.08" >/dev/null 2>&1; then
  flatpak_scope="--user"
else
  printf 'org.freedesktop.Sdk//25.08 is not installed\n' >&2
  exit 1
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
target="${CARGO_TARGET_DIR:-$repo/target}"
# The compiler's tree and the LLVM library its linker loads live under the host's /usr, which a
# Flatpak sandbox reserves for its runtime: the link reads a copy under the build directory,
# which the sandbox is given.
sysroot="$("${RUSTC:-rustc}" --print sysroot)"
triple="$("${RUSTC:-rustc}" -vV | sed -n 's/^host: //p')"
rustdir="$target/rust-sysroot"
mkdir -p "$rustdir/lib/rustlib"
cp -aL "$sysroot/lib/rustlib/$triple" "$rustdir/lib/rustlib/"
for lib in "$sysroot"/lib/libLLVM.so*; do
  if [ -e "$lib" ]; then cp -aL "$lib" "$rustdir/lib/"; fi
done
# Link the binary against the runtime's C library, not this host's: the sandbox it is exported
# into carries the runtime's glibc, and a host newer than it (Fedora 44's on the arm64 release
# runner, beside a 25.08 runtime) would otherwise bake in symbol versions the runtime lacks.
# Compilation stays on the host, so the compiler cache keeps the objects; only the link runs in
# the SDK, which resolves libc and the startup files from the runtime's own tree.
linker="$work/sdk-linker"
printf '#!/bin/bash\nexec flatpak run %q --command=gcc --env=%q --filesystem=%q --filesystem=%q org.freedesktop.Sdk//25.08 "$@"\n' \
  "$flatpak_scope" "LD_LIBRARY_PATH=$rustdir/lib" "$repo" "$target" >"$linker"
chmod 0755 "$linker"
"${CARGO:-cargo}" rustc --locked --release -p imager --bin imager --features flatpak-media-writer \
  -- -C "linker=$linker" --sysroot="$rustdir"
binary="${TILO_IMAGER_BINARY:-$target/release/imager}"

stage="$work/manifest"
mkdir -p "$stage" "$out"
cp "$repo/apps/imager/packaging/flatpak/org.tilo.Imager.yml" "$stage/"
cp "$binary" "$stage/tilo-imager-bin"
sed 's/^Icon=tilo-imager$/Icon=org.tilo.Imager/' \
  "$repo/apps/imager/packaging/linux/tilo-imager.desktop" \
  >"$stage/org.tilo.Imager.desktop"
cp "$repo/apps/imager/packaging/linux/org.tilo.Imager.metainfo.xml" "$stage/"
cp "$repo/apps/imager/packaging/linux/tilo-imager.svg" "$stage/org.tilo.Imager.svg"

flatpak-builder "$flatpak_scope" --force-clean --disable-download \
  --disable-rofiles-fuse \
  --state-dir="$work/state" \
  --repo="$work/repository" "$work/build" "$stage/org.tilo.Imager.yml"
artifact="$out/tilo-imager-linux-$arch.flatpak"
flatpak build-bundle "$work/repository" "$artifact" org.tilo.Imager
(
  cd "$out"
  sha256sum "$(basename "$artifact")" >"$(basename "$artifact").sha256"
)

# Test the file users actually download, rather than only running the builder
# tree which produced it. A private per-job user installation proves import,
# deployment, runtime resolution, the exported command and the final sandbox
# permissions without changing the runner's ordinary Flatpak inventory.
flatpak info --system "org.freedesktop.Platform//25.08" >/dev/null 2>&1 \
  || { printf 'the system Flatpak 25.08 runtime is required for isolated bundle verification\n' >&2; exit 1; }
isolated_install="$work/flatpak-user"
smoke="$work/installed-smoke"
mkdir -p "$isolated_install" "$smoke"
chmod 0700 "$isolated_install"
export FLATPAK_USER_DIR="$isolated_install"
flatpak install --user --noninteractive --no-deps --bundle "$artifact"
flatpak info --user org.tilo.Imager >/dev/null
flatpak run --user org.tilo.Imager --tilo-package-smoke
flatpak run --user org.tilo.Imager --tilo-flatpak-probe-smoke
flatpak run --user --filesystem="$smoke" org.tilo.Imager \
  --shots "$smoke/flatpak-installed.png" \
  --scenario done --viewport 1180x820
test -s "$smoke/flatpak-installed.png"
flatpak uninstall --user --noninteractive org.tilo.Imager
