#!/usr/bin/env bash
# The Lounge image's entrypoint against a stub relay: the arguments it hands
# the host for the deployment's environment, and that it refuses an
# environment the image does not honor instead of starting with part of it.
#
# No container: it runs the entrypoint with a tilo-relay on PATH that prints
# its arguments, so the whole interface is asserted in a second.
#
# usage: test-entrypoint.sh
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/tilo-lounge.XXXXXX")
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin"
cat > "$work/bin/tilo-relay" <<'STUB'
#!/bin/sh
set -eu
for argument in "$@"; do printf '%s\n' "$argument"; done
STUB
chmod +x "$work/bin/tilo-relay"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# lounge [NAME=VALUE…]: the entrypoint's relay invocation, with only the
# environment given here.
lounge() {
  env -i PATH="$work/bin:/usr/bin:/bin" \
    TILO_ROOMS_PERSISTENT=lounge \
    TILO_ROOMS_ROOT_FILE=/run/secrets/room-root-lounge \
    TILO_ROOMS_RELAY_ORIGIN=https://eu1.tilo.coffee/ \
    TILO_ROOMS_DESCRIPTION='Coffee and tea, and whatever is being made' \
    TILO_ROOMS_WEB_ENTRY=https://tilo.coffee/ \
    TILO_ROOMS_STATE="$work/state" \
    "$@" sh "$here/entrypoint.sh"
}

# subsequence WANT ARGS: every line of WANT appears in ARGS, in that order.
subsequence() {
  awk 'FNR == NR { want[++count] = $0; next }
       count && $0 == want[seen + 1] { if (++seen == count) matched = 1 }
       END { exit !matched }' "$1" "$2"
}

fails_with() {
  local status=0
  shift 0
  "$@" >"$work/out" 2>"$work/err" || status=$?
  [[ $status == 2 ]] || fail "expected a refusal (exit 2), got $status: $(cat "$work/err")"
}

cat > "$work/want" <<'EXPECTED'
room
--state
--persistent
lounge
--root-file
/run/secrets/room-root-lounge
--room-id-file
--member-limits
--room-records
--public-avatars
--public-avatars-url
https://eu1.tilo.coffee/avatars
--listing-description
Coffee and tea, and whatever is being made
--default-role
editor
--inactive-after-days
30
--web
127.0.0.1:9781
--packet-relay
0.0.0.0:9782
--tls
dev
--web-plain
0.0.0.0:9780
--public-origin
https://eu1.tilo.coffee
--http
0.0.0.0:8790
--web-entry
https://tilo.coffee/
--allowed-origin
https://eu1.tilo.coffee
--fetch-proxy
--chat-channel
general
--seed-app
so.tilo.chat
EXPECTED
lounge >"$work/args" || fail "the plain deployment did not start"
subsequence "$work/want" "$work/args" || fail "the plain deployment maps other arguments"
# The state is the workspace's own directory under the state root, the layout
# the old wrapper left on the volume.
grep -Fqx -- "--state" "$work/args" || fail "no state flag"
grep -Fqx -- "$work/state/lounge/relay" "$work/args" || fail "the relay state is not under the workspace"
grep -Fqx -- "$work/state/lounge/room-id" "$work/args" || fail "the workspace id is not kept with the workspace"
grep -Fqx -- "$work/state/lounge/avatars" "$work/args" || fail "the avatars are not published from the workspace"
# The packet relay binds where the ingress forwards; without a named origin the
# host advertises the address it bound.
grep -Fqx -- --packet-relay-url "$work/args" && fail "a published relay URL was announced without one set"

