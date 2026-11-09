#!/bin/sh
# Writes the entry configuration a bare visit reads (entry.json), then serves.
# Without a home workspace a bare visit opens the person's own desktop; with
# one, it opens that workspace, joined through the door its host lists it at:
#   TILO_WEB_HOME_HOST  host name of the home workspace's host (unset: no home)
#   TILO_WEB_HOME_NAME  its name, required with a host
#   TILO_WEB_HOME_APP   the app in front, optional (unset: its desktop)
#   TILO_WEB_HOME_FEEDBACK  the id of its Chat channel members report
#                       problems to, optional (unset: reports are not offered)
#   TILO_WEB_HOME_DOOR  the door everyone joins it through: an https:// address
#                       ending in /, which the door's own paths hang off
#                       (`join` and `errors`; a host's `presence` and wait list
#                       too, where it serves one). Unset: its people reconnect
#                       and others need an invitation, as for a workspace its
#                       host does not list. A workspace a relay lists has its
#                       door at
#                       https://<host>/peer/v1/directory/<workspace>/.
# Tilo computers list the site's public workspaces from its directory: the
# home host's own (https://<host>/directory) unless one is named:
#   TILO_WEB_DIRECTORY  the directory's https:// address (unset: the home's)
# The desktop's About sheet offers these links; an unset one is not offered:
#   TILO_WEB_ABOUT_NOTIFY     where to sign up to hear when Tilo comes out
#   TILO_WEB_ABOUT_SOURCE     the source code
#   TILO_WEB_ABOUT_COMMUNITY  the community
# Only the app itself may frame it, and the one site that embeds it as a live
# preview, if any (an embed reports its state to that site alone):
#   TILO_WEB_EMBEDDER   that site's https:// origin (unset: none)
# Where the site's shelf of sealed invitations is answered (the Caddyfile):
# the host and port of the relay serving this site, which keeps an invitation
# under the handle its link names. Unset, a link still opens the page, and the
# page has no shelf to leave an invitation on or take one from.
#   TILO_WEB_SHELF      a relay's host:port, as Caddy reaches it (unset: none)
# Where the tilo command's releases are kept, which the site serves at
# /releases/<latest|vX.Y.Z>/<file> for its installer (the Caddyfile): the base
# URL the release tooling (tools/ci/publish/cli-release.sh) publishes under,
# each release at <base>/<version>/<file>. https://, or http:// on this
# machine's loopback for a mirror beside it.
#   TILO_RELEASES_ORIGIN  (unset: the Tilo package registry,
#                       https://code.tilo.so/api/packages/tilo/generic/tilo-cli)
set -eu
host=${TILO_WEB_HOME_HOST:-}
name=${TILO_WEB_HOME_NAME:-}
app=${TILO_WEB_HOME_APP:-}
feedback=${TILO_WEB_HOME_FEEDBACK:-}
door=${TILO_WEB_HOME_DOOR:-}
mkdir -p /tmp/tilo-web
fields=
if [ -z "$host" ]; then
  [ -z "$name$app$feedback$door" ] || { echo "TILO_WEB_HOME_NAME, TILO_WEB_HOME_APP, TILO_WEB_HOME_FEEDBACK and TILO_WEB_HOME_DOOR need TILO_WEB_HOME_HOST" >&2; exit 2; }
else
  # Values land in JSON as they are: only characters that need no escaping.
  case "$host" in *[!a-z0-9.:-]*) echo "TILO_WEB_HOME_HOST must be a host name: $host" >&2; exit 2 ;; esac
  case "$name" in '' | *[!A-Za-z0-9\ ._-]*) echo "TILO_WEB_HOME_NAME is required with a host and may use letters, digits, space, . _ -" >&2; exit 2 ;; esac
  case "$app" in *[!a-z0-9._-]*) echo "TILO_WEB_HOME_APP must be an app id" >&2; exit 2 ;; esac
  case "$feedback" in *[!a-z0-9_]*) echo "TILO_WEB_HOME_FEEDBACK must be a channel id like c_feedback" >&2; exit 2 ;; esac
  case "$door" in
    '') ;;
    https://*[!/]) echo "TILO_WEB_HOME_DOOR must end in /: $door" >&2; exit 2 ;;
    https://*) case "$door" in *[!A-Za-z0-9._~:/?#@!\$\&\'\(\)*+,\;=%-]*) echo "TILO_WEB_HOME_DOOR must be a plain https:// address: $door" >&2; exit 2 ;; esac ;;
    *) echo "TILO_WEB_HOME_DOOR must be an https:// address: $door" >&2; exit 2 ;;
  esac
  front=
  [ -z "$app" ] || front=",\"app\":\"$app\""
  [ -z "$feedback" ] || front="$front,\"feedback\":{\"channel\":\"$feedback\"}"
  [ -z "$door" ] || front="$front,\"admission\":\"$door\""
  fields=$(printf '"home":{"name":"%s","host":{"origin":"https://%s"}%s}' "$name" "$host" "$front")
