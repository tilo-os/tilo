#!/usr/bin/env bash
# Type-check what the browser builds for wasm32-unknown-unknown, each with the
# packages and features of the script that builds it (WEB_RUNTIME.md). A check
# of each crate with its default features never compiles the web core as a
# dynamic-linking main module with its app collection, nor the Cloud frontend.
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

# build-web-core.sh and build-web-app.sh: the core with its app collection.
check "web core" -p tilo-browser-runtime -p ebook -p word-processor -p chat --features tilo-browser-runtime/dylink
# build-web-core.sh --features cloud: the same core with the certified peer.
check "web core with peer" -p tilo-browser-runtime -p ebook -p word-processor -p chat \
  --features tilo-browser-runtime/dylink,tilo-browser-runtime/cloud
# scripts/build.sh: the Cloud frontend's monolithic module.
check "cloud frontend" -p tilo-browser-runtime --features cloud,static-apps
# web-registry-client/build-verifier.sh.
check "registry verifier" -p registry-web-bindgen
