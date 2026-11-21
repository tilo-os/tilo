#!/usr/bin/env bash
# The size of the desktop web bundle, as the sdk-web-size check builds it:
# the start module every page loads, which a change behind `dom` must not
# grow. Writes the listing to $CARGO_TARGET_DIR/htmlrenderer/size.
set -euo pipefail
cargo=${CARGO:-cargo}
"$cargo" build --locked -p tilo-app-cli --bin tilo-app
app=$CARGO_TARGET_DIR/debug/tilo-app
export CARGO_TARGET_DIR=$CARGO_TARGET_DIR/release-web
"$app" build --target web --bundle desktop --release apps/calculator
bundle=$CARGO_TARGET_DIR/tilo-app/web/calculator/bundle
mkdir -p "$CARGO_TARGET_DIR/../htmlrenderer/size"
ls -l "$bundle/pkg" | tee "$CARGO_TARGET_DIR/../htmlrenderer/size/pkg.txt"
echo "the html renderer measure has no budget since the web desktop went"
