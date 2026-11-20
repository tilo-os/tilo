#!/usr/bin/env bash
# A newcomer's tour of the built web page against the Lounge's server, run
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
# With TILO_CLI_BIN the newcomer sets Tilo up through an agent running it. The bundle's own app
# list tells the tour which apps this page can open.
toured=0
TILO_BUNDLE=$bundle node "$here/integration/tour.mjs" "$site_origin" "$server_origin" "$work/tour" \
  ${TILO_CLI_BIN:+"$TILO_CLI_BIN" "$work/site.ca.crt"} \
  || toured=$?
if (( toured != 0 )); then
  echo 'FAIL: the tour stopped' >&2
  [[ ! -s ${log:-} ]] || { echo '--- the Lounge said:' >&2; tail -40 "$log" >&2; }
fi
# The pictures, in the log as well and last in it: a reviewer reads them from
# `t log page-tour`, which keeps a log's end, where downloading the outputs is not
# at hand. A tour that found problems keeps them too: a failed check stores no
# outputs, and the screens of what went wrong are the ones a reviewer needs.
if [[ ${TILO_TOUR_IN_LOG:-0} == 1 && -d $work/tour ]]; then
  echo '--- tour.tar.gz.b64'
  tar -C "$work/tour" -czf - --exclude="*.json" . | base64 -w 4000
  echo
  echo '--- end'
fi
exit "$toured"
