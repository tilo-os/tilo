#!/bin/sh
# Writes the entry configuration a bare visit reads (entry.json), then serves.
# Without a home workspace a bare visit opens the person's own desktop; with
# one, it opens that workspace, joined through the entry its host lists it at:
#   TILO_WEB_HOME_HOST  host name of the home workspace's host (unset: no home),
#                       reached over https, or over plain http when it is
#                       this machine (localhost, a name under it, 127.0.0.1)
#   TILO_WEB_HOME_NAME  its name, required with a host
#   TILO_WEB_HOME_APP   the app in front, optional (unset: its desktop)
#   TILO_WEB_HOME_ENTRY  the entry everyone joins it through: an https:// address
#                       (http:// on this machine)
#                       ending in /, which the entry's own paths hang off
#                       (`join` and `errors`; a host's `presence` and wait list
#                       too, where it serves one). Unset: its people reconnect
#                       and others need an invitation, as for a workspace its
#                       host does not list. A workspace a server lists has its
#                       entry at
#                       https://<host>/peer/v1/directory/<workspace>/.
#   TILO_WEB_HOME_HOST_INFO  who that host is and where it is dialled, the one
#                       line of JSON the host writes in `web-host.json`,
#                       required with a host: the page dials the
#                       host as this names it rather than ask the host first.
#                       The site serves it with the page, so the page trusts
#                       it as it trusts itself. It holds while the host keeps
#                       its identity (its root and the device key in its
#                       state) and the packet relay pages reach it through; a
#                       host set up again answers as another, and the page
#                       stops and says this needs setting again.
# Tilo computers list the site's public workspaces from its directory: the
# home host's own (https://<host>/directory) unless one is named:
#   TILO_WEB_DIRECTORY  the directory's https:// address, http:// on this
#                       machine (unset: the home's)
# The site's API server, named in its entry:
#   TILO_WEB_API        the API server's https:// address, http:// on this
#                       machine (unset: none named; such a computer is told to
#                       name one)
# The desktop's About sheet offers these links; an unset one is not offered:
#   TILO_WEB_ABOUT_NOTIFY     where to sign up to hear when Tilo comes out
#   TILO_WEB_ABOUT_SOURCE     the source code
#   TILO_WEB_ABOUT_COMMUNITY  the community
# A system web view on an iPhone or a Mac lets the page ask for a passkey only in
# an app the site itself names. The site names them to Apple's systems at
# /.well-known/apple-app-site-association (the Caddyfile); a site that names
# none has no such file, and its page can use no passkey inside an app:
#   TILO_WEB_APPLE_APPS  the application identifiers of the apps that may use
#                       this site's passkeys, each <team>.<bundle id>, with
#                       spaces between them, e.g.
#                       "ABCDE12345.so.tilo.shell ABCDE12345.so.tilo.assistant"
#                       (unset: none)
# Only the app itself may frame it, and the one site that embeds it as a live
# preview, if any (an embed reports its state to that site alone):
#   TILO_WEB_EMBEDDER   that site's https:// origin (unset: none)
# Where a workspace's app as a web page (/w/…) is rendered (the Caddyfile):
# the server serving this site. It is the server the home workspace is hosted
# on, the one `entry.json` names, unless the deployment reaches it by another
# name or port and names that. With no home workspace and nothing named, the
# site renders no such page.
#   TILO_WEB_SERVER     a server's address as Caddy reaches it, e.g.
#                       https://eu1.example (unset: the home host)
# Where the tilo command's releases are kept, which the site serves at
# /releases/<latest|vX.Y.Z>/<file> for its installer (the Caddyfile): the base
# URL the release tooling (tools/ci/publish/cli-release.sh) publishes under,
# each release at <base>/<version>/<file>. https://, or http:// on this
# machine's loopback for a mirror beside it.
#   TILO_RELEASES_ORIGIN  (unset: the Tilo package registry,
#                       https://code.tilo.so/api/packages/tilo/generic/tilo-cli)
# Where this server's files are and where it listens: the image's own paths
# and port, which a deployment leaves alone. A run of the same entrypoint
# outside the image (os/containers/tilo-sh/stack.sh) names its own:
#   TILO_WEB_SITE       the laid-out site (/srv/tilo-web)
#   TILO_WEB_INSTALL    the directory holding install.sh (/srv/tilo-install)
#   TILO_WEB_RUN        where entry.json is written (/tmp/tilo-web)
#   TILO_WEB_CADDYFILE  the Caddyfile (/etc/caddy/Caddyfile)
#   TILO_WEB_PORT       the port (8080), on every address unless
#   TILO_WEB_BIND       names one
set -eu
# here URL: whether URL is plain http on this machine (localhost, a name under
# it, a 127.x address or [::1]), which a browser treats as secure as https
# and which never leaves the machine.
here() {
  case "$1" in http://*) ;; *) return 1 ;; esac
  authority=${1#http://}
  authority=${authority%%/*}
  case "$authority" in \[*) at=${authority%%]*}] ;; *) at=${authority%:*} ;; esac
  case "$at" in
    localhost | *.localhost | '[::1]') return 0 ;;
    127.*) case "$at" in *[!0-9.]*) return 1 ;; esac; return 0 ;;
  esac
  return 1
}
host=${TILO_WEB_HOME_HOST:-}
name=${TILO_WEB_HOME_NAME:-}
app=${TILO_WEB_HOME_APP:-}
entry=${TILO_WEB_HOME_ENTRY:-}
info=${TILO_WEB_HOME_HOST_INFO:-}
export TILO_WEB_SITE="${TILO_WEB_SITE:-/srv/tilo-web}"
export TILO_WEB_INSTALL="${TILO_WEB_INSTALL:-/srv/tilo-install}"
export TILO_WEB_RUN="${TILO_WEB_RUN:-/tmp/tilo-web}"
caddyfile=${TILO_WEB_CADDYFILE:-/etc/caddy/Caddyfile}
mkdir -p "$TILO_WEB_RUN"
fields=
if [ -z "$host" ]; then
  [ -z "$name$app$entry$info" ] || { echo "TILO_WEB_HOME_NAME, TILO_WEB_HOME_APP, TILO_WEB_HOME_ENTRY and TILO_WEB_HOME_HOST_INFO need TILO_WEB_HOME_HOST" >&2; exit 2; }
else
  # Values land in JSON as they are: only characters that need no escaping.
  case "$host" in *[!a-z0-9.:-]*) echo "TILO_WEB_HOME_HOST must be a host name: $host" >&2; exit 2 ;; esac
  case "$name" in '' | *[!A-Za-z0-9\ ._-]*) echo "TILO_WEB_HOME_NAME is required with a host and may use letters, digits, space, . _ -" >&2; exit 2 ;; esac
  case "$app" in *[!a-z0-9._-]*) echo "TILO_WEB_HOME_APP must be an app id" >&2; exit 2 ;; esac
  case "$entry" in
    '' | https://*) ;;
    *) here "$entry" || { echo "TILO_WEB_HOME_ENTRY must be an https:// address (http:// on this machine): $entry" >&2; exit 2; } ;;
  esac
  case "$entry" in
    '') ;;
    *[!/]) echo "TILO_WEB_HOME_ENTRY must end in /: $entry" >&2; exit 2 ;;
    *[!A-Za-z0-9._~:/?#@!\$\&\'\(\)*+,\;=%-]*) echo "TILO_WEB_HOME_ENTRY must be a plain address: $entry" >&2; exit 2 ;;
  esac
  # One JSON object, written into entry.json as a string: only characters
  # that need no escaping there but its quotes, which are escaped below. A
  # backslash is refused on its own: the image's busybox sh lets one through
  # a bracket expression.
  case "$info" in
    '') echo "TILO_WEB_HOME_HOST_INFO is required with a host: what the host writes in web-host.json" >&2; exit 2 ;;
    \{*\}) ;;
    *) echo "TILO_WEB_HOME_HOST_INFO must be the JSON object the host writes in web-host.json" >&2; exit 2 ;;
  esac
  case "$info" in *\\* | *[!]A-Za-z0-9' {}[":,._/+=-']*) echo "TILO_WEB_HOME_HOST_INFO must be the one line of JSON the host writes in web-host.json" >&2; exit 2 ;; esac
  quoted=$(printf '%s' "$info" | sed 's/"/\\"/g')
  front=
  [ -z "$app" ] || front=",\"app\":\"$app\""
  [ -z "$entry" ] || front="$front,\"admission\":\"$entry\""
  scheme=https
  ! here "http://$host" || scheme=http
  fields=$(printf '"home":{"name":"%s","host":{"origin":"%s://%s","info":"%s"}%s}' "$name" "$scheme" "$host" "$quoted" "$front")
fi
directory=${TILO_WEB_DIRECTORY:-}
if [ -n "$directory" ]; then
  case "$directory" in
    https://*) ;;
    *) here "$directory" || { echo "TILO_WEB_DIRECTORY must be an https:// address (http:// on this machine): $directory" >&2; exit 2; } ;;
  esac
  case "$directory" in *[!A-Za-z0-9._~:/?#@!\$\&\'\(\)*+,\;=%-]*) echo "TILO_WEB_DIRECTORY must be a plain URL: $directory" >&2; exit 2 ;; esac
elif [ -n "$host" ]; then
  directory="$scheme://$host/directory"
fi
[ -z "$directory" ] || fields="$fields${fields:+,}\"directory\":\"$directory\""
api=${TILO_WEB_API:-}
if [ -n "$api" ]; then
  case "$api" in
    https://*) ;;
    *) here "$api" || { echo "TILO_WEB_API must be an https:// address (http:// on this machine): $api" >&2; exit 2; } ;;
  esac
  case "$api" in *[!A-Za-z0-9._~:/?#@!\$\&\'\(\)*+,\;=%-]*) echo "TILO_WEB_API must be a plain URL: $api" >&2; exit 2 ;; esac
  fields="$fields${fields:+,}\"api\":\"$api\""
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
releases=${TILO_RELEASES_ORIGIN:-https://code.tilo.so/api/packages/tilo/generic/tilo-cli}
case "$releases" in
  https://*/*) ;;
  http://*/*) here "$releases" || { echo "TILO_RELEASES_ORIGIN must be an https:// URL with a path (http:// on this machine): $releases" >&2; exit 2; } ;;
  *) echo "TILO_RELEASES_ORIGIN must be an https:// URL with a path: $releases" >&2; exit 2 ;;
esac
case "$releases" in
  */ | *[!A-Za-z0-9._~:/%-]*) echo "TILO_RELEASES_ORIGIN must be a plain URL not ending in /: $releases" >&2; exit 2 ;;
