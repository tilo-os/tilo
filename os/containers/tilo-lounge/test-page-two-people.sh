#!/usr/bin/env bash
# Two people on the web page, each in their own browser, against the Lounge's
# own programs run directly (direct-lounge.sh): the gate's people step,
# brief. They chat, see each other here, call each other and read each
# other's name on the call's card, and pan the strip and swipe over Sheets
# among a few apps. The Lounge gate (test-integration.sh) runs the whole
# step against the built image.
#
# usage: test-page-two-people.sh BUNDLE WORK
# BUNDLE is the web desktop bundle (tools/ci/publish/web-desktop-bundle.sh);
# WORK is where it keeps what it found: each page's snapshot and console, the
# report, the site's and the Lounge's logs. Needs node, chromium, curl,
# TILO_RELAY_BIN and TILO_CRYPTO_BIN naming built programs, and TILO_FONTS_DIR
# the product's font catalog.
set -euo pipefail
usage='usage: test-page-two-people.sh BUNDLE WORK'
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
bundle=${1:?$usage}
work=${2:?$usage}
rm -rf "$work"
mkdir -p "$work"
# shellcheck source=direct-lounge.sh
. "$here/direct-lounge.sh"
trap stop_lounge EXIT
start_lounge "$bundle" "$work"

TILO_PEOPLE_BRIEF=1 node "$here/integration/people.mjs" "$site_origin" "$relay_origin" "$relay_origin" \
  "$work/people" "$roles" \
  || lounge_fail 'two people did not meet in the Lounge'
echo 'ok: two people chatted, saw each other here, called each other by name and panned the strip'
