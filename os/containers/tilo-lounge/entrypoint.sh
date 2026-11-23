#!/bin/sh
# The Lounge's host: tilo-server's persistent public workspace, the deployment
# routes beside it (/mode, /avatars/<name>, /discord/callback) and the Discord
# bridge, all in one process, read from the TILO_WORKSPACES_* the deployment sets.
#
# Required:
#   TILO_WORKSPACES_PERSISTENT     the workspace's name; its state lives in
#                             $TILO_WORKSPACES_STATE/$TILO_WORKSPACES_PERSISTENT
#   TILO_WORKSPACES_ROOT_FILE      its owner root, 64 hex characters, mounted from
#                             the operator's secrets
#   TILO_WORKSPACES_SERVER_ORIGIN   the https:// origin the ingress publishes for the
#                             relay: its pages, its avatars and the callback
#   TILO_WORKSPACES_DESCRIPTION    what the public directory says the workspace is;
#                             listed, anyone joins through its entry at
#                             TILO_WORKSPACES_DEFAULT_ROLE
#   TILO_WORKSPACES_WEB_ENTRY      the Tilo app's web entry the links it mints open
#
# The workspace's first owners, by their user public keys (64 hex
# characters each, comma-separated): declared at every start where they are no
# owner yet. Everyone else comes in through the workspace's own actions, which
# its owners and administrators ask with a key of theirs (`tilo workspace people add`,
# `tilo workspace people invite`, the API's /v1/workspaces/{w}/members):
#   TILO_WORKSPACES_OWNERS     the owners' keys
#
# The operator socket takes what only the host's operator does: having
# another host hold a workspace:
#   TILO_WORKSPACES_OPERATOR_SOCKET  that socket (default: the workspace's state
#                               directory, operator.sock)
#
# An operator sheds load without stopping the host by writing `spectator-only`
# or `back-soon` to
# $TILO_WORKSPACES_STATE/$TILO_WORKSPACES_PERSISTENT/server/mode (`normal`
# opens it again), which takes effect without a restart and outlives one;
# `GET /mode` on the host's listener says which one is in force.
#
# The pages reach the host's live session through the host's own iroh packet
# relay, over websockets, which the deployment publishes beside the HTTP
# routes: it binds TILO_WORKSPACES_LIVE, and pages dial TILO_WORKSPACES_LIVE_ORIGIN
# when an ingress terminates TLS in front of it. The HTTPS listener behind the
# ingress binds TILO_WORKSPACES_WEB; an address ending in `:0` takes a port the
# kernel picks, which the host announces in its started event.
#
# The host's own HTTPS listener presents a development certificate it makes
# and keeps under the state root, unless the operator mounts a publicly
# trusted one: a PEM chain and its key, set together. The host reads them
# again when the files change, so a renewal reaches it without a restart.
#   TILO_WORKSPACES_TLS_CERT       the certificate chain (PEM, leaf first)
#   TILO_WORKSPACES_TLS_KEY        its private key (PEM)
#
# Pages reach the host itself over WebTransport (HTTP/3, UDP), which no
# HTTPS ingress carries: the host listens at TILO_WORKSPACES_LINK, and pages open
# their sessions at TILO_WORKSPACES_LINK_ORIGIN, a name leading straight to that
# UDP port. The host presents its TLS certificate there: one a browser
# trusts for that name, or, with the development certificate, one pages pin
# by the hash the host publishes. Unset, the host listens beside
# TILO_WORKSPACES_WEB at the public origin, which only a host with no ingress
# reaches.
#
# A front that publishes this same server under a second name (its name on
# another network, beside the ingress's) is named here. The server takes a
# name it was not told for another server, and refuses the setup code a page
# that came by that name asks for its person's agent:
#   TILO_WORKSPACES_SERVER_ORIGINS_ALSO  the other origins the server is reached
#                                   under, comma-separated
#
# Every workspace this host keeps gives each person who asks workspaces of
# their own, governed here beside the Lounge, keeping between them as many
# gigabytes per person as the operator says:
#   TILO_WORKSPACES_PERSON_STORAGE_GB  gigabytes per person (default 10)
#
# Everything else has the default below. Any other TILO_WORKSPACES_* variable stops
# the start: this host does not read it, and a deployment that still sets what
# the host no longer honors must not start believing it does.
set -eu

