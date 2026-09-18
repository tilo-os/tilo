#!/usr/bin/env bash
# The whole loop, on a real Debian base, with no privilege at any point:
# create a layer, install into it, see what it did, keep it, stack on it,
# revert one path, throw it all away, and prove the base never moved.
#
#     cargo build -p tinker && ./crates/system/tinker/demo.sh
#
# Needs rootless podman (to fetch the base once), bubblewrap, and a subordinate
# id range in /etc/subuid. Nothing here asks for a password.
set -uo pipefail

TINKER=${TINKER:-$(cd "$(dirname "$0")/../../.." && pwd)/target/debug/tinker}
export TILO_TINKER_ROOT=${TILO_TINKER_ROOT:-${TMPDIR:-/tmp}/tinker-demo}

[ -x "$TINKER" ] || { echo "build it first: cargo build -p tinker" >&2; exit 1; }

step() { printf '\n\033[1m== %s\033[0m\n' "$*"; }

# Layer contents belong to subordinate ids, so removing the store needs the
# mapping too — the same reason `tinker discard` re-enters it.
unshare --user --map-root-user --map-auto --mount -- rm -rf "$TILO_TINKER_ROOT" 2>/dev/null
rm -rf "$TILO_TINKER_ROOT"

step "a base to stack on (fetched once, then reused)"
"$TINKER" base add debian --from-image docker.io/library/debian:stable-slim || exit 1
BASE_BEFORE=$(find "$TILO_TINKER_ROOT/bases/debian" -mindepth 1 -printf '%P %y %s\n' | sort | sha256sum)

step "what would a session actually be? (--explain runs nothing)"
"$TINKER" shell --explain | tr ' ' '\n' | grep -A3 -e '--overlay$' -e '--bind$' | head -12

step "install a package, edit a config, delete a file — one throwaway shell"
"$TINKER" shell -- /bin/bash -lc '
  apt-get update -qq
  apt-get install -y -qq cowsay
  echo "tuned = yes" > /etc/my-tweak.conf
  rm -f /etc/motd
  /usr/games/cowsay "root here, and this is not your computer"'

step "what did that actually do to me?"
"$TINKER" diff scratch1

step "keep it after the fact, under a name chosen with hindsight"
"$TINKER" keep scratch1 --as cowbox --note "cowsay + a config tweak"
"$TINKER" list

step "undo one path; everything else in the layer stays"
"$TINKER" revert cowbox /etc/my-tweak.conf
"$TINKER" shell cowbox -- /bin/sh -lc '
  printf "my-tweak.conf  : %s\n" "$(cat /etc/my-tweak.conf 2>/dev/null || echo reverted)"
  printf "motd           : %s\n" "$(cat /etc/motd 2>/dev/null || echo still deleted)"
  printf "cowsay         : %s\n" "$(ls /usr/games/cowsay 2>/dev/null || echo missing)"'

step "stack a second layer on the kept one"
"$TINKER" shell buildbox --on cowbox -- /bin/sh -lc '
  ls /usr/games/cowsay >/dev/null && echo "buildbox can see cowbox underneath it"
  touch /usr/local/bin/from-buildbox'

step "a layer underneath a stack cannot be pulled out"
"$TINKER" discard cowbox --yes

step "throw it all away, top down"
"$TINKER" discard buildbox
"$TINKER" discard cowbox --yes
"$TINKER" list

step "and the base never moved"
BASE_AFTER=$(find "$TILO_TINKER_ROOT/bases/debian" -mindepth 1 -printf '%P %y %s\n' | sort | sha256sum)
if [ "$BASE_BEFORE" = "$BASE_AFTER" ]; then
  echo "base is identical to before the demo — nothing to repair, nothing to roll back"
else
  echo "BASE CHANGED — this is a bug" >&2
  exit 1
fi
