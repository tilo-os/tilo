#!/bin/sh
# The Lounge's host: tilo-relay's persistent public workspace, the deployment
# routes beside it (/mode, /avatars/<name>, /discord/callback) and the Discord
# bridge, all in one process, read from the TILO_ROOMS_* the deployment sets.
#
# Required:
#   TILO_ROOMS_PERSISTENT     the workspace's name; its state lives in
#                             $TILO_ROOMS_STATE/$TILO_ROOMS_PERSISTENT
#   TILO_ROOMS_ROOT_FILE      its owner root, 64 hex characters, mounted from
#                             the operator's secrets
#   TILO_ROOMS_RELAY_ORIGIN   the https:// origin the ingress publishes for the
#                             relay: its pages, its avatars and the callback
#   TILO_ROOMS_DESCRIPTION    what the public directory says the workspace is;
#                             listed, anyone joins through its door at
#                             TILO_ROOMS_DEFAULT_ROLE
#   TILO_ROOMS_WEB_ENTRY      the Tilo app's web entry the links it mints open
#
# The operator may declare more owners and moderators than the workspace's
# owner root, in a file `{"owners": [hex], "moderators": [hex]}` the host
# reads again whenever it changes, so a person's role reaches the workspace
# while it runs:
#   TILO_ROOMS_ROLES_FILE    that file's path (unset: the owner root alone)
#
# The pages reach the host's live plane through the host's own iroh packet
# relay, over websockets, which the deployment publishes beside the HTTP
# routes: it binds TILO_ROOMS_LIVE, and pages dial TILO_ROOMS_LIVE_ORIGIN
# when an ingress terminates TLS in front of it. The HTTPS listener behind the
# ingress binds TILO_ROOMS_WEB; an address ending in `:0` takes a port the
# kernel picks, which the host announces in its started event.
#
# Everything else has the default below. Any other TILO_ROOMS_* variable stops
# the start: this host does not read it, and a deployment that still sets what
# the host no longer honors must not start believing it does.
set -eu

fail() {
  echo "tilo-lounge: $*" >&2
  exit 2
}

known='TILO_ROOMS_PERSISTENT TILO_ROOMS_NAME TILO_ROOMS_DESCRIPTION TILO_ROOMS_ADMISSION
TILO_ROOMS_WAIT_LIST_MAX TILO_ROOMS_PUBLIC_API TILO_ROOMS_LIVE_ORIGIN
TILO_ROOMS_DEFAULT_ROLE TILO_ROOMS_CHAT_CHANNELS TILO_ROOMS_FEEDBACK_CHANNEL
TILO_ROOMS_SEED_APPS TILO_ROOMS_AUTHOR_APPS TILO_ROOMS_REFOUND TILO_ROOMS_BOTS TILO_ROOMS_LAYOUT
TILO_ROOMS_INACTIVE_AFTER_DAYS TILO_ROOMS_ROOT_FILE TILO_ROOMS_ROLES_FILE
TILO_ROOMS_STATE
TILO_ROOMS_WEB_ENTRY TILO_ROOMS_SITE_ORIGINS TILO_ROOMS_RELAY_ORIGIN
TILO_ROOMS_RELAY_PLAIN TILO_ROOMS_LIVE TILO_ROOMS_LIVE_ORIGIN TILO_ROOMS_WEB
TILO_ROOMS_HTTP TILO_ROOMS_FETCH_PROXY
TILO_ROOMS_MODERATION_KEY_FILE TILO_ROOMS_MODERATION_MODEL
TILO_ROOMS_DISCORD_CLIENT_ID TILO_ROOMS_DISCORD_CLIENT_SECRET_FILE
TILO_ROOMS_DISCORD_REDIRECT_URL TILO_ROOMS_DISCORD_AUTHORIZE_URL
TILO_ROOMS_DISCORD_API_BASE TILO_ROOMS_DISCORD_TOKEN_FILE'
known=$(printf '%s' "$known" | tr '\n' ' ')
for variable in $(env | sed -n 's/^\(TILO_ROOMS_[A-Za-z0-9_]*\)=.*/\1/p'); do
  case " $known " in
    *" $variable "*) ;;
    *) fail "$variable is not a setting this image reads; remove it from the deployment" ;;
  esac
done