fail() {
  echo "tilo-lounge: $*" >&2
  exit 2
}

# `TILO_WORKSPACES_LAYOUT` is read but no longer passed on: every workspace's
# layout is its shared desktop record, so the host has no layout flag, and
# a deployment that still names one (eu1 does) starts unchanged.
known='TILO_WORKSPACES_PERSISTENT TILO_WORKSPACES_NAME TILO_WORKSPACES_DESCRIPTION TILO_WORKSPACES_ADMISSION
TILO_WORKSPACES_WAIT_LIST_MAX TILO_WORKSPACES_CONTACT TILO_WORKSPACES_LOCAL
TILO_WORKSPACES_MAIL_FROM TILO_WORKSPACES_RESEND_KEY_FILE TILO_WORKSPACES_RESEND_URL TILO_WORKSPACES_MAIL_TEMPLATES
TILO_WORKSPACES_PUBLIC_API TILO_WORKSPACES_LIVE_ORIGIN
TILO_WORKSPACES_DEFAULT_ROLE TILO_WORKSPACES_CHAT_CHANNELS
TILO_WORKSPACES_SEED_APPS TILO_WORKSPACES_REFOUND TILO_WORKSPACES_BOTS TILO_WORKSPACES_LAYOUT
TILO_WORKSPACES_ROOT_FILE TILO_WORKSPACES_OPERATOR_SOCKET
TILO_WORKSPACES_PERSON_STORAGE_GB TILO_WORKSPACES_IROH_LISTEN TILO_WORKSPACES_HOLD_FOR TILO_WORKSPACES_CALL_OWN_NETWORK TILO_WORKSPACES_OWNERS
TILO_WORKSPACES_OPENAI_SIGN_IN TILO_WORKSPACES_CHATGPT_BACKEND TILO_WORKSPACES_OPENAI_API
TILO_WORKSPACES_STATE
TILO_WORKSPACES_WEB_ENTRY TILO_WORKSPACES_SITE_ORIGINS TILO_WORKSPACES_SERVER_ORIGIN
TILO_WORKSPACES_SERVER_ORIGINS_ALSO
TILO_WORKSPACES_SERVER_PLAIN TILO_WORKSPACES_LIVE TILO_WORKSPACES_LIVE_ORIGIN TILO_WORKSPACES_WEB
TILO_WORKSPACES_TLS_CERT TILO_WORKSPACES_TLS_KEY
TILO_WORKSPACES_LINK TILO_WORKSPACES_LINK_ORIGIN
TILO_WORKSPACES_HTTP TILO_WORKSPACES_FETCH_PROXY
TILO_WORKSPACES_MODERATION_KEY_FILE TILO_WORKSPACES_MODERATION_MODEL
TILO_WORKSPACES_DISCORD_CLIENT_ID TILO_WORKSPACES_DISCORD_CLIENT_SECRET_FILE
TILO_WORKSPACES_DISCORD_REDIRECT_URL TILO_WORKSPACES_DISCORD_AUTHORIZE_URL
TILO_WORKSPACES_DISCORD_API_BASE TILO_WORKSPACES_DISCORD_TOKEN_FILE'
known=$(printf '%s' "$known" | tr '\n' ' ')
for variable in $(env | sed -n 's/^\(TILO_WORKSPACES_[A-Za-z0-9_]*\)=.*/\1/p'); do
  case " $known " in
    *" $variable "*) ;;
    *) fail "$variable is not a setting this image reads; remove it from the deployment" ;;
  esac
done

name=${TILO_WORKSPACES_PERSISTENT:-}
[ -n "$name" ] || fail 'TILO_WORKSPACES_PERSISTENT is required: the workspace this host serves'
root=${TILO_WORKSPACES_ROOT_FILE:-}
[ -n "$root" ] || fail 'TILO_WORKSPACES_ROOT_FILE is required: the workspace owner root, 64 hex characters'
origin=${TILO_WORKSPACES_SERVER_ORIGIN:-}
[ -n "$origin" ] || fail 'TILO_WORKSPACES_SERVER_ORIGIN is required: the https:// origin the ingress publishes'
description=${TILO_WORKSPACES_DESCRIPTION:-}
# `invite` (the setting eu1 ships) lists the workspace and closes its entry:
# only people invited or pre-admitted join. `open` (the default) lets anyone
# in as the default role. Anything else stops the start.
admission=${TILO_WORKSPACES_ADMISSION:-open}
case $admission in
  open|invite) ;;
  *) fail "TILO_WORKSPACES_ADMISSION is open (anyone joins as a guest) or invite (only with an invitation), not $admission" ;;
