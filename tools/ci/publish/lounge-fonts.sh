#!/usr/bin/env bash
# The Lounge's product font catalog: the fonts its app components lay text out
# from, which a container has no system fonts to take. The image's context and
# the checks that run the relay directly both serve it, so it is built once,
# apart from either. usage: lounge-fonts.sh OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
out=${1:?usage: lounge-fonts.sh OUT}
cd "$repo_root"
target=${CARGO_TARGET_DIR:-$repo_root/target}
rm -rf "$out"
mkdir -p "$(dirname -- "$out")"
export TILO_FONT_ASSET_CACHE=$target/font-artifacts
TILO_FONT_ASSETS_OFFLINE=0 python3 tools/font-catalog.py prepare crates/sdk/ui-core/assets/fonts \
  "$out" --scope product
