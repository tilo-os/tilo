#!/usr/bin/env bash
# A person sets Tilo up through their agent, against the Lounge's own
# programs run directly: the image's entrypoint with the gate's environment,
# the tilo-relay the image carries on its PATH, the web entry served beside
# it, the tilo command a release publishes, and real headless Chromium tabs.
#
# This is the gate's agent step (integration/agent-setup.mjs) without the
# image and its micro-VM around it, so a change to the page, the relay or the
# command is tried in minutes. The Lounge gate (test-integration.sh) still
# runs the same step against the built image before anything is published.
#
# usage: test-agent-setup.sh BUNDLE WORK
# BUNDLE is the web desktop bundle (tools/ci/publish/web-desktop-bundle.sh);
# WORK is where it keeps what it found: each tab's snapshot and console, the
# report, the site's and the Lounge's logs. Needs node, chromium, curl, and
# TILO_RELAY_BIN, TILO_CLI_BIN and TILO_CRYPTO_BIN naming built programs, and
# TILO_FONTS_DIR the product's font catalog (the agent-setup check hands it the
# lounge-binaries, cli-x86_64 and image-lounge-context outputs).
set -euo pipefail
usage='usage: test-agent-setup.sh BUNDLE WORK'
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$here/../../.." && pwd)
bundle=${1:?$usage}
work=${2:?$usage}
relay_bin=${TILO_RELAY_BIN:?$usage}
tilo_bin=${TILO_CLI_BIN:?$usage}
crypto=${TILO_CRYPTO_BIN:?$usage}
rm -rf "$work"
mkdir -p "$work"
log=$work/lounge.log
site_log=$work/site.log
pids=()
cleanup() { local pid; for pid in "${pids[@]}"; do kill -KILL "$pid" 2>/dev/null || true; done; }
trap cleanup EXIT
fail() {
  echo "FAIL: $*" >&2
  [[ ! -s $log ]] || { echo '--- the Lounge said:' >&2; tail -40 "$log" >&2; }
  exit 1
}

# The gate's own addresses.
http_port=18790 plain_port=19780 live_port=19781 site_port=19782 web_port=9781
relay_origin=https://127.0.0.1:$web_port
site_origin=https://127.0.0.1:$site_port

[[ -f $bundle/build.json ]] || fail "no desktop bundle at $bundle"
python3 -I "$repo_root/crates/browser/tilo-browser-runtime/container/layout.py" "$bundle" "$work/site"
"$crypto" generate-tls-fixture 127.0.0.1 "$work/site.ca.crt" "$work/site.key" "$work/site.crt" \
  || fail 'no certificate for the site'

# The volume, as a fresh deployment mounts it: empty, with a throwaway owner
# root among the operator's secrets.
volume=$work/volume
mkdir -p "$volume/secrets" "$work/bin"
printf 'a%.0s' $(seq 64) > "$volume/secrets/room-root"
chmod 600 "$volume/secrets/room-root"
# The relay as the image carries it: the product's font catalog beside the
# program, which is where a Tilo binary finds it outside the image (its apps
# lay text out, and a host that seeds them has no system fonts).
cp "$relay_bin" "$work/bin/tilo-relay"
cp -R "${TILO_FONTS_DIR:?$usage}" "$work/bin/fonts"

# The image's entrypoint, with the environment the gate gives the image.
env -i HOME="$work" PATH="$work/bin:/usr/bin:/bin" \
  TILO_SOURCE_REVISION=agent-setup \
  TILO_SECRETS_DIR="$volume/secrets" \
  TILO_ROOMS_STATE="$volume" \
  TILO_ROOMS_PERSISTENT=lounge \
  TILO_ROOMS_NAME=Lounge \
  TILO_ROOMS_DESCRIPTION='The Tilo community: chat, notes and a shared desktop' \
  TILO_ROOMS_DEFAULT_ROLE=editor \
  TILO_ROOMS_ADMISSION=open \
  TILO_ROOMS_RELAY_ORIGIN="$relay_origin" \
  TILO_ROOMS_LIVE="127.0.0.1:$live_port" \
  TILO_ROOMS_RELAY_PLAIN="127.0.0.1:$plain_port" \
  TILO_ROOMS_HTTP="127.0.0.1:$http_port" \
  TILO_ROOMS_WEB_ENTRY="$site_origin/" \
  TILO_ROOMS_SITE_ORIGINS="$site_origin" \
  TILO_ROOMS_ROOT_FILE="$volume/secrets/room-root" \
  TILO_ROOMS_FETCH_PROXY=0 \
  TILO_ROOMS_CHAT_CHANNELS=general \
  TILO_ROOMS_PERSON_STORAGE_GB=10 \
  sh "$here/entrypoint.sh" >> "$log" 2>&1 &
pids+=($!)
for _ in $(seq 1200); do
  curl -fsS --max-time 2 "http://127.0.0.1:$http_port/mode" >/dev/null 2>&1 && break
  kill -0 "${pids[0]}" 2>/dev/null || fail 'the Lounge stopped before it was Ready'
  sleep 0.2
done
room=$(cat "$volume/lounge/room-id" 2>/dev/null) || fail 'the Lounge kept no workspace id'
echo "ok: the Lounge is Ready ($room)"

node "$here/integration/site.mjs" "$work/site" "$work/site.crt" "$work/site.key" \
  "$relay_origin" Lounge "$relay_origin/peer/v1/directory/$room/" "$site_port" >> "$site_log" 2>&1 &
pids+=($!)
for _ in $(seq 300); do
  grep -q "\"url\": *\"$site_origin\"" "$site_log" && break
  kill -0 "${pids[1]}" 2>/dev/null || fail "the web entry stopped: $(cat "$site_log")"
  sleep 0.1
done
grep -q "\"url\": *\"$site_origin\"" "$site_log" || fail "the web entry did not start: $(cat "$site_log")"
echo "ok: the web entry is served at $site_origin"

node "$here/integration/agent-setup.mjs" "$site_origin" "$relay_origin" "$tilo_bin" "$work/site.ca.crt" \
  "$work/agent-setup" || fail 'a person did not get set up through their agent'
echo 'ok: a person set up through their agent landed in their own workspace with the agent there, and came back to it'
echo 'ok: the person signed in again on a fresh browser through the agent'