esac
# A workspace's page under /w/ is the server's to render (the Caddyfile): the
# one the home workspace is hosted on, which `entry.json` names, unless the
# deployment reaches it by another name or port and says so.
server=${TILO_WEB_SERVER:-}
if [ -z "$server" ] && [ -n "$host" ]; then
  server="$scheme://$host"
  export TILO_WEB_SERVER="$server"
fi
case "$server" in
  '' | https://*) ;;
  *) here "$server" || { echo "TILO_WEB_SERVER must be an https:// address, or http:// on this machine: $server" >&2; exit 2; } ;;
esac
# An address Caddy proxies to, not a path on it: the request's own path is put
# in front of what the server serves.
case "${server#*://}" in
  */*) echo "TILO_WEB_SERVER names a host and port, not a path: $server" >&2; exit 2 ;;
esac
# Caddy proxies to a scheme and host, and puts the path in front of the request's.
rest=${releases#*://}
# Read by the Caddyfile.
export TILO_RELEASES_UPSTREAM="${releases%%://*}://${rest%%/*}"
export TILO_RELEASES_PATH="/${rest#*/}"
export TILO_WEB_FRAME_ANCESTORS="$ancestors"
# The Apple apps that may use this site's passkeys, as Apple's systems read
# them. Each lands in JSON as it is: a team, ten capitals and digits, then a
# bundle id of letters, digits, dots and hyphens.
apple=${TILO_WEB_APPLE_APPS:-}
case "$apple" in *[!A-Za-z0-9.\ -]*) echo "TILO_WEB_APPLE_APPS names application identifiers like ABCDE12345.so.tilo.shell, with spaces between them: $apple" >&2; exit 2 ;; esac
apps=
for id in $apple; do
  case "$id" in
    [A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9].[A-Za-z0-9]*) ;;
    *) echo "TILO_WEB_APPLE_APPS names application identifiers like ABCDE12345.so.tilo.shell: $id" >&2; exit 2 ;;
  esac
  case "$id" in *..* | *. | *.-* | *-) echo "TILO_WEB_APPLE_APPS names application identifiers like ABCDE12345.so.tilo.shell: $id" >&2; exit 2 ;; esac
  apps="$apps${apps:+,}\"$id\""
done
rm -f "$TILO_WEB_RUN/apple-app-site-association"
[ -z "$apps" ] || printf '{"webcredentials":{"apps":[%s]}}\n' "$apps" > "$TILO_WEB_RUN/apple-app-site-association"
printf '{%s}\n' "$fields" > "$TILO_WEB_RUN/entry.json"
exec caddy run --config "$caddyfile" --adapter caddyfile
