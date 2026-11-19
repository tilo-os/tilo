#!/usr/bin/env bash
# A computer's folder keeps syncing across a hand-over of its holder, on a fresh tilo.sh stack
# brought up here from the one build its programs are made in (the check tilo-sh-programs, whose
# output holds the tilo command too). peer-handover.sh does the check: an agent's `tilo mcp`
# starts the device's holder, the user service's holder takes the device over, and a file
# written into the folder afterward reaches the workspace.
#
# The script starts inside a user namespace where it is root, because a folder is mounted with FUSE:
# the job sandbox gives the check no capability to mount, and no_new_privs keeps fusermount3's
# setuid bit from helping, so a mount from the check itself fails with "mount failed: Operation
# not permitted".
#
# usage: test-peer-handover.sh WORK
#
# WORK keeps the stack's state (WORK/state) and its logs. From the environment: TILO_SH_PROGRAMS
# (tilo-sh-programs' output), TILO_SH_CADDY (tilo-sh-caddy's) and TILO_SH_FONTS (lounge-fonts'
# catalog, which the stack lays the server's context out with); bwrap, curl, python3, openssl and
# fusermount3, as every stack here and a folder mount need. The site is a stub of the two files
# the web entry starts with: nothing opens a page.
set -euo pipefail
usage='usage: test-peer-handover.sh WORK'
work=${1:?$usage}
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd -- "$here/../../.." && pwd)
die() { printf 'test-peer-handover: %s\n' "$*" >&2; exit 1; }
programs=${TILO_SH_PROGRAMS:?TILO_SH_PROGRAMS names the programs the check tilo-sh-programs built}
fonts=${TILO_SH_FONTS:?TILO_SH_FONTS names the font catalog the check lounge-fonts built}
: "${TILO_SH_CADDY:?TILO_SH_CADDY names the Caddy the check tilo-sh-caddy fetched}"
[[ -x $programs/tilo ]] || die "$programs holds no tilo command, which the stack's own person acts through"
[[ -f $fonts/manifest.toml ]] || die "no product font catalog in $fonts"

mkdir -p "$work"
work=$(cd -- "$work" && pwd)
export TILO_SH_STATE=$work/state TILO_SH_REMOTE=0 TILO_SH_TILO_BIN=$programs/tilo
export TILO_SH_PORT_BASE=${TILO_SH_PORT_BASE:-24100}
export TILO_SH_SITE=$work/site
stack() { bash "$here/stack.sh" "$@"; }
trap 'stack down >/dev/null 2>&1 || true' EXIT
# A state of its own, new, with the font catalog where the stack looks before it fetches one.
stack down --wipe >/dev/null 2>&1 || true
rm -rf "$TILO_SH_SITE"
mkdir -p "$TILO_SH_SITE/site" "$TILO_SH_STATE/native"
printf '<!doctype html><title>tilo.sh</title>\n' > "$TILO_SH_SITE/site/index.html"
cp "$repo/dist/install/tilo.sh" "$TILO_SH_SITE/install.sh"
cp -r "$fonts" "$TILO_SH_STATE/native/fonts"
# What up prints on its output is an owner's link, which nobody here opens.
stack up >/dev/null || die 'the stack did not come up'

# The hand-over itself, as root in a user namespace of its own so that its folder can be mounted.
unshare --user --map-root-user --mount -- env "TILO=$programs/tilo" \
  bash "$here/integration/peer-handover.sh" \
  || die 'the folder did not keep syncing across the hand-over (above)'
