#!/usr/bin/env bash
# The browser desktop bundle (crates/browser/tilo-web-desktop, release, split into modules) that
# the room service's and the web entry's images serve. Built once per commit in a check of its
# own: both image contexts take it as an output instead of each compiling every app to wasm
# again, and it builds beside the room service's native binaries rather than after them.
# usage: web-desktop-bundle.sh OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
out=${1:?usage: web-desktop-bundle.sh OUT}
cd "$repo_root"
export TILO_FONT_ASSET_CACHE=${CARGO_TARGET_DIR:-$repo_root/target}/font-artifacts
python3 tools/font-catalog.py root crates/sdk/ui-core/assets/fonts --scope fixtures --cache "$TILO_FONT_ASSET_CACHE" >/dev/null
rm -rf "$out"
mkdir -p "$(dirname -- "$out")"
moq=$scratch/moq-browser
rm -rf "$moq" && cp -r scripts/moq-browser "$moq"
TILO_MOQ_BROWSER_DIR=$moq crates/browser/tilo-browser-runtime/scripts/build.sh "$out" --bundle desktop >/dev/null
