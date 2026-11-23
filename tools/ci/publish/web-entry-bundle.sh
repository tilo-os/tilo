#!/usr/bin/env bash
# The web entry's bundle (crates/core/tilo-browser-runtime, release), which the web entry's
# image serves and the browser checks drive: the site's pages and modules, and the module that
# gets a browser into a workspace. The apps are not in it — a page's app is the server's,
# rendered as HTML.
# Built in two checks of their own: `web-entry-wasm` compiles the module, keyed by what that
# compile reads and not by commit, so a change to the pages alone takes the last one;
# `web-entry-bundle` assembles the pages around it for this commit (the bundle records it). The
# image contexts take the bundle as an output instead of compiling it again, and it builds beside
# the server's native binaries rather than after them.
# usage: web-entry-bundle.sh wasm OUT [PROFILE] | web-entry-bundle.sh bundle OUT WASM_DIR [PROFILE]
# PROFILE is build.sh's: `release` (the default) is what ships; `check` is the same without the
# whole-program link, which the local tilo.sh stack serves (os/containers/tilo-sh/stack.sh).
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
usage='usage: web-entry-bundle.sh wasm OUT [PROFILE] | web-entry-bundle.sh bundle OUT WASM_DIR [PROFILE]'
part=${1:?$usage}
out=${2:?$usage}
cd "$repo_root"
export TILO_FONT_ASSET_CACHE=${CARGO_TARGET_DIR:-$repo_root/target}/font-artifacts
rm -rf "$out"
mkdir -p "$(dirname -- "$out")"
case $part in
  wasm)
    python3 tools/font-catalog.py root crates/ui/ui-core/assets/fonts --scope fixtures --cache "$TILO_FONT_ASSET_CACHE" >/dev/null
    crates/core/tilo-browser-runtime/scripts/build.sh "$out" --profile "${3:-release}" --wasm-only >/dev/null
    ;;
  bundle)
    wasm=${3:?$usage}
    crates/core/tilo-browser-runtime/scripts/build.sh "$out" --profile "${4:-release}" --wasm-from "$wasm" >/dev/null
    ;;
  *) echo "$usage" >&2; exit 2 ;;
esac
