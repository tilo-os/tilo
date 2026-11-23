#!/usr/bin/env bash
# Install and start each Linux installer download in a machine that never built
# it: the tarball unpacked and run, the AppImage run as a person double-clicks
# it (apps/imager/packaging/verify-appimage-runtime.sh), and the Flatpak bundle
# installed into a private user installation and run. Each start renders the
# installer offscreen and must write its screenshot.
#
#   install.sh <arch> <package-dir>
set -euo pipefail

arch="${1:?x86_64 or aarch64}"
packages="${2:?the package/linux-<arch> output}"
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd -P)"
base="tilo-imager-linux-$arch"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

for artifact in tar.gz AppImage flatpak; do
  (cd "$packages" && sha256sum -c "$base.$artifact.sha256")
done

tar -xzf "$packages/$base.tar.gz" -C "$work"
"$work/$base/tilo-imager" --shots "$work/tarball.png" --scenario confirm --viewport 1180x820
test -s "$work/tarball.png"

cp "$packages/$base.AppImage" "$work/"
chmod +x "$work/$base.AppImage"
bash "$repo/apps/imager/packaging/verify-appimage-runtime.sh" "$work/$base.AppImage" "$work/appimage"

command -v flatpak >/dev/null || dnf install -y -q --setopt=install_weak_deps=False flatpak
export FLATPAK_USER_DIR="$work/flatpak-user"
mkdir -p "$FLATPAK_USER_DIR" "$work/flatpak-shots"
chmod 0700 "$FLATPAK_USER_DIR"
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --user --noninteractive flathub "org.freedesktop.Platform//25.08"
flatpak install --user --noninteractive --no-deps --bundle "$packages/$base.flatpak"
flatpak run --user org.tilo.Imager --tilo-package-smoke
flatpak run --user --filesystem="$work/flatpak-shots" org.tilo.Imager \
  --shots "$work/flatpak-shots/installed.png" --scenario done --viewport 1180x820
test -s "$work/flatpak-shots/installed.png"
flatpak uninstall --user --noninteractive org.tilo.Imager
printf 'installed and started %s (tarball, AppImage, Flatpak)\n' "$base"
