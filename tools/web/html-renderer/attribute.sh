#!/usr/bin/env bash
# Attribute the runtime module's code to crates: an unstripped release build
# of the bare runtime, read by attribute.py.
set -euo pipefail
out=$CARGO_TARGET_DIR/htmlrenderer/attribution
mkdir -p "$out"
cargo build --locked -p tilo-browser-runtime --target wasm32-unknown-unknown --profile wasm-release \
  --config 'profile.wasm-release.strip="none"' --config 'profile.wasm-release.debug=false'
python3 tools/web/html-renderer/attribute.py "$CARGO_TARGET_DIR/wasm32-unknown-unknown/wasm-release/tilo_browser_runtime.wasm" --top 60 > "$out/attribution.json"
cat "$out/attribution.json" | head -c 4000
