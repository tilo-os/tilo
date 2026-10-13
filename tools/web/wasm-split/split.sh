#!/usr/bin/env bash
# Split the browser desktop bundle's module into a start module and the
# modules its apps load on demand (crates/browser/tilo-web-desktop/src/split.rs).
#
# usage: split.sh ORIGINAL PKG_DIR
#
# ORIGINAL is the linked tilo_web_desktop.wasm, built with its symbol table
# (Cargo profile `wasm-split`, or `dev`) and `--emit-relocs` (the crate's
# build.rs). PKG_DIR holds wasm-bindgen's output for it, made with
# `--keep-lld-exports --no-demangle` under the runtime's module name; its
# tilo_browser_runtime_bg.wasm is replaced by the start module, beside which
# the app modules, `__wasm_split.js` and `split.json` are written. Both
# scripts/build.sh and `tilo-app build --bundle desktop` run this, so the
# page and the SDK's bundle are split the same way.
set -euo pipefail

original=${1:?usage: split.sh ORIGINAL PKG_DIR}
pkg=${2:?usage: split.sh ORIGINAL PKG_DIR}
tool=$(cd "$(dirname "$0")" && pwd)
target_dir=${CARGO_TARGET_DIR:-$tool/../../../target}/wasm-split-tool
cargo build --quiet --release --locked --manifest-path "$tool/Cargo.toml" --target-dir "$target_dir"
# Beside the output, not in /tmp: the module is large and /tmp may be memory.
bindgened=$pkg/.bindgened.wasm
trap 'rm -f "$bindgened"' EXIT
mv "$pkg/tilo_browser_runtime_bg.wasm" "$bindgened"
"$target_dir/release/wasm-split" "$original" "$bindgened" "$pkg" tilo_browser_runtime
