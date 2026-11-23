#!/usr/bin/env bash
# Build and package the standalone Linux installer the way a release does:
# the portable tarball and AppImage (apps/imager/packaging/package-host.sh) and
# the Flatpak bundle (apps/imager/packaging/package-flatpak.sh), each with its
# SHA-256 sidecar, natively on this machine's architecture.
#
#   package.sh <arch> <out>
#
# The AppImage tools are the pinned ones os-assets-appimage-<arch> verified into
# os/external-assets/cache; nothing is downloaded implicitly. Without signing
# credentials in the environment the artifacts are unsigned: that is the dry
# run every head of main gets, and an imager-v* tag adds the signing step.
set -euo pipefail

arch="${1:?x86_64 or aarch64}"
out="${2:?output directory}"
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd -P)"
cd "$repo"
case "$(uname -m)" in
  x86_64|amd64) [ "$arch" = x86_64 ] ;;
  aarch64|arm64) [ "$arch" = aarch64 ] ;;
  *) false ;;
esac || { printf 'package.sh %s must run on a native %s machine, not %s\n' "$arch" "$arch" "$(uname -m)" >&2; exit 1; }

command -v flatpak-builder >/dev/null && command -v capnp >/dev/null || \
  dnf install -y -q --setopt=install_weak_deps=False \
    flatpak flatpak-builder capnproto bubblewrap fuse fuse-libs file desktop-file-utils
# The SDK and runtime the manifest names, system-wide, as a release builder has them.
flatpak remote-add --system --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --system --noninteractive flathub \
  "org.freedesktop.Sdk//25.08" "org.freedesktop.Platform//25.08"

cache="$repo/os/external-assets/cache"
tools="${TMPDIR:-/tmp}/tilo-imager-appimage-tools"
mkdir -p "$tools"
install -m 0755 "$cache/linuxdeploy-1-alpha-20251107-1-$arch.AppImage" "$tools/linuxdeploy"
install -m 0755 "$cache/appimagetool-1.9.1-$arch.AppImage" "$tools/appimagetool"
export PATH="$tools:$PATH"
export TILO_APPIMAGE_RUNTIME="$cache/type2-runtime-20251108-$arch"
# A job has no FUSE device: the AppImage tools run from their extracted tree.
export APPIMAGE_EXTRACT_AND_RUN=1
# linuxdeploy's own strip predates the `.relr.dyn` sections Fedora's libraries carry and refuses
# them; the libraries ship as Fedora built them.
export NO_STRIP=1
export TILO_REQUIRE_NATIVE_PACKAGES=1

case "${T_REF:-}" in
  tag:imager-v*)
    # The installer offers the media this tag built: release-catalog/imager's catalogue.
    export TILO_RELEASE_CATALOG=/target/imager-release/catalog/tilo-installer-catalog-v2.json
    [ -s "$TILO_RELEASE_CATALOG" ] \
      || { printf 'missing %s: a release tag builds the installer with its own media catalogue\n' "$TILO_RELEASE_CATALOG" >&2; exit 1; }
    export TILO_VERSION="${T_REF#tag:imager-v}"
    ;;
esac

rm -rf "$out" && mkdir -p "$out"
apps/imager/packaging/package-host.sh "$out"
apps/imager/packaging/package-flatpak.sh "$out"
# The staging trees the packagers leave beside the artifacts are not shipped.
rm -rf "$out/Tilo_Imager.AppDir" "$out/tilo-imager-linux-$arch"
for artifact in tar.gz AppImage flatpak; do
  file="$out/tilo-imager-linux-$arch.$artifact"
  [ -s "$file" ] && [ -s "$file.sha256" ] \
    || { printf 'the Linux release is missing %s or its checksum\n' "$file" >&2; exit 1; }
  (cd "$out" && sha256sum -c "$(basename "$file").sha256")
done
