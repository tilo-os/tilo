#!/usr/bin/env bash
# Builds the desktop web bundle (release), every app the desktop links, with
# the page that mounts one of them alone. Lands in
# $CARGO_TARGET_DIR/htmlrenderer/desktop.
set -euo pipefail
out=$CARGO_TARGET_DIR/htmlrenderer
mkdir -p "$out"
cargo build --locked -p tilo-app-cli --bin tilo-app
app=$CARGO_TARGET_DIR/debug/tilo-app
CARGO_TARGET_DIR=$CARGO_TARGET_DIR/release-web "$app" build --target web --bundle desktop --release apps/calculator
rm -rf "$out/desktop"
cp -r "$CARGO_TARGET_DIR/release-web/tilo-app/web/calculator/bundle" "$out/desktop"
cp crates/browser/tilo-web-desktop/apps.json "$out/desktop/apps.json"
ls -la "$out/desktop/pkg"
