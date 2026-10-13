#!/bin/sh
# Writes the entry configuration a bare visit reads (entry.json), then serves.
# Without a home workspace a bare visit opens the person's own desktop; with
# one, it opens that workspace, joined through its host's admission door:
#   TILO_WEB_HOME_HOST  host name of the home workspace's host (unset: no home)
#   TILO_WEB_HOME_NAME  its name, required with a host
#   TILO_WEB_HOME_APP   the app in front, optional (unset: its desktop)
set -eu
host=${TILO_WEB_HOME_HOST:-}
name=${TILO_WEB_HOME_NAME:-}
app=${TILO_WEB_HOME_APP:-}
mkdir -p /tmp/tilo-web
if [ -z "$host" ]; then
  [ -z "$name$app" ] || { echo "TILO_WEB_HOME_NAME and TILO_WEB_HOME_APP need TILO_WEB_HOME_HOST" >&2; exit 2; }
  echo '{}' > /tmp/tilo-web/entry.json
else
  # Values land in JSON as they are: only characters that need no escaping.
  case "$host" in *[!a-z0-9.:-]*) echo "TILO_WEB_HOME_HOST must be a host name: $host" >&2; exit 2 ;; esac
  case "$name" in '' | *[!A-Za-z0-9\ ._-]*) echo "TILO_WEB_HOME_NAME is required with a host and may use letters, digits, space, . _ -" >&2; exit 2 ;; esac
  case "$app" in *[!a-z0-9._-]*) echo "TILO_WEB_HOME_APP must be an app id" >&2; exit 2 ;; esac
  front=
  [ -z "$app" ] || front=",\"app\":\"$app\""
  printf '{"home":{"name":"%s","host":{"origin":"https://%s"},"admission":"https://%s/"%s}}\n' \
    "$name" "$host" "$host" "$front" > /tmp/tilo-web/entry.json
fi
exec caddy run --config /etc/caddy/Caddyfile --adapter caddyfile
