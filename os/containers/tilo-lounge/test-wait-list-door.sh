#!/usr/bin/env bash
# The front door of a workspace that takes people by invitation, in a real
# browser: the wait list is what a stranger sees (not the setup screen), an
# address left there is confirmed through the mail the door sends to a local
# sink, and the tab stays on the wait list when it reloads. It is what a unit
# test with a fake runtime cannot see: the whole way from the page's refusal
# through the relay's door, the workspace's wait list and the mail out.
#
# usage: test-wait-list-door.sh BUNDLE WORK
# BUNDLE is the web desktop bundle (tools/ci/publish/web-desktop-bundle.sh);
# WORK keeps what it found: the screenshots, the tab's console, the report,
# the mails the door sent and the Lounge's log. Needs node, chromium, curl,
# TILO_RELAY_BIN and TILO_CRYPTO_BIN naming built programs, and TILO_FONTS_DIR
# the product's font catalog.
set -euo pipefail
usage='usage: test-wait-list-door.sh BUNDLE WORK'
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
bundle=${1:?$usage}
work=${2:?$usage}
rm -rf "$work"
mkdir -p "$work"
# shellcheck source=direct-lounge.sh
. "$here/direct-lounge.sh"
trap stop_lounge EXIT

# The mail the door sends goes to a sink this run keeps, never to a service.
# Its address is fixed like the Lounge's own (direct-lounge.sh).
sink_port=19790
export LOUNGE_ADMISSION=invite
export LOUNGE_WAIT_LIST_MAX=50
export LOUNGE_CONTACT='hello@tilo.example'
export LOUNGE_MAIL_FROM='Tilo <hello@tilo.example>'
export LOUNGE_RESEND_KEY_FILE="$work/resend-key"
export LOUNGE_RESEND_URL="http://127.0.0.1:$sink_port/emails"
printf 're_test_key_this_run_keeps\n' > "$LOUNGE_RESEND_KEY_FILE"

mkdir -p "$work/mail"
node "$here/integration/mail-sink.mjs" "$work/mail" "$sink_port" > "$work/mail-sink.log" 2>&1 &
lounge_pids+=($!)
for _ in $(seq 300); do
  grep -q '"url"' "$work/mail-sink.log" && break
  sleep 0.1
done
grep -q '"url"' "$work/mail-sink.log" || lounge_fail "the mail sink did not start: $(cat "$work/mail-sink.log")"

start_lounge "$bundle" "$work"

node "$here/integration/wait-list-door.mjs" "$site_origin" "$work/mail" "$work/wait-list-door" \
  || lounge_fail 'a stranger at the invitation-only door was not kept on its wait list'
echo 'ok: a stranger saw the wait list, left an address, confirmed it from the mail, and stayed on the list'
