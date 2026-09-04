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
  --repo="$work/repository" "$work/build" "$stage/org.qos.Imager.yml"
artifact="$out/qos-imager-linux-$arch.flatpak"
flatpak build-bundle "$work/repository" "$artifact" org.qos.Imager
(
  cd "$out"
  sha256sum "$(basename "$artifact")" >"$(basename "$artifact").sha256"
)

# Prove the exported app resolves entirely inside its runtime. The demo flag
# avoids hardware access; the real writer is tested separately against a file
# descriptor in Rust tests and on a disposable USB release runner.
flatpak-builder "$flatpak_scope" --run "$work/build" "$stage/org.qos.Imager.yml" \
  /app/bin/qos-imager --qos-package-smoke
flatpak-builder "$flatpak_scope" --run "$work/build" "$stage/org.qos.Imager.yml" \
  /app/bin/qos-imager --qos-flatpak-probe-smoke
