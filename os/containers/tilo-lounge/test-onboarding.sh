#!/usr/bin/env bash
# A stranger's way into tilo.sh, end to end, on one host this run keeps: its
# door takes people by invitation and keeps a wait list whose mails go to a
# sink here, the person who runs it is already in it, and one stranger walks
# the whole way — the wait list, the confirmation mail, the invitation a
# person of the host's sends from the workspace's own wait list, the link it
# mails, the workspace of their own they land in, their passkey, and a second
# workspace they make. Then a visitor with no invitation, who still cannot get
# in.
#
# The host starts as a deployment's first start does: its door open, so the
# people who run it are in it before it closes. This run declares its operator
# an owner (the roles file a deployment mounts), and then closes the door and
# turns the wait list on (the deployment's own steps: the same host, started
# again on the volume it kept), which is what a launch's last step is.
#
# usage: test-onboarding.sh BUNDLE WORK
# BUNDLE is the web desktop bundle (tools/ci/publish/web-desktop-bundle.sh);
# WORK keeps what it found: the screenshots and consoles of every page, the
# report, the mails the door sent, and the host's log. Needs node, chromium,
# curl, a built tilo-relay and tilo-crypto and the product's font catalog
# (TILO_RELAY_BIN, TILO_CRYPTO_BIN, TILO_FONTS_DIR), and the tilo command an
# agent runs (TILO_CLI_BIN).
set -euo pipefail
usage='usage: test-onboarding.sh BUNDLE WORK'
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
bundle=${1:?$usage}
out=${2:?$usage}
tilo_bin=${TILO_CLI_BIN:-}
[[ -x $tilo_bin ]] || { echo 'TILO_CLI_BIN names no tilo command (an agent runs it here)' >&2; exit 2; }
rm -rf "$out"
mkdir -p "$out"
# The run's own files stay out of the outputs: what is kept for people is the
# report, the screenshots, the mails and the host's log.
work=$(mktemp -d)
# shellcheck source=direct-lounge.sh
. "$here/direct-lounge.sh"
trap 'stop_lounge; rm -rf "$work"' EXIT

# This flow's own addresses, beside the other flows' and this machine's: the
# Lounge's ports from LOUNGE_PORT_BASE, the mail sink after them.
export LOUNGE_PORT_BASE=${LOUNGE_PORT_BASE:-19810}
# The site by name, as tilo.sh is: the stranger keeps themselves with a
# passkey, which belongs to a site's name and never to an address.
export LOUNGE_SITE_NAME=localhost
sink_port=$((LOUNGE_PORT_BASE + 10))
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

# The host as a deployment first starts it: its door open, so the people who
# run it are in it before it closes.
export LOUNGE_ADMISSION=open
export LOUNGE_WAIT_LIST_MAX=
start_lounge "$bundle" "$work"
roles=$work/volume/roles.json

# The stranger's story, driven in real browsers. It asks for the door to close
# once the operator is in and declared: this script is the deployment, so it
# is the one that closes it.
node "$here/integration/onboarding.mjs" "$site_origin" "$relay_origin" "$work" "$tilo_bin" \
  "$work/site.ca.crt" "$roles" &
driver=$!
for _ in $(seq 2400); do
  [[ -e $work/close-the-door ]] && break
  kill -0 "$driver" 2>/dev/null || break
  sleep 0.1
done
if [[ -e $work/close-the-door ]]; then
  export LOUNGE_ADMISSION=invite
  export LOUNGE_WAIT_LIST_MAX=50
  restart_lounge
  : > "$work/door-closed"
fi
status=0
wait "$driver" || status=$?

mkdir -p "$out/mail"
cp -R "$work/mail/." "$out/mail/" 2>/dev/null || true
cp "$work/volume"/*.json "$out/" 2>/dev/null || true
for kept in "$work"/*.png "$work"/*.log "$work"/report.json; do
  [[ -f $kept ]] && cp "$kept" "$out/"
done
cp "$log" "$out/lounge.log" 2>/dev/null || true
((status == 0)) || { echo 'FAIL: a stranger did not get from the wait list to a workspace of their own' >&2; exit 1; }
echo 'ok: a stranger came from the wait list to a workspace of their own, on their passkey, with a second workspace beside it'