# Every setting of a populated deployment reaches the host, and replaces the
# default it shares a flag with.
lounge \
  TILO_ROOMS_NAME=Lounge \
  TILO_ROOMS_SITE_ORIGINS='https://tilo.coffee, https://tilo.so' \
  TILO_ROOMS_CHAT_CHANNELS='general,feedback' \
  TILO_ROOMS_FEEDBACK_CHANNEL=feedback \
  TILO_ROOMS_SEED_APPS='so.tilo.chat,so.tilo.board' \
  TILO_ROOMS_AUTHOR_APPS='so.tilo.chat' \
  TILO_ROOMS_REFOUND='so.tilo.browser-desktop:desktop, so.tilo.chat:' \
  TILO_ROOMS_BOTS='/usr/local/share/tilo/bots/discord-bridge.json guild=1521251563828416532 server=Tilo mirror-channels=yes;/opt/echo.json a=b' \
  TILO_ROOMS_INACTIVE_AFTER_DAYS=7 \
  TILO_ROOMS_ROLES_FILE=/run/tilo-rooms/roles.json \
  TILO_ROOMS_FETCH_PROXY=0 \
  TILO_ROOMS_WEB=127.0.0.1:0 \
  TILO_ROOMS_RELAY_PLAIN=0.0.0.0:9880 \
  TILO_ROOMS_LIVE=0.0.0.0:9881 \
  TILO_ROOMS_LIVE_ORIGIN=https://live.eu1.tilo.coffee/ \
  TILO_ROOMS_HTTP=0.0.0.0:8890 \
  TILO_ROOMS_MODERATION_KEY_FILE=/run/secrets/moderation-model-key \
  TILO_ROOMS_MODERATION_MODEL=claude-test \
  TILO_ROOMS_DISCORD_CLIENT_ID=1559266815148953650 \
  TILO_ROOMS_DISCORD_CLIENT_SECRET_FILE=/run/secrets/discord-client-secret \
  >"$work/args" || fail "the populated deployment did not start"
cat > "$work/want" <<'EXPECTED'
--workspace-name
Lounge
EXPECTED
subsequence "$work/want" "$work/args" || fail "the workspace name is missing"
# One flag per setting, with the value the deployment gave it.
pair() {
  awk -v flag="$1" -v value="$2" '
    previous == flag && $0 == value { found = 1 }
    { previous = $0 }
    END { exit !found }' "$work/args"
}
pair --web 127.0.0.1:0 || fail "the HTTPS listener setting did not reach the host"
pair --web-plain 0.0.0.0:9880 || fail "the plain listener setting did not reach the host"
pair --public-origin https://eu1.tilo.coffee || fail "the public origin did not reach the host"
pair --packet-relay 0.0.0.0:9881 || fail "the packet relay's bind address did not reach the host"
pair --packet-relay-url https://live.eu1.tilo.coffee || fail "the published relay URL did not reach the host"
pair --http 0.0.0.0:8890 || fail "the routes listener setting did not reach the host"
pair --allowed-origin https://tilo.coffee || fail "a site origin did not reach the host"
pair --allowed-origin https://tilo.so || fail "a site origin did not reach the host"
pair --chat-channel feedback || fail "a chat channel did not reach the host"
pair --seed-app so.tilo.board || fail "a seeded app did not reach the host"
pair --author-app so.tilo.chat || fail "an authoring app did not reach the host"
pair --refound so.tilo.browser-desktop/desktop || fail "a refounded record did not reach the host"
pair --refound so.tilo.chat/ || fail "a refounded record of the app's primary did not reach the host"
pair --feedback-channel feedback || fail "the feedback channel did not reach the host"
pair --bot '/usr/local/share/tilo/bots/discord-bridge.json guild=1521251563828416532 server=Tilo mirror-channels=yes' \
  || fail "the bridge's bot spec did not reach the host"
