#!/usr/bin/env bash
set -euo pipefail
out=$CARGO_TARGET_DIR/htmlrenderer/attribution
mkdir -p "$out"
python3 tools/web/html-renderer/attribute.py "$CARGO_TARGET_DIR/wasm32-unknown-unknown/wasm-release/tilo_browser_runtime.wasm" --top 60 > "$out/attribution.json"
