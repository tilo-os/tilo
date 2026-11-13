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
# The host's own HTTPS listener presents a development certificate it makes
# and keeps under the state root, unless the operator mounts a publicly
# trusted one: a PEM chain and its key, set together. The host reads them
# again when the files change, so a renewal reaches it without a restart.
#   TILO_ROOMS_TLS_CERT       the certificate chain (PEM, leaf first)
#   TILO_ROOMS_TLS_KEY        its private key (PEM)
#
# Pages reach the host itself over WebTransport (HTTP/3, UDP), which no
# HTTPS ingress carries: the host listens at TILO_ROOMS_LINK, and pages open
# their sessions at TILO_ROOMS_LINK_ORIGIN, a name leading straight to that
# UDP port. The host presents its TLS certificate there: one a browser
# trusts for that name, or, with the development certificate, one pages pin
# by the hash the host publishes. Unset, the host listens beside
# TILO_ROOMS_WEB at the public origin, which only a host with no ingress
# reaches.
#
# Every workspace this host keeps gives each person who asks workspaces of
# their own, governed here beside the Lounge, keeping between them as many
# gigabytes per person as the operator says:
#   TILO_ROOMS_PERSON_STORAGE_GB  gigabytes per person (default 10)
#
# Everything else has the default below. Any other TILO_ROOMS_* variable stops
# the start: this host does not read it, and a deployment that still sets what
# the host no longer honors must not start believing it does.
set -eu

fail() {
  echo "tilo-lounge: $*" >&2
  exit 2
}

# `TILO_ROOMS_LAYOUT` is read but no longer passed on: every workspace's
# arrangement is its shared desktop record, so the host has no layout flag, and
# a deployment that still names one (eu1 does) starts unchanged.
known='TILO_ROOMS_PERSISTENT TILO_ROOMS_NAME TILO_ROOMS_DESCRIPTION TILO_ROOMS_ADMISSION
TILO_ROOMS_WAIT_LIST_MAX TILO_ROOMS_CONTACT TILO_ROOMS_LOCAL
TILO_ROOMS_MAIL_FROM TILO_ROOMS_RESEND_KEY_FILE TILO_ROOMS_RESEND_URL TILO_ROOMS_MAIL_TEMPLATES
TILO_ROOMS_PUBLIC_API TILO_ROOMS_LIVE_ORIGIN
TILO_ROOMS_DEFAULT_ROLE TILO_ROOMS_CHAT_CHANNELS TILO_ROOMS_FEEDBACK_CHANNEL
TILO_ROOMS_SEED_APPS TILO_ROOMS_AUTHOR_APPS TILO_ROOMS_REFOUND TILO_ROOMS_BOTS TILO_ROOMS_LAYOUT
TILO_ROOMS_ROOT_FILE TILO_ROOMS_ROLES_FILE
TILO_ROOMS_PERSON_STORAGE_GB TILO_ROOMS_IROH_LISTEN
TILO_ROOMS_STATE
TILO_ROOMS_WEB_ENTRY TILO_ROOMS_SITE_ORIGINS TILO_ROOMS_RELAY_ORIGIN
TILO_ROOMS_RELAY_PLAIN TILO_ROOMS_LIVE TILO_ROOMS_LIVE_ORIGIN TILO_ROOMS_WEB
TILO_ROOMS_TLS_CERT TILO_ROOMS_TLS_KEY
TILO_ROOMS_LINK TILO_ROOMS_LINK_ORIGIN
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
tls=dev
if [ -n "${TILO_ROOMS_TLS_CERT:-}" ] || [ -n "${TILO_ROOMS_TLS_KEY:-}" ]; then
  [ -n "${TILO_ROOMS_TLS_CERT:-}" ] && [ -n "${TILO_ROOMS_TLS_KEY:-}" ] \
    || fail "TILO_ROOMS_TLS_CERT and TILO_ROOMS_TLS_KEY are set together, or neither"
  # The host's flag holds both paths, split at the comma.
  case "$TILO_ROOMS_TLS_CERT$TILO_ROOMS_TLS_KEY" in
    *,*) fail "TILO_ROOMS_TLS_CERT and TILO_ROOMS_TLS_KEY are paths without a comma" ;;
  esac
  tls="files:$TILO_ROOMS_TLS_CERT,$TILO_ROOMS_TLS_KEY"
fi

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
  --web "${TILO_ROOMS_WEB:-127.0.0.1:9781}" \
  --packet-relay "${TILO_ROOMS_LIVE:-0.0.0.0:9782}" \
  --tls "$tls" \
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
if [ -n "${TILO_ROOMS_LINK:-}" ] || [ -n "${TILO_ROOMS_LINK_ORIGIN:-}" ]; then
  [ -n "${TILO_ROOMS_LINK:-}" ] && [ -n "${TILO_ROOMS_LINK_ORIGIN:-}" ] \
    || fail "TILO_ROOMS_LINK and TILO_ROOMS_LINK_ORIGIN are set together, or neither"
  set -- "$@" --link-listen "$TILO_ROOMS_LINK" --link-origin "${TILO_ROOMS_LINK_ORIGIN%/}"
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
  # The list is what a door taking people by invitation offers those it turns
  # away: an open door turns nobody away.
  [ "$admission" = invite ] || fail "TILO_ROOMS_WAIT_LIST_MAX needs TILO_ROOMS_ADMISSION=invite"
  set -- "$@" --wait-list-max "$wait_list"
fi
person_storage=${TILO_ROOMS_PERSON_STORAGE_GB:-}
if [ -n "$person_storage" ]; then
  case $person_storage in
    ''|*[!0-9]*) fail "TILO_ROOMS_PERSON_STORAGE_GB is gigabytes per person, not $person_storage" ;;
  esac
  set -- "$@" --person-storage-gb "$person_storage"
fi
# The host's own endpoint at a fixed address, for the operator's devices that
# dial it (the members that take its workspaces' work); otherwise the machine
# picks a port at each start.
if [ -n "${TILO_ROOMS_IROH_LISTEN:-}" ]; then
  set -- "$@" --iroh-listen "$TILO_ROOMS_IROH_LISTEN"
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
fi

# Where Discord's API is: Discord's own unless the deployment names another,
# which the bridge's worker is pointed at too, so a test server serves both the
# sign-in and the bridge.
if [ -n "${TILO_ROOMS_DISCORD_API_BASE:-}" ]; then
  set -- "$@" --discord-api-base "$TILO_ROOMS_DISCORD_API_BASE"
fi

# The bridge's bot token matters only while bots run; without the mounted file
# the host is left alone rather than handed a path that does not open.
token=${TILO_ROOMS_DISCORD_TOKEN_FILE:-/run/secrets/discord-bot-token}
if [ -r "$token" ]; then
  set -- "$@" --discord-token-file "$token"
fi

exec tilo-relay "$@"
