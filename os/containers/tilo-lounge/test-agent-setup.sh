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
# lounge-gate-binaries, lounge-fonts, cli-x86_64 and lounge-gate-tools outputs).
set -euo pipefail
usage='usage: test-agent-setup.sh BUNDLE WORK'
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
bundle=${1:?$usage}
work=${2:?$usage}
tilo_bin=${TILO_CLI_BIN:?$usage}
rm -rf "$work"
mkdir -p "$work"
# shellcheck source=direct-lounge.sh
. "$here/direct-lounge.sh"
trap stop_lounge EXIT
fail() { lounge_fail "$@"; }
start_lounge "$bundle" "$work"

node "$here/integration/agent-setup.mjs" "$site_origin" "$relay_origin" "$tilo_bin" "$work/site.ca.crt" \
  "$work/agent-setup" || fail 'a person did not get set up through their agent'
echo 'ok: a person set up through their agent landed in their own workspace with the agent there, and came back to it'
echo 'ok: the person signed in again on a fresh browser through the agent'
