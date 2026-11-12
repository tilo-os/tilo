#!/usr/bin/env bash
# The gate's shared-link step on its own, in minutes: a document copied from
# the bar's Share opens as that one document, live, on a phone-sized page, and
# a link to an agent run opens Console on that run — against the Lounge's own
# programs run directly, as page-two-people runs them (direct-lounge.sh).
# The Lounge gate (test-integration.sh) still runs the same step against the
# built image before anything is published.
#
# usage: test-page-share-link.sh BUNDLE WORK
# BUNDLE is the web desktop bundle (tools/ci/publish/web-desktop-bundle.sh);
# WORK is where it keeps what it found: each page's snapshot and console, the
# report, the site's and the Lounge's logs. Needs node, chromium, curl,
# TILO_RELAY_BIN and TILO_CRYPTO_BIN naming built programs, and TILO_FONTS_DIR
# the product's font catalog.
set -euo pipefail
usage='usage: test-page-share-link.sh BUNDLE WORK'
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
bundle=${1:?$usage}
work=${2:?$usage}
rm -rf "$work"
mkdir -p "$work"
# shellcheck source=direct-lounge.sh
. "$here/direct-lounge.sh"
trap stop_lounge EXIT
start_lounge "$bundle" "$work"

node "$here/integration/share-link.mjs" "$site_origin" "$relay_origin" "$room" "$work/share-link" \
  || lounge_fail 'a shared link did not open the same document live for the second person'
echo 'ok: a shared link opened the one document full tab, live, for the second person'
