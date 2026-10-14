#!/usr/bin/env bash
# Two-host peer-sync smoke test.
#
# Usage: open two terminals on the same LAN.
#
# Terminal 1 (owner):
#   ./scripts/two-host-sync-test.sh owner
#
# Terminal 2 (peer):
#   ./scripts/two-host-sync-test.sh peer
#
# Owner: launches `shell --backend winit` (peer sync is on by default),
# prints an invite token via /invite, copies it to clipboard, and
# pauses for the peer to connect.
#
# Peer: launches `shell --backend winit`,
# waits for the user to paste the token into /connect, then both
# trees should converge on each other's edits.
#
# This is documentation + glue, not an automated test.

set -euo pipefail

ROLE="${1:-}"
case "$ROLE" in
  owner)
    echo "[two-host] Starting owner. Use /invite in the palette to mint a token."
    exec cargo run -p shell --bin shell -- --backend winit
    ;;
  peer)
    echo "[two-host] Starting peer. Use /connect in the palette and paste the owner's token."
    exec cargo run -p shell --bin shell -- --backend winit
    ;;
  *)
    echo "Usage: $0 {owner|peer}" >&2
    exit 2
    ;;
esac
