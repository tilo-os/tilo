#!/usr/bin/env bash
# What stack.sh tells a service it starts as a plain process: its image's ENV,
# each path of the image moved to where this stack keeps it.
#
# No stack starts here. The stack's own start function writes the environment
# of the server, with the manifest's variables and the program replaced by
# stand-ins; the server's image entry point is then started with exactly what
# was written, in front of a tilo-server that says what it was told. Held:
#
#   - the folder for the keys of people's sealed stores reaches the server,
#     lies outside the state folder and is there when the server starts. A
#     server that is not told it keeps each key in its state, beside what the
#     key opens, and then a copy of the state alone opens people's vaults;
#   - a variable the stack hands a service whose image does not set it stops
#     the start, instead of being left out without a word;
#   - the web entry's image sets the two folders its start hands it, so it is
#     still told both;
#   - stack.sh keys, which judges a stack that is up, fails when the server did
#     not say where it keeps people's keys, when it says they are in the state,
#     when a key lies there, or when a sealed store has no key apart, and tells
#     a stack on which nobody sealed anything from one whose keys it saw.
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
unset TILO_SH_PEOPLE_KEYS TILO_SH_LOUNGE
# shellcheck source=stack.sh
. "$here/stack.sh"
told=$runs
mkdir -p "$state"/{logs,server,secrets} "$native" "$told"

# The Lounge as its image's context lays it out: the image's own entry point,
# and a tilo-server that says what it was started with.
lounge=$work/lounge
mkdir -p "$lounge"/{bin,surfaces,fonts}
cp "$repo/os/containers/tilo-lounge/entrypoint.sh" "$lounge/entrypoint.sh"
cat > "$lounge/bin/tilo-server" <<'STAND_IN'
#!/bin/sh
if [ -n "${TILO_PEOPLE_KEYS+set}" ]; then
  printf 'TILO_PEOPLE_KEYS=%s\n' "$TILO_PEOPLE_KEYS"
  if [ -d "$TILO_PEOPLE_KEYS" ]; then echo 'the folder is there'; else echo 'no folder is there'; fi
else
  echo 'TILO_PEOPLE_KEYS is not set'
fi
STAND_IN
chmod +x "$lounge/bin/tilo-server"

# The manifest's variables name the stack's own person, whom only a built
# tilo command makes: the few the entry point does not start without stand in
# for them.
server_env() {
  printf '%s\n' TILO_WORKSPACES_PERSISTENT=lounge \
    "TILO_WORKSPACES_ROOT_FILE=$state/secrets/workspace-root-lounge" \
    "TILO_WORKSPACES_SERVER_ORIGIN=$server_origin" "TILO_WORKSPACES_WEB_ENTRY=$site_origin/"
}
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
named=$(grep -c '^TILO_PEOPLE_KEYS=' "$told/server.env" || true)
if [[ $named != 1 ]]; then
  fault "stack.sh tells the server it starts no folder for the keys of people's sealed stores ($named lines of its environment name TILO_PEOPLE_KEYS), so each key lies in the state, beside what it opens"
elif ! grep -Fqx "TILO_PEOPLE_KEYS=$people_keys" "$told/server.env"; then
  fault "stack.sh tells the server another folder than its own $people_keys: $(grep '^TILO_PEOPLE_KEYS=' "$told/server.env")"
