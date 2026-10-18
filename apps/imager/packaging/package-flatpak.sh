#!/usr/bin/env bash
# Build the Imager on its native release runner, export it against the pinned
# Flatpak runtime, and produce the single-file bundle linked by the release
# page. The final sandbox smoke-load is the ABI compatibility gate.
set -euo pipefail

repo="$(git rev-parse --show-toplevel)"
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

if [ "${TILO_REQUIRE_SIGNING:-0}" = 1 ]; then export TILO_REQUIRE_CAPTURE=1; fi
source "$repo/apps/imager/packaging/build-capture-host.sh" --features flatpak-media-writer

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
stage="$work/manifest"
mkdir -p "$stage" "$out"
cp "$repo/apps/imager/packaging/flatpak/org.tilo.Imager.yml" "$stage/"
cp "$repo/target/release/imager" "$stage/tilo-imager-bin"
cp "$repo/target/release/tilo-enroll" "$stage/tilo-enroll"
"$repo/apps/imager/packaging/stage-capture-sidecar.sh" "$repo/target/release/imager" "$stage/capture-prefix" "$stage/tilo-imager-bin"
if [ -d "$stage/capture-prefix/share/tilo-imager/capture" ]; then
  tar -C "$stage/capture-prefix" -cf "$stage/tilo-capture-sidecar.tar" share/tilo-imager/capture
else
  # Flatpak keeps a fixed manifest; fresh-install builds carry an empty archive.
  mkdir -p "$stage/empty-capture"
  tar -C "$stage/empty-capture" -cf "$stage/tilo-capture-sidecar.tar" --files-from /dev/null
fi
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
flatpak run --user --command=tilo-enroll org.tilo.Imager pair-device --help >/dev/null
flatpak run --user org.tilo.Imager --tilo-package-smoke
flatpak run --user org.tilo.Imager --tilo-flatpak-probe-smoke
flatpak run --user --filesystem="$smoke" org.tilo.Imager \
  --shots "$smoke/flatpak-installed.png" \
  --scenario done --viewport 1180x820
test -s "$smoke/flatpak-installed.png"
flatpak uninstall --user --noninteractive org.tilo.Imager