esac
# A listed invitation-only workspace says what it is; a deployment that names
# no description gets the name people know it by, so eu1's environment starts
# unchanged.
if [ -z "$description" ]; then
  description=${TILO_WORKSPACES_NAME:-$name}
fi
entry=${TILO_WORKSPACES_WEB_ENTRY:-}
[ -n "$entry" ] || fail 'TILO_WORKSPACES_WEB_ENTRY is required: the Tilo app web entry the links open'
origin=${origin%/}
# eu1 also names the API's public address and the live session's. The API's is
# the server's own origin here (one origin, one ingress), and the live session
# rides the plain listener as the HTTPS stream fallback, so neither changes
# what the host serves; both are read so eu1's environment starts as it is.
dir=${TILO_WORKSPACES_STATE:-/run/tilo-workspaces}/$name
tls=dev
if [ -n "${TILO_WORKSPACES_TLS_CERT:-}" ] || [ -n "${TILO_WORKSPACES_TLS_KEY:-}" ]; then
  [ -n "${TILO_WORKSPACES_TLS_CERT:-}" ] && [ -n "${TILO_WORKSPACES_TLS_KEY:-}" ] \
    || fail "TILO_WORKSPACES_TLS_CERT and TILO_WORKSPACES_TLS_KEY are set together, or neither"
  # The host's flag holds both paths, split at the comma.
  case "$TILO_WORKSPACES_TLS_CERT$TILO_WORKSPACES_TLS_KEY" in
    *,*) fail "TILO_WORKSPACES_TLS_CERT and TILO_WORKSPACES_TLS_KEY are paths without a comma" ;;
  esac
  tls="files:$TILO_WORKSPACES_TLS_CERT,$TILO_WORKSPACES_TLS_KEY"
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

set -- workspace \
  --state "$dir/server" \
  --persistent "$name" \
  --root-file "$root" \
  --workspace-id-file "$dir/workspace-id" \
  --workspace-documents \
  --public-avatars "$dir/avatars" \
  --public-avatars-url "$origin/avatars" \
  --listing-description "$description" \
  --default-role "${TILO_WORKSPACES_DEFAULT_ROLE:-editor}" \
  --web "${TILO_WORKSPACES_WEB:-127.0.0.1:9781}" \
  --tls "$tls" \
  --web-plain "${TILO_WORKSPACES_SERVER_PLAIN:-0.0.0.0:9780}" \
  --public-origin "$origin" \
  --http "${TILO_WORKSPACES_HTTP:-0.0.0.0:8790}" \
  --web-entry "$entry" \
  --allowed-origin "$origin"

# The server has its own packet relay, unless TILO_WORKSPACES_LIVE=none: then
# it is reached through the one at TILO_WORKSPACES_LIVE_ORIGIN, as a server
# behind a router nobody can dial into is.
live=${TILO_WORKSPACES_LIVE:-0.0.0.0:9782}
if [ "$live" = none ]; then
  [ -n "${TILO_WORKSPACES_LIVE_ORIGIN:-}" ] \
    || fail "TILO_WORKSPACES_LIVE=none names the packet relay to be reached through in TILO_WORKSPACES_LIVE_ORIGIN"
else
  set -- "$@" --packet-relay "$live"
fi
# By default pages dial the packet relay at the address it bound; an ingress
# that publishes it at another host (its own wss://live.…) names that origin,
# and the certificate the ingress presents names its host too.
if [ -n "${TILO_WORKSPACES_LIVE_ORIGIN:-}" ]; then
  set -- "$@" --packet-relay-url "${TILO_WORKSPACES_LIVE_ORIGIN%/}"