pair --bot '/opt/echo.json a=b' || fail "a bot spec did not reach the host"
pair --moderation-key-file /run/secrets/moderation-model-key || fail "the moderation key did not reach the host"
pair --moderation-model claude-test || fail "the moderation model did not reach the host"
pair --discord-client-id 1559266815148953650 || fail "the Discord client id did not reach the host"
pair --discord-client-secret-file /run/secrets/discord-client-secret || fail "the Discord secret did not reach the host"
pair --discord-redirect-url https://eu1.tilo.coffee/discord/callback || fail "the Discord callback did not reach the host"
pair --discord-authorize-url https://discord.com/oauth2/authorize || fail "the Discord consent page did not reach the host"
pair --inactive-after-days 7 || fail "the inactivity setting did not reach the host"
pair --roles-file /run/tilo-rooms/roles.json || fail "the roles file did not reach the host"
grep -Fqx -- --fetch-proxy "$work/args" && fail "fetch proxy 0 still asked for the proxy"
grep -Fqx -- 0.0.0.0:9780 "$work/args" && fail "the plain listener default was not replaced"
grep -Fqx -- 127.0.0.1:9781 "$work/args" && fail "the HTTPS listener default was not replaced"

# eu1's own environment, unchanged but for the image: it takes people by
# invitation, and names no description. The workspace stays listed, its door
# admits only people invited, and the description is the name people know it
# by.
eu1() {
  env -i PATH="$work/bin:/usr/bin:/bin" \
    TILO_ROOMS_PERSISTENT=lounge \
    TILO_ROOMS_NAME=Lounge \
    TILO_ROOMS_ADMISSION=invite \
    TILO_ROOMS_ROOT_FILE=/run/secrets/room-root-lounge \
    TILO_ROOMS_RELAY_ORIGIN=https://eu1.tilo.coffee \
    TILO_ROOMS_PUBLIC_API=https://eu1.tilo.coffee \
    TILO_ROOMS_LIVE_ORIGIN=https://live.eu1.tilo.coffee \
    TILO_ROOMS_WEB_ENTRY=https://tilo.coffee/ \
    TILO_ROOMS_SITE_ORIGINS=https://tilo.coffee,https://tilo.so \
    TILO_ROOMS_FEEDBACK_CHANNEL=feedback \
    TILO_ROOMS_STATE="$work/state" \
    sh "$here/entrypoint.sh"
}
eu1 >"$work/args" || fail "eu1's environment does not start"
grep -Fqx -- --invitation-only "$work/args" || fail "invite admission did not close the door"
grep -Fqx -- --members-only "$work/args" && fail "invite admission unlisted the workspace"
pair --listing-description Lounge || fail "the description did not default to the workspace's name"
pair --persistent lounge || fail "the workspace is not the one eu1 names"
pair --public-origin https://eu1.tilo.coffee || fail "eu1's origin did not reach the host"

# An admission the image does not honor stops the start, as any other setting
# it does not read does.
fails_with lounge TILO_ROOMS_ADMISSION=anyone
grep -Fq TILO_ROOMS_ADMISSION "$work/err" || fail "the refusal does not name the setting"
fails_with lounge TILO_ROOMS_MAX_SPECTATORS=2000
grep -Fq TILO_ROOMS_MAX_SPECTATORS "$work/err" || fail "the refusal does not name the setting"

# Without what it cannot invent, it names the setting and starts nothing.
for required in TILO_ROOMS_PERSISTENT TILO_ROOMS_ROOT_FILE TILO_ROOMS_RELAY_ORIGIN TILO_ROOMS_WEB_ENTRY; do
  unset_env=(TILO_ROOMS_PERSISTENT=lounge TILO_ROOMS_ROOT_FILE=/root TILO_ROOMS_RELAY_ORIGIN=https://eu1.example
    TILO_ROOMS_DESCRIPTION=text TILO_ROOMS_WEB_ENTRY=https://tilo.example/)
  status=0
  env -i PATH="$work/bin:/usr/bin:/bin" "${unset_env[@]}" sh -c "unset $required; exec sh '$here/entrypoint.sh'" \
    >"$work/out" 2>"$work/err" || status=$?
  [[ $status == 2 ]] || fail "$required missing did not stop the start ($status)"
  grep -Fq "$required" "$work/err" || fail "the refusal does not name $required"
done

echo "ok: the Lounge entrypoint maps the deployment and refuses the rest"
