#!/usr/bin/env bash
# What stack.sh tells a service it starts as a plain process: its image's ENV,
# each path of the image moved to where this stack keeps it.
#
# No stack starts here. The stack's own start function writes the environment
# of the server, with the manifest's variables and the program replaced by
# stand-ins; the server's image entry point is then started with exactly what
# was written, in front of a tilo-runtime that says what it was told. Held:
#
#   - the server starts with the environment the stack wrote for it;
#   - a variable the stack hands a service whose image does not set it stops
#     the start, instead of being left out without a word;
#   - the web entry's image sets the two folders its start hands it, so it is
#     still told both;
#   - stack.sh vaults, which judges a stack that is up, fails when a sealed
#     store of the server's own, or its key, lies under the server's state, and
#     passes where every secret is in a workspace's vault.
#
# usage: test-native-env.sh
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/tilo-sh-env.XXXXXX")
trap 'rm -rf "$work"' EXIT
faults=0
fault() {
  echo "FAIL: $*" >&2
  faults=$((faults + 1))
}

# The stack's own addresses and functions, on a state of this test's own.
# Nothing of it is built, fetched or started.
export TILO_SH_STATE=$work/state TILO_SH_REMOTE=0
unset TILO_SH_LOUNGE
# shellcheck source=stack.sh
. "$here/stack.sh"
told=$runs
mkdir -p "$state"/{logs,server,secrets} "$native" "$told"

# The Lounge as its image's context lays it out: the image's own entry point,
# and a tilo-runtime that says what it was started with.
lounge=$work/lounge
mkdir -p "$lounge"/{bin,surfaces,fonts}
cp "$repo/os/containers/tilo-lounge/entrypoint.sh" "$lounge/entrypoint.sh"
cat > "$lounge/bin/tilo-runtime" <<'STAND_IN'
#!/bin/sh
printf 'TILO_WORKSPACES_STATE=%s\n' "${TILO_WORKSPACES_STATE-}"
STAND_IN
chmod +x "$lounge/bin/tilo-runtime"

# The manifest's variables name the stack's own person, whom only a built
# tilo command makes: the few the entry point does not start without stand in
# for them.
server_env() {
  printf '%s\n' "TILO_WORKSPACES_STATE=$state/server" \
    "TILO_WORKSPACES_WEB_ENTRY=$site_origin/" \
    "TILO_SERVER_CONFIG=$state/server/server.toml"
}
# The stack's person's founding act issued the tickets its computers join with; the test has no
# tilo binary, so this is the one the Lounge's first start takes.
mkdir -p "$state/owner"
printf 'server tilo1test\n' > "$state/owner/tickets"
# The server is started as the stack starts it, with only the environment its
# file sets, but at once and without the sandbox that hands it the trust
# bundle; what it prints is kept. Nothing else is started.
run_native() {
  local name=$1 dir=$2 envs=()
  shift 2
  [[ $name == server ]] || return 0
  mapfile -t envs < "$told/$name.env"
  (cd "$dir" && env -i "${envs[@]}" "$@") > "$work/$name.said" 2>&1
}

# -- the server ---------------------------------------------------------------

(start_native_server) || fault "stack.sh did not start the server: $(cat "$work/server.said" 2>/dev/null)"
grep -Fqx "TILO_WORKSPACES_STATE=$state/server" "$work/server.said" 2>/dev/null \
  || fault "the program the entry point started was not told its state: $(cat "$work/server.said" 2>/dev/null)"

# -- a variable the image does not set ----------------------------------------

status=0
(image_env_here "$repo/os/containers/tilo-lounge/Containerfile" \
  "TILO_WORKSPACES_STATE=$state/server" "TILO_SECRETS_DIR=$state/server/secrets" \
  "TILO_NOT_OF_THIS_IMAGE=$work/nowhere") \
  > "$work/placed" 2> "$work/refused" || status=$?
if [[ $status == 0 ]]; then
  fault "stack.sh handed the server TILO_NOT_OF_THIS_IMAGE, which the Lounge image does not set, and left it out without a word ($(grep -c TILO_NOT_OF_THIS_IMAGE "$work/placed" || true) lines of what it printed name it)"
elif ! grep -Fq TILO_NOT_OF_THIS_IMAGE "$work/refused"; then
  fault "the refusal does not name the variable it could not place: $(cat "$work/refused")"
fi
# A path of the image the stack does not move stops the start, as before.
status=0
(image_env_here "$repo/os/containers/tilo-lounge/Containerfile") > /dev/null 2> "$work/refused" || status=$?
[[ $status != 0 ]] && grep -Fq TILO_WORKSPACES_STATE "$work/refused" \
  || fault "an image path the stack was not told how to move did not stop the start: $(cat "$work/refused")"

# -- the web entry ------------------------------------------------------------

# What start_native_web hands its image's ENV: the two folders, and no other
# name. Its image sets both, so the refusal above stops no start of it.
web=$repo/crates/core/tilo-browser-runtime/container/Containerfile
handed=$(sed -n '/^start_native_web() {/,/^}/p' "$here/stack.sh" | grep -c 'image_env_here "\$container/Containerfile"' || true)
[[ $handed == 1 ]] || fault "start_native_web no longer hands its image's ENV on as this test reads it ($handed calls)"
(image_env_here "$web" "XDG_DATA_HOME=$work/web/data" "XDG_CONFIG_HOME=$work/web/config") \
  > "$work/web.env" 2> "$work/refused" \
  || fault "the web entry's image does not set what stack.sh hands it: $(cat "$work/refused")"
for pair in "XDG_DATA_HOME=$work/web/data" "XDG_CONFIG_HOME=$work/web/config"; do
  grep -Fqx "$pair" "$work/web.env" 2>/dev/null || fault "the web entry was not told $pair"
done

# -- stack.sh vaults ----------------------------------------------------------

# A state as a stack that is up leaves it, with one workspace's store under
# the server's state: whether a sealed store of the server's own lies in it is
# set for each case.
store=lounge/server/server/workspaces/workspaces/0f1e2d3c/accounts
# judged WANT WHAT [SAID]: host_keeps_no_vault ends as WANT (0 or 1), and what
# it says holds SAID.
judged() {
  local want=$1 what=$2 said=${3:-} status=0
  host_keeps_no_vault > "$work/vaults.said" 2>&1 || status=$?
  [[ $status == "$want" ]] || fault "stack.sh vaults answered $status, not $want, for $what: $(cat "$work/vaults.said")"
  [[ -z $said ]] || grep -Fq -- "$said" "$work/vaults.said" \
    || fault "stack.sh vaults did not say '$said' for $what: $(cat "$work/vaults.said")"
}
rm -rf "$state/server/lounge"
mkdir -p "$state/server/$store"
: > "$state/server/$store/store.json"
judged 0 'a workspace whose accounts name their sign-ins in its vault' 'keeps no sealed store'
mkdir -p "$state/server/$store/vault"
: > "$state/server/$store/vault/vault.enc"
judged 1 "a sealed store of the server's own" "$store/vault/vault.enc"
rm "$state/server/$store/vault/vault.enc"
: > "$state/server/$store/vault/vault.key"
judged 1 "a key of a store of the server's own" "$store/vault/vault.key"

if ((faults)); then
  echo "FAIL: $faults things stack.sh tells the services it starts did not hold" >&2
  exit 1
fi
echo "ok: the server stack.sh starts is told what its image says, a variable its image does not set stops the start, the web entry's image sets what it is handed, and stack.sh vaults tells a server that keeps no sealed store of its own from one that does"
