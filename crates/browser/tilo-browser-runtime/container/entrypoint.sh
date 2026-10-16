#!/bin/sh
# Writes the entry configuration a bare visit reads (entry.json), then serves.
# Without a home workspace a bare visit opens the person's own desktop; with
# one, it opens that workspace, joined through its host's admission door:
#   TILO_WEB_HOME_HOST  host name of the home workspace's host (unset: no home)
#   TILO_WEB_HOME_NAME  its name, required with a host
#   TILO_WEB_HOME_APP   the app in front, optional (unset: its desktop)
# The desktop's About sheet offers these links; an unset one is not offered:
#   TILO_WEB_ABOUT_NOTIFY     where to sign up to hear when Tilo comes out
#   TILO_WEB_ABOUT_SOURCE     the source code
#   TILO_WEB_ABOUT_COMMUNITY  the community
# Only the app itself may frame it, and the one site that embeds it as a live
# preview, if any (an embed reports its state to that site alone):
#   TILO_WEB_EMBEDDER   that site's https:// origin (unset: none)
set -eu
host=${TILO_WEB_HOME_HOST:-}
name=${TILO_WEB_HOME_NAME:-}
app=${TILO_WEB_HOME_APP:-}
mkdir -p /tmp/tilo-web
fields=
if [ -z "$host" ]; then
  [ -z "$name$app" ] || { echo "TILO_WEB_HOME_NAME and TILO_WEB_HOME_APP need TILO_WEB_HOME_HOST" >&2; exit 2; }
else
  # Values land in JSON as they are: only characters that need no escaping.
  case "$host" in *[!a-z0-9.:-]*) echo "TILO_WEB_HOME_HOST must be a host name: $host" >&2; exit 2 ;; esac
  case "$name" in '' | *[!A-Za-z0-9\ ._-]*) echo "TILO_WEB_HOME_NAME is required with a host and may use letters, digits, space, . _ -" >&2; exit 2 ;; esac
  case "$app" in *[!a-z0-9._-]*) echo "TILO_WEB_HOME_APP must be an app id" >&2; exit 2 ;; esac
  front=
  [ -z "$app" ] || front=",\"app\":\"$app\""
  fields=$(printf '"home":{"name":"%s","host":{"origin":"https://%s"},"admission":"https://%s/"%s}' \
    "$name" "$host" "$host" "$front")
fi
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
# Read by the Caddyfile.
export TILO_WEB_FRAME_ANCESTORS="$ancestors"
printf '{%s}\n' "$fields" > /tmp/tilo-web/entry.json
exec caddy run --config /etc/caddy/Caddyfile --adapter caddyfile
