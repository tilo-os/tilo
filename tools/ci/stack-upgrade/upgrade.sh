#!/usr/bin/env bash
# A stack holding data comes up on the next build: the build before this commit
# fills a local tilo.sh stack (os/containers/tilo-sh/stack.sh) with a workspace's
# chat, tasks, a note, a file, Mail, a contact and an event, the stack goes
# down keeping its state, and this commit's programs bring the same state up and
# read every one of them back (workspace_data.py). A change to what a server
# stores that does not carry the data an earlier build wrote forward fails here,
# on the commit that made it, instead of on the first stack that updates.
#
# usage: upgrade.sh WORK [BASE]
#   WORK  the base's checkout, programs and server context, the stack's state
#         and what the fill wrote (WORK/expected.json)
#   BASE  the commit whose build fills the stack (default: where this commit
#         left main, or the commit before when this is main)
#
# This commit's programs are TILO_SH_PROGRAMS and TILO_SH_LOUNGE, as t's
# tilo-sh-binaries builds them, and its tilo command TILO_CLI_BIN (lounge-check-cli); the base's are built here, from a checkout of
# BASE, by that commit's own scripts. The product font catalog is
# TILO_SH_FONTS (lounge-fonts' output). The stack's site is TILO_SH_SITE's for
# both builds: the fill and the check talk to the API alone.
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd -- "$here/../../.." && pwd)
work=${1:?usage: upgrade.sh WORK [BASE]}
mkdir -p "$work"
work=$(cd -- "$work" && pwd)
base=${2:-}
if [[ -z $base ]]; then
  base=$(git -C "$repo" merge-base HEAD main 2>/dev/null || git -C "$repo" rev-parse HEAD^)
  [[ $base != "$(git -C "$repo" rev-parse HEAD)" ]] || base=$(git -C "$repo" rev-parse HEAD^)
fi
base=$(git -C "$repo" rev-parse --verify "$base^{commit}")
: "${TILO_SH_PROGRAMS:?the programs of this commit (tilo-sh-binaries builds them in /target/publish/tilo-sh/bin)}"
: "${TILO_SH_LOUNGE:?the server context of this commit (tilo-sh-binaries lays it out in /target/publish/tilo-sh/lounge)}"
fonts=${TILO_SH_FONTS:?the product font catalog (lounge-fonts fetches it to /target/publish/lounge/fonts)}
now_tilo=${TILO_CLI_BIN:?the tilo command of this commit (lounge-check-cli builds it in /target/publish/lounge-check/cli/tilo)}
now_programs=$TILO_SH_PROGRAMS now_lounge=$TILO_SH_LOUNGE
say() { printf 'stack-upgrade: %s\n' "$*" >&2; }

# What a server stores is written by the crates and the stack's own scripts; a
# commit that changes neither keeps the base's formats.
if git -C "$repo" diff --quiet "$base" HEAD -- Cargo.lock crates apps os/containers/tilo-sh os/containers/tilo-lounge; then
  say "nothing a server stores changed since $base: no upgrade to try"
  exit 0
fi

# -- the base's build ---------------------------------------------------------
checkout=$work/base
if [[ ! -d $checkout/.git && ! -f $checkout/.git ]]; then
  git -C "$repo" worktree add --detach --force "$checkout" "$base" >&2
fi
git -C "$checkout" checkout -q --detach "$base"
trap 'git -C "$repo" worktree remove --force "$checkout" >/dev/null 2>&1 || true' EXIT
say "building $(git -C "$repo" log -1 --format='%h %s' "$base")"
bash "$checkout/tools/ci/publish/tilo-sh-programs.sh" "$work/base-programs" >&2
TILO_LOUNGE_SURFACE_COMMIT=$base \
  bash "$checkout/tools/ci/publish/lounge-context.sh" "$work/base-lounge" "$work/base-programs" "$fonts" >/dev/null

stack=$repo/os/containers/tilo-sh/stack.sh
export TILO_SH_STATE=$work/state TILO_SH_HOME_APP= TILO_SH_ADMISSION=open
export TILO_SH_PORT_BASE=${TILO_SH_PORT_BASE:-26100}
rm -rf "$TILO_SH_STATE" "$TILO_SH_STATE-keys"
up() {
  TILO_SH_PROGRAMS=$1 TILO_SH_LOUNGE=$2 bash "$stack" up >&2
  TILO_SH_PROGRAMS=$1 TILO_SH_LOUNGE=$2 bash "$stack" owner --here >/dev/null
  eval "$(TILO_SH_PROGRAMS=$1 TILO_SH_LOUNGE=$2 bash "$stack" env)"
  export TILO_API_URL TILO_API_KEY
}
down() { bash "$stack" down >&2 || true; }
trap 'down; git -C "$repo" worktree remove --force "$checkout" >/dev/null 2>&1 || true' EXIT

# -- filled by the base, opened by this commit --------------------------------
say "the base's stack, filled"
up "$work/base-programs" "$work/base-lounge"
python3 -I "$here/workspace_data.py" fill > "$work/expected.json"
down
say "this commit's stack, on the state the base wrote"
# Its tilo command, packed as a release packs it, for the site's /releases; the
# base's stack packed the one beside its programs.
mkdir -p "$work/release"
bash "$repo/tools/ci/publish/cli-release.sh" pack "$now_tilo" "$work/release/tilo-linux-x86_64.tar.gz"
export TILO_SH_CLI_DIR=$work/release TILO_SH_TILO_BIN=$now_tilo
up "$now_programs" "$now_lounge"
python3 -I "$here/workspace_data.py" check "$work/expected.json"