fi
if [ -n "${TILO_WORKSPACES_LINK:-}" ] || [ -n "${TILO_WORKSPACES_LINK_ORIGIN:-}" ]; then
  [ -n "${TILO_WORKSPACES_LINK:-}" ] && [ -n "${TILO_WORKSPACES_LINK_ORIGIN:-}" ] \
    || fail "TILO_WORKSPACES_LINK and TILO_WORKSPACES_LINK_ORIGIN are set together, or neither"
  set -- "$@" --link-listen "$TILO_WORKSPACES_LINK" --link-origin "${TILO_WORKSPACES_LINK_ORIGIN%/}"
fi
if [ -n "${TILO_WORKSPACES_NAME:-}" ]; then
  set -- "$@" --workspace-name "$TILO_WORKSPACES_NAME"
fi
# The entry: listed either way (the site's directory shows it), open to
# anyone as the default role or only to people invited.
if [ "$admission" = invite ]; then
  set -- "$@" --invitation-only
fi
wait_list=${TILO_WORKSPACES_WAIT_LIST_MAX:-}
if [ -n "$wait_list" ]; then
  case $wait_list in
    ''|*[!0-9]*) fail "TILO_WORKSPACES_WAIT_LIST_MAX is a count, not $wait_list" ;;
  esac
  # The list is what an entry taking people by invitation offers those it turns
  # away: an open entry turns nobody away.
  [ "$admission" = invite ] || fail "TILO_WORKSPACES_WAIT_LIST_MAX needs TILO_WORKSPACES_ADMISSION=invite"
  set -- "$@" --wait-list-max "$wait_list"
fi
person_storage=${TILO_WORKSPACES_PERSON_STORAGE_GB:-}
if [ -n "$person_storage" ]; then
  case $person_storage in
    ''|*[!0-9]*) fail "TILO_WORKSPACES_PERSON_STORAGE_GB is gigabytes per person, not $person_storage" ;;
  esac
  set -- "$@" --person-storage-gb "$person_storage"
fi
# The host's own endpoint at a fixed address, for the operator's devices that
# dial it (the members that take its workspaces' work); otherwise the machine
# picks a port at each start.
if [ -n "${TILO_WORKSPACES_IROH_LISTEN:-}" ]; then
  set -- "$@" --iroh-listen "$TILO_WORKSPACES_IROH_LISTEN"
fi
# The workspaces of other people this host holds when the host governing one
# hands it over: `anyone`, or their ids, comma-separated.
if [ "${TILO_WORKSPACES_CALL_OWN_NETWORK:-}" = 1 ]; then
  set -- "$@" --call-own-network
fi
# Where people sign in to OpenAI and reach their ChatGPT plan, in place of
# OpenAI's own addresses: a local stack's fake.
if [ -n "${TILO_WORKSPACES_OPENAI_SIGN_IN:-}" ]; then
  set -- "$@" --openai-sign-in "$TILO_WORKSPACES_OPENAI_SIGN_IN"
fi
if [ -n "${TILO_WORKSPACES_CHATGPT_BACKEND:-}" ]; then
  set -- "$@" --chatgpt-backend "$TILO_WORKSPACES_CHATGPT_BACKEND"
fi
if [ -n "${TILO_WORKSPACES_OPENAI_API:-}" ]; then
  set -- "$@" --openai-api "$TILO_WORKSPACES_OPENAI_API"
fi
if [ -n "${TILO_WORKSPACES_HOLD_FOR:-}" ]; then
  set -- "$@" --hold-for "$TILO_WORKSPACES_HOLD_FOR"
fi
if [ "${TILO_WORKSPACES_FETCH_PROXY:-1}" != 0 ]; then
  set -- "$@" --fetch-proxy
fi
# The workspace's first owners, declared at every start where they are not
# yet: how a fresh host gets the person who lets everyone else in.
set -- "$@" $(comma_flags "${TILO_WORKSPACES_OWNERS-}" --owner)
# Where the operator asks the running workspace what only the host's operator
# does: a UNIX socket in its state directory, which a container has instead of
# a stdin to write to.
set -- "$@" --operator-socket "${TILO_WORKSPACES_OPERATOR_SOCKET:-$dir/operator.sock}"

