#!/usr/bin/env bash
# Build the Imager on its native release runner, export it against the pinned
# Flatpak runtime, and produce the single-file bundle linked by the release
# page. The final sandbox smoke-load is the ABI compatibility gate.
set -euo pipefail

repo="$(git rev-parse --show-toplevel)"
out="${1:-$repo/dist/qos-imager}"
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

cargo build --locked --release -p imager --features flatpak-media-writer

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
stage="$work/manifest"
mkdir -p "$stage" "$out"
cp "$repo/apps/imager/packaging/flatpak/org.qos.Imager.yml" "$stage/"
cp "$repo/target/release/imager" "$stage/qos-imager-bin"
sed 's/^Icon=qos-imager$/Icon=org.qos.Imager/' \
  "$repo/apps/imager/packaging/linux/qos-imager.desktop" \
  >"$stage/org.qos.Imager.desktop"
cp "$repo/apps/imager/packaging/linux/org.qos.Imager.metainfo.xml" "$stage/"
cp "$repo/apps/imager/packaging/linux/qos-imager.svg" "$stage/org.qos.Imager.svg"

flatpak-builder "$flatpak_scope" --force-clean --disable-download \
  --disable-rofiles-fuse \
  --state-dir="$work/state" \
  --repo="$work/repository" "$work/build" "$stage/org.qos.Imager.yml"
artifact="$out/qos-imager-linux-$arch.flatpak"
flatpak build-bundle "$work/repository" "$artifact" org.qos.Imager
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
flatpak info --user org.qos.Imager >/dev/null
flatpak run --user org.qos.Imager --qos-package-smoke
flatpak run --user org.qos.Imager --qos-flatpak-probe-smoke
flatpak run --user --filesystem="$smoke" org.qos.Imager \
  --shots "$smoke/flatpak-installed.png" \
  --scenario done --viewport 1180x820
test -s "$smoke/flatpak-installed.png"
flatpak uninstall --user --noninteractive org.qos.Imager
