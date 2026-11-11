#!/usr/bin/env bash
# A person's setup through their agent survives reloads, closed tabs, back and
# forward, a second tab, a code running out and going offline, against the
# Lounge's own programs run directly as test-agent-setup.sh runs them: the
# image's entrypoint, the relay the image carries, the web entry served beside
# it, the tilo command a release publishes, and real headless Chromium tabs.
# The matrix is integration/setup-resume.mjs; its captures of each resumed
# state, its report and the tabs' consoles go to WORK.
#
# usage: test-setup-resume.sh BUNDLE WORK
# Needs node, chromium, curl, and TILO_RELAY_BIN, TILO_CLI_BIN and
# TILO_CRYPTO_BIN naming built programs, and TILO_FONTS_DIR the product's font
# catalog.
set -euo pipefail
usage='usage: test-setup-resume.sh BUNDLE WORK'
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

node "$here/integration/setup-resume.mjs" "$site_origin" "$relay_origin" "$tilo_bin" "$work/site.ca.crt" \
  "$work/setup-resume" || fail 'a setup did not resume where it was'
echo 'ok: every step of a setup resumed where it was, and nothing opened the Lounge by surprise'