# A chat with no channel has nowhere for a visitor's first message to go. Chat
# is a tilolang app whose record its first change founds, so no app is seeded
# unless the deployment names one.
set -- "$@" $(comma_flags "${TILO_WORKSPACES_CHAT_CHANNELS-general}" --chat-channel)
set -- "$@" $(comma_flags "${TILO_WORKSPACES_SEED_APPS-}" --seed-app)
# Records whose retained history lost the base its deltas were written over:
# founded again at start from what the workspace still reads, their old
# history archived beside the replay, never deleted. Named as
# APP:RECORD (an app id holds no `:`), comma-separated.
refound=${TILO_WORKSPACES_REFOUND:-}
while [ -n "$refound" ]; do
  case $refound in
    *,*) entry=${refound%%,*}; refound=${refound#*,} ;;
    *) entry=$refound; refound= ;;
  esac
  entry=$(printf '%s' "$entry" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')
  [ -z "$entry" ] && continue
  case $entry in
    *:*) set -- "$@" --refound "${entry%%:*}/${entry#*:}" ;;
    *) fail "TILO_WORKSPACES_REFOUND entry $entry is not APP:RECORD" ;;
  esac
done
set -- "$@" $(comma_flags "${TILO_WORKSPACES_SITE_ORIGINS:-}" --allowed-origin)
set -- "$@" $(comma_flags "${TILO_WORKSPACES_SERVER_ORIGINS_ALSO:-}" --public-origin-also)

# Bot programs the workspace installs at its first start, each a directory
# entry or a program on this host with its inputs: `<source> key=value …`,
# separated by `;` (spaces are part of a spec, so these do not word-split).
bots=${TILO_WORKSPACES_BOTS:-}
while [ -n "$bots" ]; do
  case $bots in
    *';'*) spec=${bots%%;*}; bots=${bots#*;} ;;
    *) spec=$bots; bots= ;;
  esac
  spec=$(printf '%s' "$spec" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')
  [ -z "$spec" ] || set -- "$@" --bot "$spec"
done

# The moderation agent runs only with a key; the model is the server's unless
# the deployment names one.
moderation=${TILO_WORKSPACES_MODERATION_KEY_FILE:-}
if [ -z "$moderation" ] && [ -r /run/secrets/moderation-model-key ]; then
  moderation=/run/secrets/moderation-model-key
fi
if [ -n "$moderation" ]; then
  set -- "$@" --moderation-key-file "$moderation"
  if [ -n "${TILO_WORKSPACES_MODERATION_MODEL:-}" ]; then
    set -- "$@" --moderation-model "$TILO_WORKSPACES_MODERATION_MODEL"
  fi
fi

# "Connect Discord": the app's client id and secret, the callback this host's
# --http answers at, and Discord's own API unless the deployment names another.
if [ -n "${TILO_WORKSPACES_DISCORD_CLIENT_ID:-}" ]; then
  set -- "$@" \
    --discord-client-id "$TILO_WORKSPACES_DISCORD_CLIENT_ID" \
    --discord-client-secret-file "${TILO_WORKSPACES_DISCORD_CLIENT_SECRET_FILE:-/run/secrets/discord-client-secret}" \
    --discord-redirect-url "${TILO_WORKSPACES_DISCORD_REDIRECT_URL:-$origin/discord/callback}" \
    --discord-authorize-url "${TILO_WORKSPACES_DISCORD_AUTHORIZE_URL:-https://discord.com/oauth2/authorize}"
fi

# Where Discord's API is: Discord's own unless the deployment names another,
# which the bridge's worker is pointed at too, so a test server serves both the
# sign-in and the bridge.
if [ -n "${TILO_WORKSPACES_DISCORD_API_BASE:-}" ]; then
  set -- "$@" --discord-api-base "$TILO_WORKSPACES_DISCORD_API_BASE"
fi

# The bridge's bot token matters only while bots run; without the mounted file
# the host is left alone rather than handed a path that does not open.
token=${TILO_WORKSPACES_DISCORD_TOKEN_FILE:-/run/secrets/discord-bot-token}
if [ -r "$token" ]; then
  set -- "$@" --discord-token-file "$token"
fi

exec tilo-server "$@"
