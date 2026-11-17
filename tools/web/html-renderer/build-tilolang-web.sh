#!/usr/bin/env bash
# Builds a tilolang app for the web: the tilolang runtime's web bundle
# (`tilo-app build --target web --bundle app apps/tilolang`) with the app's
# payload beside it under tl/<app-id>/, which the page installs
# (`installTilolangPackage`) before it mounts the app, and app.html and
# tilolang.html to open it with. Runs where cargo builds (the builder, through
# `$CARGO exec`); the output lands in $CARGO_TARGET_DIR/htmlrenderer/tilolang.
#
# usage: build-tilolang-web.sh [APP_DIR] [--search DIR]...   (default: apps/tl/gallery --search apps/tl)
set -euo pipefail
app_dir=${1:-apps/tl/gallery}
shift || true
search=("$@")
if [[ ${#search[@]} -eq 0 ]]; then search=(--search apps/tl); fi
out=$CARGO_TARGET_DIR/htmlrenderer/tilolang
cargo build --locked -p tilo-app-cli --bin tilo-app -p tilolang-cli --bin tilolang
"$CARGO_TARGET_DIR/debug/tilo-app" build --target web --bundle app apps/tilolang
rm -rf "$out"
mkdir -p "$out"
cp -r "$CARGO_TARGET_DIR/tilo-app/web/tilolang-app/bundle/." "$out/"
packages=$(mktemp -d)
trap 'rm -rf "$packages"' EXIT
"$CARGO_TARGET_DIR/debug/tilolang" build "$app_dir" "${search[@]}" --out "$packages" \
  --identity "$packages/publisher.json"
for staged in "$packages"/*/; do
  [[ -f $staged/manifest.toml ]] || continue
  id=$(sed -n 's/^id *= *"\(.*\)"/\1/p' "$staged/manifest.toml" | head -n 1)
  mkdir -p "$out/tl/$id"
  cp -r "$staged/payload/." "$out/tl/$id/"
done
cp tools/web/html-renderer/app.html tools/web/html-renderer/tilolang.html "$out/"
ls "$out/tl"
