#!/usr/bin/env bash
# A page's app request held past the time the server waits for it
# (integration/page-reopen.mjs): the site's entry makes the browser a device,
# the page opens, a note is typed in, the app workers are stopped for longer
# than the server waits, more is typed while they are, and the page must show
# the note again with what was typed and go on working. What each step saw,
# and a picture of it, is kept for a person to read.
#
# usage: test-page-reopen.sh BUNDLE WORK
set -euo pipefail
usage='usage: test-page-reopen.sh BUNDLE WORK'
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
bundle=${1:?$usage}
work=${2:?$usage}
rm -rf "$work"
mkdir -p "$work"
# shellcheck source=direct-lounge.sh
. "$here/direct-lounge.sh"
trap stop_lounge EXIT
start_lounge "$bundle" "$work"
# The pictures, in the log as well: a reviewer reads them from `t log
# page-reopen` where downloading the outputs is not at hand, and a walk that
# stopped partway still says what it saw.
keep_pictures() {
  [[ ${TILO_PAGE_REOPEN_IN_LOG:-0} == 1 ]] || return 0
  echo '--- page-reopen.tar.gz.b64'
  tar -C "$work/page" -czf - . 2>/dev/null | base64 -w 4000
  echo
  echo '--- end'
}
trap 'keep_pictures; stop_lounge' EXIT
node "$here/integration/page-reopen.mjs" "$site_origin" "$server_origin" "$workspace" "$work/page" \
  || lounge_fail 'the reopened page was not walked through'