fi
directory=${TILO_WEB_DIRECTORY:-}
if [ -n "$directory" ]; then
  case "$directory" in
    https://*) ;;
    *) echo "TILO_WEB_DIRECTORY must be an https:// address: $directory" >&2; exit 2 ;;
  esac
  case "$directory" in *[!A-Za-z0-9._~:/?#@!\$\&\'\(\)*+,\;=%-]*) echo "TILO_WEB_DIRECTORY must be a plain URL: $directory" >&2; exit 2 ;; esac
elif [ -n "$host" ]; then
  directory="https://$host/directory"
fi
[ -z "$directory" ] || fields="$fields${fields:+,}\"directory\":\"$directory\""
about=
for link in notify:${TILO_WEB_ABOUT_NOTIFY:-} source:${TILO_WEB_ABOUT_SOURCE:-} community:${TILO_WEB_ABOUT_COMMUNITY:-}; do
  key=${link%%:*}
  url=${link#*:}
  [ -n "$url" ] || continue
  case "$url" in
    https://*) ;;
    *) echo "TILO_WEB_ABOUT_* must be https:// URLs: $url" >&2; exit 2 ;;
  esac
  case "$url" in *[!A-Za-z0-9._~:/?#@!\$\&\'\(\)*+,\;=%-]*) echo "TILO_WEB_ABOUT_* must be plain URLs: $url" >&2; exit 2 ;; esac
  about="$about${about:+,}\"$key\":\"$url\""
done
[ -z "$about" ] || fields="$fields${fields:+,}\"about\":{$about}"
embedder=${TILO_WEB_EMBEDDER:-}
ancestors="'self'"
if [ -n "$embedder" ]; then
  case "$embedder" in
    https://*) ;;
    *) echo "TILO_WEB_EMBEDDER must be an https:// origin: $embedder" >&2; exit 2 ;;
  esac
  # An origin: scheme, host and optional port, no path; it lands in JSON and
  # in the CSP header as it is.
  case "${embedder#https://}" in '' | *[!a-z0-9.:-]*) echo "TILO_WEB_EMBEDDER must be an origin like https://site.example: $embedder" >&2; exit 2 ;; esac
  fields="$fields${fields:+,}\"embedder\":\"$embedder\""
  ancestors="$ancestors $embedder"
fi
releases=${TILO_RELEASES_ORIGIN:-https://code.tilo.so/api/packages/tilo/generic/tilo-cli}
case "$releases" in
  https://*/* | http://127.0.0.1:*/* | http://localhost:*/*) ;;
  *) echo "TILO_RELEASES_ORIGIN must be an https:// URL with a path: $releases" >&2; exit 2 ;;
esac
case "$releases" in
  */ | *[!A-Za-z0-9._~:/%-]*) echo "TILO_RELEASES_ORIGIN must be a plain URL not ending in /: $releases" >&2; exit 2 ;;
esac
# Caddy proxies to a scheme and host, and puts the path in front of the request's.
rest=${releases#*://}
# Read by the Caddyfile.
export TILO_RELEASES_UPSTREAM="${releases%%://*}://${rest%%/*}"
export TILO_RELEASES_PATH="/${rest#*/}"
export TILO_WEB_FRAME_ANCESTORS="$ancestors"
printf '{%s}\n' "$fields" > /tmp/tilo-web/entry.json
exec caddy run --config /etc/caddy/Caddyfile --adapter caddyfile
