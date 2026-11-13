#!/usr/bin/env bash
# A stranger's way into tilo.sh, end to end, on a whole tilo.sh of this run's
# own: the Lounge relay, the front door, the web entry and production's Caddy,
# started the way production's manifests start them (os/containers/tilo-sh's
# stack.sh), so the door a stranger meets here is the one tilo.sh runs — taking
# people by invitation, keeping a wait list, and mailing through a sink this run
# reads. The person who runs it is declared an owner in the roles file, as the
# deployment declares its own people, which is how a fresh invitation-only
# Lounge gets its first member.
#
# The stranger's walk: the wait list and its confirmation mail; the invitation
# the person who runs the host sends from the workspace's own wait list, and
# the mail that carries it; the workspace of their own the link lands them in,
# with the agent setup that puts their agent there; the passkey that brings
# them back to it; a second workspace they make; and a visitor with no
# invitation, who still cannot get in.
#
# It stops at the passkey: a browser that forgot the site signs the person in
# with their passkey, and the host then refuses to certify the tab as one of
# their devices, so the walk ends where that refusal is. What it proves today
# is everything up to the person's own workspace with their agent in it.
#
# usage: test-onboarding.sh BUNDLE WORK
# BUNDLE is the web desktop bundle the stack's site serves
# (tools/ci/publish/web-desktop-bundle.sh). WORK keeps what it found: every
# page's screenshots and console, the report, the mails the door sent, and the
# stack's logs. Needs node, chromium, curl, bwrap and the stack's environment:
# TILO_SH_PROGRAMS, TILO_SH_LOUNGE, TILO_SH_SITE, TILO_SH_CADDY and
# TILO_SH_CLI_DIR naming what t built, and TILO_CLI_BIN the tilo command an
# agent runs.
set -euo pipefail
usage='usage: test-onboarding.sh BUNDLE WORK'
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd -- "$here/../../.." && pwd)
tilo_sh=$repo/os/containers/tilo-sh
driver=$here/integration/onboarding.mjs
bundle=${1:?$usage}
out=${2:?$usage}
tilo_bin=${TILO_CLI_BIN:-}
[[ -x $tilo_bin ]] || { echo 'TILO_CLI_BIN names no tilo command (an agent runs it here)' >&2; exit 2; }
rm -rf "$out"
mkdir -p "$out"
# The stack's state and its addresses: this run's own, beside every other
# stack on the machine (the e2e check runs its own on TILO_SH_PORT_BASE).
export TILO_SH_STATE=$out/state
export TILO_SH_PORT_BASE=${ONBOARDING_PORT_BASE:-25100}
export TILO_SH_ADMISSION=invite
# The pages here are people arriving at the Lounge, so the deployment opens
# none of its apps in front of them, and no VM host is run.
export TILO_SH_HOME_APP=
export TILO_SH_VM_HOST=0
export TILO_SH_BUNDLE=$bundle
# The site's /releases, packed as a release packs it, from the command this run
# was handed, unless the caller packed one already: the stack serves it as the
# web entry image does, and nothing here waits on the release's own build.
if [[ -z ${TILO_SH_CLI_DIR:-} ]]; then
  export TILO_SH_CLI_DIR=$out/release
  mkdir -p "$TILO_SH_CLI_DIR"
  bash "$repo/tools/ci/publish/cli-release.sh" pack "$tilo_bin" "$TILO_SH_CLI_DIR/tilo-linux-x86_64.tar.gz"
fi
export TILO_SH_TILO_BIN=$tilo_bin
trap 'bash "$tilo_sh/stack.sh" down || true' EXIT
bash "$tilo_sh/stack.sh" up --fresh || { echo 'FAIL: the stack did not come up' >&2; exit 1; }
# Sourced after it is up, for its addresses: the site, the relay, the door and
# the mail sink, all production's own.
# shellcheck source=../tilo-sh/stack.sh
. "$tilo_sh/stack.sh"
role=$state/secrets/roles.json
bash "$tilo_sh/stack.sh" status || true

node "$driver" "$site_origin" "$relay_origin" "$out" "$tilo_bin" "$role" \
  || { echo 'FAIL: a stranger did not get from the wait list to a workspace of their own' >&2
       echo '--- the mails the door sent:' >&2; bash "$tilo_sh/stack.sh" mail >&2 || true
       exit 1; }
echo 'ok: a stranger came from the wait list to a workspace of their own, with their agent and a passkey, and a second workspace beside it'
