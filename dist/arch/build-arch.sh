#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
version=$(<"$repo_root/dist/VERSION")
out=${TILO_PACKAGE_OUT:-$repo_root/dist/arch/build}
work=$out/work
[[ ! -e $work ]] || { echo "work path already exists: $work" >&2; exit 1; }
mkdir -p "$out" "$work/tilo-$version"
trap 'rm -rf -- "$work"' EXIT

git -C "$repo_root" archive HEAD | tar -x -C "$work/tilo-$version"
# Include the small set of working-tree inputs needed to validate packaging
# before commit, without copying dist/*/build back into its own source archive.
mkdir -p "$work/tilo-$version/dist/session" "$work/tilo-$version/.cargo"
install -pm 0755 "$repo_root/dist/stage-payload.sh" "$work/tilo-$version/dist/stage-payload.sh"
install -pm 0644 "$repo_root/dist/VERSION" "$work/tilo-$version/dist/VERSION"
cp -a "$repo_root/dist/session/." "$work/tilo-$version/dist/session/"
install -pm 0644 "$repo_root/.cargo/config.toml" "$work/tilo-$version/.cargo/config.toml"
install -pm 0644 "$repo_root/Cargo.toml" "$repo_root/Cargo.lock" \
    "$repo_root/rust-toolchain.toml" "$work/tilo-$version/"
tar -C "$work" -czf "$work/tilo-$version.tar.gz" "tilo-$version"
install -pm 0644 "$repo_root/dist/arch/PKGBUILD" "$work/PKGBUILD"
sed -i "s/^pkgver=.*/pkgver=$version/" "$work/PKGBUILD"
# Arch Linux ARM still defaults to xz on some images, while the release and
# Forgejo package contracts use the .pkg.tar.zst format.  Pin the
# compressor locally instead of relying on the base image's makepkg.conf.
command -v zstd >/dev/null
makepkg_config=$work/makepkg.conf
cp /etc/makepkg.conf "$makepkg_config"
if grep -q '^PKGEXT=' "$makepkg_config"; then
  sed -i "s/^PKGEXT=.*/PKGEXT='.pkg.tar.zst'/" "$makepkg_config"
else
  printf "%s\n" "PKGEXT='.pkg.tar.zst'" >> "$makepkg_config"
fi
(
  cd "$work"
  PKGDEST="$out" SRCDEST="$work" BUILDDIR="$work/makepkg" \
    makepkg --config "$makepkg_config" --syncdeps --noconfirm --cleanbuild
)
printf '%s\n' "$out"/*.pkg.tar.*
