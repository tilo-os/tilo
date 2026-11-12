#!/usr/bin/env bash
# Boots the built page in a browser the parity runs only compare in — Safari's
# WebKit and Firefox, through Playwright's builds (`engines.mjs`) — and says
# whether it mounts, draws and throws nothing, in a normal window and in a
# private one. A page that starts in Chrome and not here locks out every Mac
# and iPhone visitor, which is what this check is for.
#
# usage: tools/web/html-renderer/boot.sh BUNDLE [--browser webkit|firefox|chromium] [--app ID] [--out DIR]
#   BUNDLE is the desktop bundle (tools/web/html-renderer/build-desktop-web.sh).
#   Results land in $CARGO_TARGET_DIR/htmlrenderer/boot.
#
# WebKit is the browser to run: it is the one Safari shares, and the one whose
# session decides whether a page has a file store. `engines.sh install` puts
# Playwright beside the other build outputs; `engines.sh check webkit` says
# whether this host can start that build at all.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out=${CARGO_TARGET_DIR:-$(cd "$here/../../.." && pwd)/target}/htmlrenderer
export PLAYWRIGHT=${PLAYWRIGHT:-$out/playwright/node_modules/playwright-core/index.mjs}
bundle=${1:?usage: boot.sh BUNDLE [--browser webkit|firefox|chromium] [--app ID] [--out DIR]}
shift
work=$out/boot
# `--out` is this script's own; every other argument goes to check-boot.mjs.
arguments=()
while [[ $# -gt 0 ]]; do
  case $1 in
    --out) work=${2:?--out needs a directory}; shift 2 ;;
    *) arguments+=("$1"); shift ;;
  esac
done
# Chromium is Chrome for Testing, which the bundle's page boots in too.
if [[ ! -x ${CHROME:-} ]]; then
  CHROME=$(ls -d "$HOME"/.cache/ms-playwright/chromium-*/chrome-linux*/chrome 2>/dev/null | sort -V | tail -n 1 || true)
  export CHROME
fi
rm -rf "$work"
node "$here/check-boot.mjs" "$bundle" "$work" ${arguments[@]+"${arguments[@]}"}
