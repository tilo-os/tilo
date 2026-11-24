#!/usr/bin/env bash
# Two people of one workspace in a voice call, each in a headless Chromium of
# their own (integration/page-call.mjs): they arrive at the Lounge's web entry
# as people, name themselves from the workspace's own search, one makes a voice
# channel there, and both open it in Chat, join its call, unmute and turn their
# camera on. The call is proved by its own counters on each page — voice frames
# heard, pictures sent and received — and by the other person's tile drawing
# picture after picture. What each step saw is kept in the report; nothing is
# judged by a screenshot.
#
# usage: test-page-call.sh BUNDLE WORK
set -euo pipefail
usage='usage: test-page-call.sh BUNDLE WORK'
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
bundle=${1:?$usage}
work=${2:?$usage}
rm -rf "$work"
mkdir -p "$work"
# shellcheck source=direct-lounge.sh
. "$here/direct-lounge.sh"
trap stop_lounge EXIT
# What the server said of the page's asks, when a call step did not land: a
# page that shows a call no one is in says why nowhere else.
export LOUNGE_LOG=${LOUNGE_LOG:-tilo_agent_session=debug,tilo_server=info,tilo_workspace_service::agents=info,warn}
start_lounge "$bundle" "$work"
keep_page_lines() {
  [[ -s ${log:-} ]] || return 0
  echo '--- page acts, opens and refusals:'
  { grep -i "page's act\|found no page held\|found its record not ready\|a page was refused\|no page is open\|page was closed\|call" "$log" || true; } \
    | { grep -vi 'acted=false' || true; } | tail -80
}
trap 'keep_page_lines; stop_lounge' EXIT
# The browsers the driver starts close with it, and its supervisor closes theirs
# with it; the timeout bounds a walk that hangs so neither outlives the check.
timeout --kill-after=30 1500 node "$here/integration/page-call.mjs" "$site_origin" "$server_origin" "$workspace" "$work/page" \
  || lounge_fail 'the call the two pages carried was not what the walk proved'
