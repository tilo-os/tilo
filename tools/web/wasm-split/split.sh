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
set -euo pipefail

usage='usage: split.sh ORIGINAL PKG_DIR | split.sh --unsplit PKG_DIR'
original=${1:?$usage}
pkg=${2:?$usage}
tool=$(cd "$(dirname "$0")" && pwd)
target_dir=${CARGO_TARGET_DIR:-$tool/../../../target}/wasm-split-tool
cargo build --quiet --release --locked --manifest-path "$tool/Cargo.toml" --target-dir "$target_dir"
if [[ $original == --unsplit ]]; then
  exec "$target_dir/release/wasm-split" --unsplit "$pkg/tilo_browser_runtime_bg.wasm" "$pkg"
fi
# Beside the output, not in /tmp: the module is large and /tmp may be memory.
bindgened=$pkg/.bindgened.wasm
trap 'rm -f "$bindgened"' EXIT
mv "$pkg/tilo_browser_runtime_bg.wasm" "$bindgened"
"$target_dir/release/wasm-split" "$original" "$bindgened" "$pkg" tilo_browser_runtime