name=${TILO_ROOMS_PERSISTENT:-}
[ -n "$name" ] || fail 'TILO_ROOMS_PERSISTENT is required: the workspace this host serves'
root=${TILO_ROOMS_ROOT_FILE:-}
[ -n "$root" ] || fail 'TILO_ROOMS_ROOT_FILE is required: the workspace owner root, 64 hex characters'
origin=${TILO_ROOMS_RELAY_ORIGIN:-}
[ -n "$origin" ] || fail 'TILO_ROOMS_RELAY_ORIGIN is required: the https:// origin the ingress publishes'
description=${TILO_ROOMS_DESCRIPTION:-}
# `invite` (the setting eu1 ships) lists the workspace and closes its door:
# only people invited or pre-admitted join. `open` (the default) lets anyone
# in as the default role. Anything else stops the start.
admission=${TILO_ROOMS_ADMISSION:-open}
case $admission in
  open|invite) ;;
  *) fail "TILO_ROOMS_ADMISSION is open (anyone joins as a guest) or invite (only with an invitation), not $admission" ;;
esac
# A listed invitation-only workspace says what it is; a deployment that names
# no description gets the name people know it by, so eu1's environment starts
# unchanged.
if [ -z "$description" ]; then
  description=${TILO_ROOMS_NAME:-$name}
fi
entry=${TILO_ROOMS_WEB_ENTRY:-}
[ -n "$entry" ] || fail 'TILO_ROOMS_WEB_ENTRY is required: the Tilo app web entry the links open'
origin=${origin%/}
# eu1 also names the API's public address and the live plane's. The API's is
# the relay's own origin here (one origin, one ingress), and the live plane
# rides the plain listener as the HTTPS stream fallback, so neither changes
# what the host serves; both are read so eu1's environment starts as it is.
dir=${TILO_ROOMS_STATE:-/run/tilo-rooms}/$name

# comma_flags VALUE FLAG: FLAG and one comma-separated, trimmed entry of VALUE
# per entry. The settings it splits (origins, app ids, channel names) hold no
# spaces.
comma_flags() {
  rest=$1
  while [ -n "$rest" ]; do
    case $rest in
      *,*) item=${rest%%,*}; rest=${rest#*,} ;;
      *) item=$rest; rest= ;;
    esac
    item=$(printf '%s' "$item" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')
    [ -z "$item" ] || printf '%s %s ' "$2" "$item"
  done
}

set -- room \
  --state "$dir/relay" \
  --persistent "$name" \
  --root-file "$root" \
  --room-id-file "$dir/room-id" \
  --member-limits \
  --room-records \
  --public-avatars "$dir/avatars" \
  --public-avatars-url "$origin/avatars" \
  --listing-description "$description" \
  --default-role "${TILO_ROOMS_DEFAULT_ROLE:-editor}" \
  --layout "${TILO_ROOMS_LAYOUT:-shared}" \
  --inactive-after-days "${TILO_ROOMS_INACTIVE_AFTER_DAYS:-30}" \
  --web "${TILO_ROOMS_WEB:-127.0.0.1:9781}" \
  --packet-relay "${TILO_ROOMS_LIVE:-0.0.0.0:9782}" \
  --tls dev \
  --web-plain "${TILO_ROOMS_RELAY_PLAIN:-0.0.0.0:9780}" \
  --public-origin "$origin" \
  --http "${TILO_ROOMS_HTTP:-0.0.0.0:8790}" \
  --web-entry "$entry" \
  --allowed-origin "$origin"

# By default pages dial the packet relay at the address it bound; an ingress
# that publishes it at another host (its own wss://live.…) names that origin,
# and the certificate the ingress presents names its host too.
if [ -n "${TILO_ROOMS_LIVE_ORIGIN:-}" ]; then
  set -- "$@" --packet-relay-url "${TILO_ROOMS_LIVE_ORIGIN%/}"
fi
if [ -n "${TILO_ROOMS_NAME:-}" ]; then
  set -- "$@" --workspace-name "$TILO_ROOMS_NAME"
fi
# The door: listed either way (the site's directory shows it), open to
# anyone as the default role or only to people invited.
if [ "$admission" = invite ]; then
  set -- "$@" --invitation-only
fi
wait_list=${TILO_ROOMS_WAIT_LIST_MAX:-}
if [ -n "$wait_list" ]; then
  case $wait_list in
    ''|*[!0-9]*) fail "TILO_ROOMS_WAIT_LIST_MAX is a count, not $wait_list" ;;
  esac
  set -- "$@" --wait-list-max "$wait_list"
fi
if [ "${TILO_ROOMS_FETCH_PROXY:-1}" != 0 ]; then
  set -- "$@" --fetch-proxy
