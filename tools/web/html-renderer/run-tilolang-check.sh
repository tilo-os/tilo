#!/usr/bin/env bash
# Runs tilolang-check.mjs over build-tilolang-web.sh's output in headless
# Chrome for Testing, where the bundle was built (the builder, through
# `rcargo --fetch htmlrenderer/tilolang-check exec`). Screenshots and
# check.json land in $CARGO_TARGET_DIR/htmlrenderer/tilolang-check.
set -euo pipefail
export TMPDIR=${TMPDIR:-$(mktemp -d)}
bash tools/ci/ensure-browser-dependencies.sh >/dev/null
if [[ ! -x ${CHROME:-} ]]; then
  npx --yes @puppeteer/browsers@2 install chrome@stable --path "$CARGO_TARGET_DIR/htmlrenderer/chrome" > "$CARGO_TARGET_DIR/htmlrenderer/chrome.txt"
  export CHROME=$(awk '{print $2}' "$CARGO_TARGET_DIR/htmlrenderer/chrome.txt" | tail -n 1)
fi
out=$CARGO_TARGET_DIR/htmlrenderer/tilolang-check
rm -rf "$out"
node tools/web/html-renderer/tilolang-check.mjs "$CARGO_TARGET_DIR/htmlrenderer/tilolang" "$out" "$@"
