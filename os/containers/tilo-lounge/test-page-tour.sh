#!/usr/bin/env bash
# A newcomer's tour of the built web page against the Lounge's relay, run
# directly as page-boot runs it (integration/tour.mjs): every screen a
# first-time visitor meets, in the light and dark appearances and at a phone's
# width, kept as pictures for a person to review.
#
# usage: test-page-tour.sh BUNDLE WORK
set -euo pipefail
usage='usage: test-page-tour.sh BUNDLE WORK'
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
bundle=${1:?$usage}
work=${2:?$usage}
rm -rf "$work"
mkdir -p "$work"
# shellcheck source=direct-lounge.sh
. "$here/direct-lounge.sh"
trap stop_lounge EXIT
start_lounge "$bundle" "$work"
# With TILO_CLI_BIN the newcomer sets Tilo up through an agent running it.
node "$here/integration/tour.mjs" "$site_origin" "$relay_origin" "$work/tour" \
  ${TILO_CLI_BIN:+"$TILO_CLI_BIN" "$work/site.ca.crt"} \
  || lounge_fail 'the tour stopped'
# The pictures, in the log as well: a reviewer reads them from `t log page-tour`
# where downloading the outputs is not at hand.
if [[ ${TILO_TOUR_IN_LOG:-0} == 1 ]]; then
  echo '--- tour.tar.gz.b64'
  tar -C "$work/tour" -czf - --exclude="*.json" . | base64 -w 4000
  echo
  echo '--- end'
fi
