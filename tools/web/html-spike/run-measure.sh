#!/usr/bin/env bash
# Runs measure.mjs against the bundle build-notes-web.sh made, in headless
# Chrome for Testing. Results and screenshots land in $CARGO_TARGET_DIR/htmlspike/measure.
set -euo pipefail
export TMPDIR=${TMPDIR:-$(mktemp -d)}
bash tools/ci/ensure-browser-dependencies.sh >/dev/null
if [[ ! -x ${CHROME:-} ]]; then
  npx --yes @puppeteer/browsers@2 install chrome@stable --path "$CARGO_TARGET_DIR/htmlspike/chrome" > "$CARGO_TARGET_DIR/htmlspike/chrome.txt"
  export CHROME=$(awk '{print $2}' "$CARGO_TARGET_DIR/htmlspike/chrome.txt" | tail -n 1)
fi
cp tools/web/html-spike/notes.html tools/web/html-spike/baseline.html crates/browser/tilo-browser-runtime/web/*.mjs "$CARGO_TARGET_DIR/htmlspike/bundle/"
rm -f "$CARGO_TARGET_DIR/htmlspike/bundle/"*.test.mjs
rm -rf "$CARGO_TARGET_DIR/htmlspike/measure"
node tools/web/html-spike/measure.mjs "$CARGO_TARGET_DIR/htmlspike/bundle" "$CARGO_TARGET_DIR/htmlspike/measure" "$@"
