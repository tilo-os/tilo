#!/usr/bin/env bash
# Standalone artifact. Canonical desktop/SDK packaging is a separate gate before enablement.
set -euo pipefail
cd "$(dirname "$0")/../../../.."
out=${1:-target/browser-compute}
cargo build --locked -p tilo-browser-compute --target wasm32-unknown-unknown --profile wasm-release
mkdir -p "$out/pkg/compute"
wasm-bindgen target/wasm32-unknown-unknown/wasm-release/tilo_browser_compute.wasm --target web --out-dir "$out/pkg/compute" --out-name tilo_browser_compute
cp crates/browser/tilo-browser-compute/web/compute-{worker,domains,protocol}.mjs "$out/"
