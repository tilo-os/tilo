#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
version=$(<"$repo_root/dist/VERSION")
out=${QOS_PACKAGE_OUT:-$repo_root/dist/arch/build}
work=$out/work
[[ ! -e $work ]] || { echo "work path already exists: $work" >&2; exit 1; }
mkdir -p "$out" "$work/qos-$version"
trap 'rm -rf -- "$work"' EXIT

git -C "$repo_root" archive HEAD | tar -x -C "$work/qos-$version"
# Include the small set of working-tree inputs needed to validate packaging
# before commit, without copying dist/*/build back into its own source archive.
mkdir -p "$work/qos-$version/dist/session" "$work/qos-$version/.cargo"
install -pm 0755 "$repo_root/dist/stage-payload.sh" "$work/qos-$version/dist/stage-payload.sh"
install -pm 0644 "$repo_root/dist/VERSION" "$work/qos-$version/dist/VERSION"
cp -a "$repo_root/dist/session/." "$work/qos-$version/dist/session/"
install -pm 0644 "$repo_root/.cargo/config.toml" "$work/qos-$version/.cargo/config.toml"
install -pm 0644 "$repo_root/Cargo.toml" "$repo_root/Cargo.lock" \
    "$repo_root/rust-toolchain.toml" "$work/qos-$version/"
tar -C "$work" -czf "$work/qos-$version.tar.gz" "qos-$version"
install -pm 0644 "$repo_root/dist/arch/PKGBUILD" "$work/PKGBUILD"
sed -i "s/^pkgver=.*/pkgver=$version/" "$work/PKGBUILD"
(
  cd "$work"
  PKGDEST="$out" SRCDEST="$work" BUILDDIR="$work/makepkg" makepkg --syncdeps --noconfirm --cleanbuild
)
printf '%s\n' "$out"/*.pkg.tar.*
