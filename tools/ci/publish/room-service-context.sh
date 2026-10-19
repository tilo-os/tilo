#!/usr/bin/env bash
# The room service image's build context (crates/hosting/tilo-room-service/container): the
# programs (room-service-binaries.sh), the product fonts, the desktop bundle
# (web-desktop-bundle.sh, shared with the web entry's image) and the room's pages. The programs
# and the bundle are outputs of checks of their own that build side by side; this only lays them
# out, so the Containerfile only copies them.
# usage: room-service-context.sh OUT BINARIES BUNDLE
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
usage='usage: room-service-context.sh OUT BINARIES BUNDLE'
out=${1:?$usage}
binaries=${2:?$usage}
bundle=${3:?$usage}
cd "$repo_root"
[[ -x $binaries/tilo-relay && -x $binaries/tilo-room-service ]] || die "no room service programs in $binaries"
[[ -f $bundle/build.json ]] || die "no desktop bundle at $bundle"
target=${CARGO_TARGET_DIR:-$repo_root/target}
rm -rf "$out"
mkdir -p "$out/bin" "$out/bots"
cp "$binaries"/* "$out/bin/"
export TILO_FONT_ASSET_CACHE=$target/font-artifacts
TILO_FONT_ASSETS_OFFLINE=0 python3 tools/font-catalog.py prepare crates/sdk/ui-core/assets/fonts \
  "$out/bin/fonts" --scope product
cp apps/discord/bridge/program.json "$out/bots/discord-bridge.json"
cp -r "$bundle" "$out/assets"
chmod -R u+w "$out/assets"
cp crates/hosting/tilo-room-service/web/* "$out/assets/"
cp crates/hosting/tilo-room-service/container/Containerfile \
  crates/hosting/tilo-room-service/container/Containerfile.dockerignore \
  os/scripts/verify-no-cisco-openh264.sh "$out/"
# The image's revision label: the commit this context was built from.
git rev-parse HEAD > "$out/source-revision"
