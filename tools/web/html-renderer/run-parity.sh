#!/usr/bin/env bash
# Runs parity.mjs over the desktop bundle the runtime's build.sh made, in
# headless Chrome for Testing, or in Playwright's Firefox and WebKit
# (`--browser firefox --against <a Chromium run's directory>`). Screenshots,
# diffs and parity.json land in $CARGO_TARGET_DIR/htmlrenderer/parity, with
# fields.mjs's look at a sign-in form's fields.
set -euo pipefail
export TMPDIR=${TMPDIR:-$(mktemp -d)}
# Firefox and WebKit are Playwright's builds: the tools import it from
# PLAYWRIGHT, which `engines.sh install` puts beside the other build outputs.
export PLAYWRIGHT=${PLAYWRIGHT:-${CARGO_TARGET_DIR:-$(cd "$(dirname "$0")/../../.." && pwd)/target}/htmlrenderer/playwright/node_modules/playwright-core/index.mjs}
bash tools/ci/ensure-browser-dependencies.sh >/dev/null
if [[ ! -x ${CHROME:-} ]]; then
  npx --yes @puppeteer/browsers@2 install chrome@stable --path "$CARGO_TARGET_DIR/htmlrenderer/chrome" > "$CARGO_TARGET_DIR/htmlrenderer/chrome.txt"
  export CHROME=$(awk '{print $2}' "$CARGO_TARGET_DIR/htmlrenderer/chrome.txt" | tail -n 1)
fi
bundle=$CARGO_TARGET_DIR/htmlrenderer/desktop
cp tools/web/html-renderer/app.html tools/web/html-renderer/baseline.html crates/browser/tilo-browser-runtime/web/*.mjs "$bundle/"
rm -f "$bundle/"*.test.mjs
if [[ ${1:-} != --fields ]]; then rm -rf "$CARGO_TARGET_DIR/htmlrenderer/parity"; fi
# --fields runs only the sign-in fields; anything else goes to parity.mjs.
node tools/web/html-renderer/fields.mjs "$bundle" "$CARGO_TARGET_DIR/htmlrenderer/parity"
if [[ ${1:-} != --fields ]]; then node tools/web/html-renderer/parity.mjs "$bundle" "$CARGO_TARGET_DIR/htmlrenderer/parity" "$@"; fi
