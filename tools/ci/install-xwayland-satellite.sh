#!/usr/bin/env bash
# Build xwayland-satellite from its pinned upstream source with Tilo's patches
# (os/external-assets/xwayland-satellite/README.md) and install it under a
# prefix. The OS image, every CI builder and the legacy VM guest install the
# same build to /usr.
#
# The caller owns fetching the source into the asset cache and installing the
# build dependencies (cargo, clang-devel, fontconfig-devel, wayland-devel,
# xcb-util-cursor-devel). At runtime the binary needs Xwayland, xcb-util-cursor
# and fontconfig with open-sans-fonts.
#
#   install-xwayland-satellite.sh CACHE ABSOLUTE_PREFIX ASSET_MANIFEST PATCH_DIR
#   install-xwayland-satellite.sh --asset-name
#   install-xwayland-satellite.sh --stamp ASSET_MANIFEST PATCH_DIR
#
# --stamp prints what an install records in
# PREFIX/share/licenses/tilo-xwayland-satellite/SOURCE, so a caller can tell
# whether the installed build is this one.
set -euo pipefail

# v0.8.2 plus the patches. Upstream review of the clipboard handoff fix:
# https://github.com/Supreeeme/xwayland-satellite/pull/527. Once a release
# contains it, the image and CI can install the distribution package again.
asset=xwayland-satellite-8d135d3.tar.gz
source_url=https://github.com/Supreeeme/xwayland-satellite/tree/8d135d3b2854b30fd01ea6cd6c27e523dd50a839
features=systemd,fontconfig

usage() {
  echo 'usage: install-xwayland-satellite.sh CACHE ABSOLUTE_PREFIX ASSET_MANIFEST PATCH_DIR' >&2
  echo '       install-xwayland-satellite.sh --asset-name | --stamp ASSET_MANIFEST PATCH_DIR' >&2
  exit 2
}

stamp() {
  local manifest=$1 patches=$2 expected
  expected=$(awk -F '\t' -v name="$asset" '$1 !~ /^#/ && $2 == name {print $1}' "$manifest")
  [[ $expected =~ ^[0-9a-f]{64}$ ]] || { echo "missing or ambiguous digest for $asset" >&2; exit 1; }
  printf '%s  %s\n' "$expected" "$asset"
  (cd "$patches" && sha256sum -- *.patch)
}

if [[ $# == 1 && $1 == --asset-name ]]; then
  printf '%s\n' "$asset"
  exit 0
fi
if [[ $# == 3 && $1 == --stamp ]]; then
  stamp "$2" "$3"
  exit 0
fi
[[ $# == 4 ]] || usage
cache=$1
destination=$2
manifest=$3
patches=$(cd "$4" && pwd)
[[ $destination == /* ]] || { echo 'xwayland-satellite destination must be absolute' >&2; exit 2; }
recorded=$(stamp "$manifest" "$patches")
printf '%s\n' "$recorded" | head -n 1 | (cd "$cache" && sha256sum --check --strict -) >&2

stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT
mkdir "$stage/src"
tar -xzf "$cache/$asset" --strip-components=1 --no-same-owner -C "$stage/src"
# The ceiling keeps git apply from treating an enclosing checkout (a TMPDIR
# inside the CI workspace) as the tree to patch.
for patch in "$patches"/*.patch; do
  (cd "$stage/src" && GIT_CEILING_DIRECTORIES="$stage" git apply --whitespace=nowarn "$patch")
done
# The library tests include the patch's own handoff cases; they need no
# display server. Build and test share the release profile, so the binary's
# dependencies compile once.
(
  cd "$stage/src"
  export CARGO_TARGET_DIR="$stage/target" CARGO_PROFILE_RELEASE_DEBUG=0
  cargo test --locked --release --features "$features" --lib
  CARGO_PROFILE_RELEASE_STRIP=symbols cargo build --locked --release --features "$features"
)

licenses="$destination/share/licenses/tilo-xwayland-satellite"
install -D -m 0755 "$stage/target/release/xwayland-satellite" "$destination/bin/xwayland-satellite"
install -D -m 0644 -t "$licenses" "$stage/src/LICENSE" "$patches"/*.patch
printf '%s\n' \
  'xwayland-satellite - rootless Xwayland integration' \
  'License: MPL-2.0 (see LICENSE)' \
  "Source: $source_url (v0.8.2)" \
  'Modified: bin/xwayland-satellite is built from the source above with the patches here.' \
  > "$licenses/NOTICE"
printf '%s\n' "$recorded" > "$licenses/SOURCE"
# The probe the shell makes before starting the bridge.
"$destination/bin/xwayland-satellite" --test-listenfd-support
