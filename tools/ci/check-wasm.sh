#!/usr/bin/env bash
# Type-check what the browser builds for wasm32-unknown-unknown, each with the
# packages and features of the script that builds it
# (crates/browser/tilo-browser-runtime/scripts/build.sh). A check of each crate
# alone never compiles the page runtime with every web app linked into it.
#
#   tools/ci/check-wasm.sh
#
# check.yml's `wasm` job runs it after checking the selected browser crates. A
# type check, not a build: the link and the web runtime's post-processing need
# wasm-bindgen and binaryen.
set -euo pipefail
cd "$(dirname "$0")/../.."

target=wasm32-unknown-unknown
rustup target add "$target"

check() {
  local label=$1
  shift
  bash tools/ci/measure.sh "wasm32 check: $label" -- \
    bash scripts/ci-run.sh "cargo check (wasm32, $label)" -- \
    cargo check --locked --target "$target" --lib "$@"
}

# scripts/build.sh desktop: the page runtime with every web app registered.
check "web desktop" -p tilo-web-desktop
# scripts/build.sh runtime: the page runtime an app bundle links.
check "page runtime" -p tilo-browser-runtime

# Private image-only worker kernel.
check "image compute worker" -p tilo-browser-compute
