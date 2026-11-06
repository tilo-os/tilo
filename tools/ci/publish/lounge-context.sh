#!/usr/bin/env bash
# The Lounge image's build context (os/containers/tilo-lounge): the programs
# (lounge-binaries.sh), the bot programs the workspace installs by path, and
# the product font catalog its app components lay text out from, since a
# container has no system fonts. usage: lounge-context.sh OUT BINARIES
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
usage='usage: lounge-context.sh OUT BINARIES'
out=${1:?$usage}
binaries=${2:?$usage}
cd "$repo_root"
[[ -x $binaries/tilo-relay && -x $binaries/discord-worker ]] || die "no Lounge programs in $binaries"
target=${CARGO_TARGET_DIR:-$repo_root/target}
rm -rf "$out"
mkdir -p "$out/bin" "$out/bots"
cp "$binaries/tilo-relay" "$binaries/discord-worker" "$out/bin/"
export TILO_FONT_ASSET_CACHE=$target/font-artifacts
TILO_FONT_ASSETS_OFFLINE=0 python3 tools/font-catalog.py prepare crates/sdk/ui-core/assets/fonts \
  "$out/fonts" --scope product
cp apps/discord/bridge/program.json "$out/bots/discord-bridge.json"
container=os/containers/tilo-lounge
cp "$container/Containerfile" "$container/Containerfile.dockerignore" "$container/entrypoint.sh" \
  os/scripts/verify-no-cisco-openh264.sh "$out/"
# The image's revision label and TILO_SOURCE_REVISION: the commit this context
# was built from, which GET /mode reports.
git rev-parse HEAD > "$out/source-revision"
