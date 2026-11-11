#!/usr/bin/env bash
# Runs measure.mjs against the bundle build-notes-web.sh made, in headless
# Chrome for Testing. Results and screenshots land in $CARGO_TARGET_DIR/htmlrenderer/measure.
set -euo pipefail
export TMPDIR=${TMPDIR:-$(mktemp -d)}
bash tools/ci/ensure-browser-dependencies.sh >/dev/null
if [[ ! -x ${CHROME:-} ]]; then
  npx --yes @puppeteer/browsers@2 install chrome@stable --path "$CARGO_TARGET_DIR/htmlrenderer/chrome" > "$CARGO_TARGET_DIR/htmlrenderer/chrome.txt"
  export CHROME=$(awk '{print $2}' "$CARGO_TARGET_DIR/htmlrenderer/chrome.txt" | tail -n 1)
fi
cp tools/web/html-renderer/app.html tools/web/html-renderer/baseline.html crates/browser/tilo-browser-runtime/web/*.mjs "$CARGO_TARGET_DIR/htmlrenderer/bundle/"
rm -f "$CARGO_TARGET_DIR/htmlrenderer/bundle/"*.test.mjs
rm -rf "$CARGO_TARGET_DIR/htmlrenderer/measure"
node tools/web/html-renderer/measure.mjs "$CARGO_TARGET_DIR/htmlrenderer/bundle" "$CARGO_TARGET_DIR/htmlrenderer/measure" "$@"
