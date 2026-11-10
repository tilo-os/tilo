#!/usr/bin/env bash
# The built web page boots in headless Chromium against the Lounge's own
# relay, run directly (direct-lounge.sh): it arrives, joins the workspace,
# opens Notes, and says no exception or panic while it does
# (integration/page-boot.mjs). It is what a unit test with a fake runtime
# cannot see: an export the page calls by another name, storage read before
# the page loaded it, a std clock reached in wasm.
#
# usage: test-page-boot.sh BUNDLE WORK
# BUNDLE is the web desktop bundle (tools/ci/publish/web-desktop-bundle.sh);
# WORK is where it keeps what it found: the tab's console, a screenshot, the
# report, the site's and the Lounge's logs. Needs node, chromium, curl,
# TILO_RELAY_BIN and TILO_CRYPTO_BIN naming built programs, and TILO_FONTS_DIR
# the product's font catalog (the page-boot check hands it the
# lounge-binaries, lounge-gate-tools and image-lounge-context outputs).
set -euo pipefail
usage='usage: test-page-boot.sh BUNDLE WORK'
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
bundle=${1:?$usage}
work=${2:?$usage}
rm -rf "$work"
mkdir -p "$work"
# shellcheck source=direct-lounge.sh
. "$here/direct-lounge.sh"
trap stop_lounge EXIT
start_lounge "$bundle" "$work"

node "$here/integration/page-boot.mjs" "$site_origin" "$relay_origin" "$work/page-boot" \
  || lounge_fail 'the web page did not boot cleanly'
echo 'ok: the web page booted, joined the workspace and opened Notes without an exception or panic'
