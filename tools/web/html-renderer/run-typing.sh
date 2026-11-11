#!/usr/bin/env bash
# Runs typing.mjs over a built bundle in headless Chrome: what one key costs,
# against the plain-DOM page in the same run.
#
# usage: tools/web/html-renderer/run-typing.sh [--keys N] [--trace]
#   BUNDLE=… the bundle to measure (default: the desktop bundle build.sh made)
set -euo pipefail
export TMPDIR=${TMPDIR:-$(mktemp -d)}
bundle=${BUNDLE:-$CARGO_TARGET_DIR/htmlrenderer/desktop}
cp tools/web/html-renderer/app.html tools/web/html-renderer/baseline.html crates/browser/tilo-browser-runtime/web/*.mjs "$bundle/"
rm -f "$bundle/"*.test.mjs
node tools/web/html-renderer/typing.mjs "$bundle" "$CARGO_TARGET_DIR/htmlrenderer/typing" "$@"
