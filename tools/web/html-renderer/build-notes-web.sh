#!/usr/bin/env bash
# Builds the single-app Notes web bundle (release) with the measurement pages,
# then an unstripped build of the bare runtime module to attribute its code to
# crates. Outputs land in $CARGO_TARGET_DIR/htmlrenderer/.
set -euo pipefail
out=$CARGO_TARGET_DIR/htmlrenderer
mkdir -p "$out"
cargo build --locked -p tilo-app-cli --bin tilo-app
"$CARGO_TARGET_DIR/debug/tilo-app" build --target web --bundle app --release apps/notes
rm -rf "$out/bundle"
cp -r "$CARGO_TARGET_DIR/tilo-app/web/notes/bundle" "$out/bundle"
cp tools/web/html-renderer/notes.html "$out/bundle/notes.html"
cp tools/web/html-renderer/baseline.html "$out/bundle/baseline.html"
ls -la "$out/bundle/pkg"
if [[ ${ATTRIBUTE:-1} == 1 ]]; then
  cargo build --locked -p tilo-browser-runtime --target wasm32-unknown-unknown --profile wasm-release \
    --config 'profile.wasm-release.strip="none"' --config 'profile.wasm-release.debug=false'
  python3 tools/web/html-renderer/attribute.py "$CARGO_TARGET_DIR/wasm32-unknown-unknown/wasm-release/tilo_browser_runtime.wasm" --top 40 > "$out/attribution.json"
  head -c 3000 "$out/attribution.json"
fi
