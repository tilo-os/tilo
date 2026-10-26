#!/usr/bin/env bash
# The browser desktop bundle (crates/browser/tilo-web-desktop, release, split into modules) that
# the room service's and the web entry's images serve and the browser end-to-end checks drive.
# Built in two checks of their own: `wasm` compiles the modules, keyed by what that compile reads
# and not by commit, so a change to the pages alone takes the last one; `bundle` assembles the
# pages around them for this commit (the bundle records it). The image contexts take the bundle
# as an output instead of each compiling every app to wasm again, and it builds beside the room
# service's native binaries rather than after them.
# usage: web-desktop-bundle.sh wasm OUT | web-desktop-bundle.sh bundle OUT WASM_DIR
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
usage='usage: web-desktop-bundle.sh wasm OUT | web-desktop-bundle.sh bundle OUT WASM_DIR'
part=${1:?$usage}
out=${2:?$usage}
cd "$repo_root"
export TILO_FONT_ASSET_CACHE=${CARGO_TARGET_DIR:-$repo_root/target}/font-artifacts
rm -rf "$out"
mkdir -p "$(dirname -- "$out")"
case $part in
  wasm)
    python3 tools/font-catalog.py root crates/sdk/ui-core/assets/fonts --scope fixtures --cache "$TILO_FONT_ASSET_CACHE" >/dev/null
    crates/browser/tilo-browser-runtime/scripts/build.sh "$out" --bundle desktop --wasm-only >/dev/null
    ;;
  bundle)
    wasm=${3:?$usage}
    moq=$scratch/moq-browser
    rm -rf "$moq" && cp -r scripts/moq-browser "$moq"
    TILO_MOQ_BROWSER_DIR=$moq crates/browser/tilo-browser-runtime/scripts/build.sh "$out" --bundle desktop --wasm-from "$wasm" >/dev/null
    ;;
  *) echo "$usage" >&2; exit 2 ;;
esac