fi
case $people_keys/ in
  "$state"/*) fault "the folder for people's keys, $people_keys, lies inside the state folder $state" ;;
esac
grep -Fqx "TILO_PEOPLE_KEYS=$people_keys" "$work/server.said" 2>/dev/null \
  || fault "the program the entry point started was not told the folder for people's keys: $(cat "$work/server.said" 2>/dev/null)"
grep -Fqx 'the folder is there' "$work/server.said" 2>/dev/null \
  || fault "the folder for people's keys was not there when the server started"
[[ ! -d $people_keys || $(stat -c %a "$people_keys") == 700 ]] \
  || fault "the folder for people's keys is open to others: mode $(stat -c %a "$people_keys")"

# -- a variable the image does not set ----------------------------------------

status=0
(image_env_here "$repo/os/containers/tilo-lounge/Containerfile" \
  "TILO_WORKSPACES_STATE=$state/server" "TILO_SECRETS_DIR=$state/server/secrets" \
  "TILO_PEOPLE_KEYS=$people_keys" "TILO_NOT_OF_THIS_IMAGE=$work/nowhere") \
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
web=$repo/crates/browser/tilo-browser-runtime/container/Containerfile
handed=$(sed -n '/^start_native_web() {/,/^}/p' "$here/stack.sh" | grep -c 'image_env_here "\$container/Containerfile"' || true)
[[ $handed == 1 ]] || fault "start_native_web no longer hands its image's ENV on as this test reads it ($handed calls)"
(image_env_here "$web" "XDG_DATA_HOME=$work/web/data" "XDG_CONFIG_HOME=$work/web/config") \
  > "$work/web.env" 2> "$work/refused" \
  || fault "the web entry's image does not set what stack.sh hands it: $(cat "$work/refused")"
for pair in "XDG_DATA_HOME=$work/web/data" "XDG_CONFIG_HOME=$work/web/config"; do
  grep -Fqx "$pair" "$work/web.env" 2>/dev/null || fault "the web entry was not told $pair"
done

# -- stack.sh keys ------------------------------------------------------------

# A state as a stack that is up leaves it: the server's log, and under the
# server's state one workspace's sealed store. What the server says, and where
# that store's key lies, is set for each case.
log=$state/logs/server.log
person=lounge/server/server/workspaces/workspaces/0f1e2d3c/accounts/vault
apart_line="2026-01-01T00:00:00.000000Z  INFO tilo_server::people_keys: the keys of people's sealed stores are kept apart from the state folder keys=$people_keys state=$state/server/lounge/server/server"
state_line="2026-01-01T00:00:00.000000Z  WARN tilo_server::people_keys: the keys of people's sealed stores are kept in the state folder, each beside what it opens, so a copy of the state opens them: TILO_PEOPLE_KEYS names no folder state=$state/server/lounge/server/server"
# judged WANT WHAT [SAID]: keys_apart ends as WANT (0 or 1), and what it says
# holds SAID.
judged() {
  local want=$1 what=$2 said=${3:-} status=0
  keys_apart > "$work/keys.said" 2>&1 || status=$?
  [[ $status == "$want" ]] || fault "stack.sh keys answered $status, not $want, for $what: $(cat "$work/keys.said")"
  [[ -z $said ]] || grep -Fq -- "$said" "$work/keys.said" \
    || fault "stack.sh keys did not say '$said' for $what: $(cat "$work/keys.said")"
}
rm -rf "${people_keys:?}" "$state/server/lounge"
: > "$log"
judged 1 'a server that said nothing of where it keeps the keys' 'did not say'
printf '%s\n' "$state_line" > "$log"
judged 1 'a server that says it keeps the keys in its state' 'does not keep'
printf '%s\n' "$state_line" "$apart_line" > "$log"
judged 0 'a server that says so since its last start, with nothing sealed yet' 'nobody has sealed anything'
mkdir -p "$state/server/$person" "$people_keys/workspaces/0f1e2d3c/accounts/vault"
: > "$state/server/$person/vault.enc"
judged 1 'a sealed store with no key under the folder for the keys' 'only 0 keys'
: > "$state/server/$person/vault.key"
judged 1 'a key that lies in the state' "$person/vault.key"
mv "$state/server/$person/vault.key" "$people_keys/workspaces/0f1e2d3c/accounts/vault/vault.key"
judged 0 'a sealed store whose key lies apart' '1 sealed stores in it, 1 keys under'
printf '%s\n' "$apart_line" "$state_line" > "$log"
judged 1 'a server that kept the keys apart before and now says it does not' 'does not keep'

if ((faults)); then
  echo "FAIL: $faults things stack.sh tells the services it starts did not hold" >&2
  exit 1
fi
echo "ok: the server stack.sh starts is told the folder for people's keys ($people_keys, outside $state), a variable its image does not set stops the start, the web entry's image sets what it is handed, and stack.sh keys tells a stack that keeps people's keys apart from one that does not"
