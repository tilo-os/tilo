#!/usr/bin/env bash
# A folder's bulk write survives its holder being killed halfway, on a fresh tilo.sh stack
# brought up here from the one build its programs are made in (the check tilo-sh-programs, whose
# output holds the tilo command too). peer-kill-midwrite.sh does the check: thousands of files
# go into a mounted folder, the holder is killed with SIGKILL halfway, `tilo status` is asked
# the whole time, and the write must finish with nothing the folder acknowledged lost, no status
# replacing a holder that was there, and the server's tree reading whole.
#
# The script starts inside a user namespace where it is root, because a folder is mounted with
# FUSE: the job sandbox gives the check no capability to mount, and no_new_privs keeps fusermount3's
# setuid bit from helping.
#
# usage: test-peer-kill-midwrite.sh WORK [files|git|long]
#
# With `git`, the write is a checkout of this repository instead (peer-git-midwrite.sh): timed,
# and ended halfway by SIGKILL and by SIGTERM. With `long`, one holder
# lives under a change that goes on for many minutes (peer-long-change.sh), and `tilo status` and
# `tilo ls` must answer within seconds the whole time.
#
# WORK keeps the stack's state (WORK/state) and its logs (WORK/state/logs, the write's among
# them). From the environment: TILO_SH_PROGRAMS (tilo-sh-programs' output), TILO_SH_CADDY
# (tilo-sh-caddy's) and TILO_SH_FONTS (lounge-fonts' catalog).
set -euo pipefail
usage='usage: test-peer-kill-midwrite.sh WORK [files|git|long]'
work=${1:?$usage}
write=${2:-files}
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd -- "$here/../../.." && pwd)
die() { printf 'test-peer-kill-midwrite: %s\n' "$*" >&2; exit 1; }
programs=${TILO_SH_PROGRAMS:?TILO_SH_PROGRAMS names the programs the check tilo-sh-programs built}
fonts=${TILO_SH_FONTS:?TILO_SH_FONTS names the font catalog the check lounge-fonts built}
: "${TILO_SH_CADDY:?TILO_SH_CADDY names the Caddy the check tilo-sh-caddy fetched}"
[[ -x $programs/tilo ]] || die "$programs holds no tilo command, which the stack's own person acts through"
[[ -f $fonts/manifest.toml ]] || die "no product font catalog in $fonts"

mkdir -p "$work"
work=$(cd -- "$work" && pwd)
export TILO_SH_STATE=$work/state TILO_SH_REMOTE=0 TILO_SH_TILO_BIN=$programs/tilo
export TILO_SH_PORT_BASE=${TILO_SH_PORT_BASE:-24300}
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

# The write itself, as root in a user namespace of its own so that its folder can be mounted.
case $write in
  files) script=peer-kill-midwrite.sh ;;
  git) script=peer-git-midwrite.sh ;;
  long) script=peer-long-change.sh ;;
  *) die "$usage" ;;
esac
mkdir -p "$TILO_SH_STATE/logs/kill-midwrite"
unshare --user --map-root-user --mount -- env "TILO=$programs/tilo" "REPO=$repo" \
  "TILO_KILL_LOGS=$TILO_SH_STATE/logs/kill-midwrite" "TILO_LONG_LOGS=$TILO_SH_STATE/logs/kill-midwrite" \
  bash "$here/integration/$script" \
  || die "the $write write did not survive its holder ending (above)"
