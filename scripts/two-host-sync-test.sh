#!/usr/bin/env bash
# Two-host workspace-sync smoke test.
#
# Usage: on each of two machines on the same network, from a checkout:
#
#   ./scripts/two-host-sync-test.sh owner
#   ./scripts/two-host-sync-test.sh peer
#
# Both start the desktop session, `tilo-session`. On the owner, open the
# workspace's settings and, under "Invite people", "Create link" and then
# "Copy link"; on the peer, "Join a workspace" and paste it. Both desks then
# hold one workspace: a note typed on one shows on the other.
#
# The session is the one the image installs, `/usr/bin/tilo-session`; a
# checkout's build is named with TILO_SESSION, and every argument is passed on:
#
#   TILO_SESSION=target/debug/tilo-session ./scripts/two-host-sync-test.sh owner
#
# `--backend nested` runs the desktop in a window of the desktop already on
# screen, which is how both roles can be tried from one machine; `--backend
# headless` starts it with no display at all, as a session on a builder does.
#
# This is documentation + glue, not an automated test.

set -euo pipefail

ROLE="${1:-}"
shift || true
case "$ROLE" in
  owner)
    echo "[two-host] owner: make a link under the workspace's settings, Invite people."
    ;;
  peer)
    echo "[two-host] peer: Join a workspace, and paste the owner's link."
    ;;
  *)
    echo "Usage: $0 {owner|peer} [tilo-session arguments...]" >&2
    exit 2
    ;;
esac
session="${TILO_SESSION:-tilo-session}"
command -v "$session" >/dev/null || {
    echo "[two-host] no session program at $session; set TILO_SESSION to a build" >&2
    echo "[two-host] (TILO_SESSION=target/debug/tilo-session) or install the session" >&2
    exit 2
}
exec "$session" "$@"
