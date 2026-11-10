#!/usr/bin/env bash
# Split the browser desktop bundle's module into a start module and the
# modules its apps load on demand (crates/browser/tilo-web-desktop/src/split.rs).
#
# usage: split.sh ORIGINAL PKG_DIR
#        split.sh --unsplit PKG_DIR
#
# ORIGINAL is the linked tilo_web_desktop.wasm, built with its symbol table
# (Cargo profile `wasm-split`, `dev` or `wasm-dev`) and `--emit-relocs` (the
# crate's build.rs). PKG_DIR holds wasm-bindgen's output for it, made with
# `--keep-lld-exports --no-demangle` under the runtime's module name; its
# tilo_browser_runtime_bg.wasm is replaced by the start module, beside which
# the app modules, `__wasm_split.js` and `split.json` are written. Both
# scripts/build.sh and `tilo-app build --bundle desktop` run this, so the
# page and the SDK's bundle are split the same way.
#
# `--unsplit` leaves the module whole and writes only a `__wasm_split.js`
# that answers each split point from it: the quick development bundle
# (`build.sh --no-split`), which skips the split's time.
#
# The tool compiles with `cargo`, or with the cargo `$CARGO` names; its binary
# is read from Cargo's own artifact report, so a cargo that compiles elsewhere
# and brings artifacts back is followed wherever it puts them.
set -euo pipefail

usage='usage: split.sh ORIGINAL PKG_DIR | split.sh --unsplit PKG_DIR'
original=${1:?$usage}
pkg=${2:?$usage}
tool=$(cd "$(dirname "$0")" && pwd)
cargo=${CARGO:-cargo}
target_dir=${CARGO_TARGET_DIR:-$tool/../../../target}/wasm-split-tool
# Built from the tool's own directory, so a cargo that compiles in another
# checkout gets a path it has: the tool is its own workspace.
splitter=$(cd "$tool" && "$cargo" build --quiet --release --locked \
  --target-dir "$target_dir" --message-format=json-render-diagnostics |
  python3 -c 'import json, sys
for line in sys.stdin:
    if not line.lstrip().startswith("{"):
        continue
    message = json.loads(line)
    if message.get("reason") == "compiler-artifact" and message["target"]["name"] == "wasm-split" \
            and message.get("executable"):
        print(message["executable"])' | tail -n 1)
[[ -x $splitter ]] || { echo 'wasm-split was not built' >&2; exit 2; }
if [[ $original == --unsplit ]]; then
  exec "$splitter" --unsplit "$pkg/tilo_browser_runtime_bg.wasm" "$pkg"
fi
# Beside the output, not in /tmp: the module is large and /tmp may be memory.
bindgened=$pkg/.bindgened.wasm
trap 'rm -f "$bindgened"' EXIT
mv "$pkg/tilo_browser_runtime_bg.wasm" "$bindgened"
"$splitter" "$original" "$bindgened" "$pkg" tilo_browser_runtime