fi
# The roles the operator declares beside the workspace's owner root: a file
# the host takes in again whenever it changes, so a role granted while the
# workspace runs reaches it without a restart.
if [ -n "${TILO_ROOMS_ROLES_FILE:-}" ]; then
  set -- "$@" --roles-file "$TILO_ROOMS_ROLES_FILE"
fi

# A chat with no channel has nowhere for a visitor's first message to go, and
# the workspace's apps are its Chat unless the deployment says otherwise.
set -- "$@" $(comma_flags "${TILO_ROOMS_CHAT_CHANNELS-general}" --chat-channel)
set -- "$@" $(comma_flags "${TILO_ROOMS_SEED_APPS-so.tilo.chat}" --seed-app)
set -- "$@" $(comma_flags "${TILO_ROOMS_AUTHOR_APPS:-}" --author-app)
# Records whose retained history lost the base its deltas were written over:
# founded again at start from what the workspace still reads, their old
# history archived beside the replay, never deleted. Named as
# APP:RECORD (an app id holds no `:`), comma-separated.
refound=${TILO_ROOMS_REFOUND:-}
while [ -n "$refound" ]; do
  case $refound in
    *,*) entry=${refound%%,*}; refound=${refound#*,} ;;
    *) entry=$refound; refound= ;;
  esac
  entry=$(printf '%s' "$entry" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')
  [ -z "$entry" ] && continue
  case $entry in
    *:*) set -- "$@" --refound "${entry%%:*}/${entry#*:}" ;;
    *) fail "TILO_ROOMS_REFOUND entry $entry is not APP:RECORD" ;;
  esac
done
set -- "$@" $(comma_flags "${TILO_ROOMS_SITE_ORIGINS:-}" --allowed-origin)
if [ -n "${TILO_ROOMS_FEEDBACK_CHANNEL:-}" ]; then
  set -- "$@" --feedback-channel "$TILO_ROOMS_FEEDBACK_CHANNEL"
fi

# Bot programs the workspace installs at its first start, each a directory
# entry or a program on this host with its inputs: `<source> key=value …`,
# separated by `;` (spaces are part of a spec, so these do not word-split).
bots=${TILO_ROOMS_BOTS:-}
while [ -n "$bots" ]; do
  case $bots in
    *';'*) spec=${bots%%;*}; bots=${bots#*;} ;;
    *) spec=$bots; bots= ;;
  esac
  spec=$(printf '%s' "$spec" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')
  [ -z "$spec" ] || set -- "$@" --bot "$spec"
done

# The moderation agent runs only with a key; the model is the relay's unless
# the deployment names one.
moderation=${TILO_ROOMS_MODERATION_KEY_FILE:-}
if [ -z "$moderation" ] && [ -r /run/secrets/moderation-model-key ]; then
  moderation=/run/secrets/moderation-model-key
fi
if [ -n "$moderation" ]; then
  set -- "$@" --moderation-key-file "$moderation"
  if [ -n "${TILO_ROOMS_MODERATION_MODEL:-}" ]; then
    set -- "$@" --moderation-model "$TILO_ROOMS_MODERATION_MODEL"
  fi
fi

# "Connect Discord": the app's client id and secret, the callback this host's
# --http answers at, and Discord's own API unless the deployment names another.
if [ -n "${TILO_ROOMS_DISCORD_CLIENT_ID:-}" ]; then
  set -- "$@" \
    --discord-client-id "$TILO_ROOMS_DISCORD_CLIENT_ID" \
    --discord-client-secret-file "${TILO_ROOMS_DISCORD_CLIENT_SECRET_FILE:-/run/secrets/discord-client-secret}" \
    --discord-redirect-url "${TILO_ROOMS_DISCORD_REDIRECT_URL:-$origin/discord/callback}" \
    --discord-authorize-url "${TILO_ROOMS_DISCORD_AUTHORIZE_URL:-https://discord.com/oauth2/authorize}"
  if [ -n "${TILO_ROOMS_DISCORD_API_BASE:-}" ]; then
    set -- "$@" --discord-api-base "$TILO_ROOMS_DISCORD_API_BASE"
  fi
fi

# The bridge's bot token matters only while bots run; without the mounted file
# the host is left alone rather than handed a path that does not open.
token=${TILO_ROOMS_DISCORD_TOKEN_FILE:-/run/secrets/discord-bot-token}
if [ -r "$token" ]; then
  set -- "$@" --discord-token-file "$token"
fi

exec tilo-relay "$@"
