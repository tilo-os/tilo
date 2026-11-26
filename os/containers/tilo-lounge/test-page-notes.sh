#!/usr/bin/env bash
# A person walks a workspace's Notes as a web page against the Lounge's
# server, started as page-boot starts its own (integration/page-notes.mjs): the
# site's entry makes the browser a device, the page opens, a note is opened and
# typed in, Back and Forward are given, the page is reloaded at the note's
# address, looked at at a phone's width, and opened from a second browser
# profile. What each step saw, and a picture of it, is kept for a person to
# read.
#
# usage: test-page-notes.sh BUNDLE WORK
set -euo pipefail
usage='usage: test-page-notes.sh BUNDLE WORK'
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
bundle=${1:?$usage}
work=${2:?$usage}
rm -rf "$work"
mkdir -p "$work"
# shellcheck source=direct-lounge.sh
. "$here/direct-lounge.sh"
trap stop_lounge EXIT
# Why the server did or did not act on what the page asked: a page that shows
# what a person typed but keeps none of it says why nowhere else.
export LOUNGE_LOG=tilo_agent_session=debug,tilo_server=info,tilo_app_host::agents=info,warn
start_lounge "$bundle" "$work"
# What the site beside this page loads first, for the page's own numbers to be
# read against: the page fetches none of it.
module=$bundle/pkg/tilo_browser_runtime_bg.wasm
echo "== the site's start module: $(stat -c%s "$module") B raw, $(gzip -9 -c "$module" | wc -c) B gzip -9, $(brotli -q 11 -c "$module" | wc -c) B brotli -q 11"
# The pictures, in the log as well: a reviewer reads them from `t log
# page-notes` where downloading the outputs is not at hand, and a walk that
# stopped partway still says what it saw.
keep_pictures() {
  [[ ${TILO_PAGE_NOTES_IN_LOG:-0} == 1 ]] || return 0
  echo '--- page-notes.tar.gz.b64'
  tar -C "$work/page" -czf - . 2>/dev/null | base64 -w 4000
  echo
  echo '--- end'
}
# What the server said of the page's asks: a page that shows what a person
# typed but keeps none of it says why nowhere else.
keep_page_lines() {
  [[ -s ${log:-} ]] || return 0
  echo '--- page acts, opens and refusals:'
  # A walk whose acts all went through leaves no such line, which is no failure.
  { grep -i "page's act\|found no page held\|found its record not ready\|a page was refused\|no page is open\|page was closed" "$log" || true; } \
    | { grep -vi 'acted=false' || true; } | tail -60
}
trap 'keep_pictures; keep_page_lines; stop_lounge' EXIT
node "$here/integration/page-notes.mjs" "$site_origin" "$server_origin" "$workspace" "$work/page" \
  || lounge_fail 'the page walk did not finish'
