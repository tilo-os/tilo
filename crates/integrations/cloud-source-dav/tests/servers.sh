#!/usr/bin/env bash
# Real servers for the container layer of the cloud-source conformance suite:
# Apache mod_dav (a plain WebDAV server) and the official Nextcloud image on
# PostgreSQL (on SQLite it answers 503 when parallel clients wait on its lock).
#
#   eval "$(crates/integrations/cloud-source-dav/tests/servers.sh start)"
#   cargo test --locked -p tilo-cloud-source-dav --test conformance container_
#   crates/integrations/cloud-source-dav/tests/servers.sh stop
#
# `start` prints the TILO_TEST_WEBDAV_* and TILO_TEST_NEXTCLOUD_* variables
# the suite reads; without them the container tests are skipped. Uses rootless
# podman (CONTAINER_ENGINE=docker works too) and binds loopback ports only.
#
# `seed` puts a person's Nextcloud on the running server, for a test that walks
# someone setting one up rather than talking to its DAV endpoints: Ada's
# address, a second person Grace whose address an invitation can name, a
# calendar of each, and the files Ada keeps there. It prints the extra
# variables such a test reads, and is safe to repeat.
set -euo pipefail

engine=${CONTAINER_ENGINE:-podman}
prefix=tilo-cloud-conformance
user=ada
password=conformance-password
webdav_image=docker.io/bytemark/webdav:latest
nextcloud_image=docker.io/library/nextcloud:31-apache
postgres_image=docker.io/library/postgres:17-alpine
# The seed's second person and both people's addresses.
grace=grace
ada_email=ada@example.test
grace_email=grace@example.test

port_of() {
  "$engine" port "$1" 80/tcp | head -n1 | sed 's/.*://'
}

nextcloud_url() {
  echo "http://127.0.0.1:$(port_of "$prefix-nextcloud-db")"
}

start() {
  "$engine" run -d --rm --name "$prefix-webdav" -p 127.0.0.1::80 \
    -e AUTH_TYPE=Basic -e USERNAME="$user" -e PASSWORD="$password" \
    "$webdav_image" >/dev/null
  # Nextcloud joins the database's network namespace, which publishes its port.
  "$engine" run -d --rm --name "$prefix-nextcloud-db" -p 127.0.0.1::80 \
    -e POSTGRES_DB=nextcloud -e POSTGRES_USER=nextcloud -e POSTGRES_PASSWORD="$password" \
    "$postgres_image" >/dev/null
  "$engine" run -d --rm --name "$prefix-nextcloud" --network "container:$prefix-nextcloud-db" \
    -e POSTGRES_HOST=127.0.0.1 -e POSTGRES_DB=nextcloud \
    -e POSTGRES_USER=nextcloud -e POSTGRES_PASSWORD="$password" \
    -e NEXTCLOUD_ADMIN_USER="$user" -e NEXTCLOUD_ADMIN_PASSWORD="$password" \
    -e NEXTCLOUD_TRUSTED_DOMAINS="127.0.0.1 localhost" \
    "$nextcloud_image" >/dev/null
  local webdav nextcloud
  webdav=$(port_of "$prefix-webdav")
  nextcloud=$(port_of "$prefix-nextcloud-db")
  # Both refuse connections until they serve; Nextcloud installs itself on
  # first start before Apache listens. curl's own retry bounds the wait.
  curl -sS -o /dev/null --retry 60 --retry-connrefused --retry-delay 1 \
    -u "$user:$password" "http://127.0.0.1:$webdav/" >&2
  curl -fsS -o /dev/null --retry 300 --retry-all-errors --retry-delay 1 \
    "http://127.0.0.1:$nextcloud/status.php" >&2
  cat <<EOF
export TILO_TEST_WEBDAV_URL=http://127.0.0.1:$webdav/
export TILO_TEST_WEBDAV_USER=$user
export TILO_TEST_WEBDAV_PASSWORD=$password
export TILO_TEST_NEXTCLOUD_URL=http://127.0.0.1:$nextcloud/
export TILO_TEST_NEXTCLOUD_USER=$user
export TILO_TEST_NEXTCLOUD_PASSWORD=$password
EOF
}

# An answer from Nextcloud's provisioning API: the JSON body it read, and
# `die` when it did not do what the seed asked. 100 is done, 101 and 102 are
# "already so", which seeding a second time gets.
ocs() {
  local method=$1 path=$2
  shift 2
  local base body
  base=$(nextcloud_url)
  body=$(curl -sS -u "$user:$password" -H 'OCS-APIRequest: true' \
    -H 'Accept: application/json' -X "$method" "$base$path" "$@")
  case $body in
    *'"statuscode":100'*|*'"statuscode":101'*|*'"statuscode":102'*) ;;
    *) echo "seed: $method $path was refused: $body" >&2; exit 1 ;;
  esac
}

# A folder on Ada's files, and a file in it. 405 is "already there".
dav_mkdir() {
  local base code
  base=$(nextcloud_url)
  code=$(curl -sS -o /dev/null -w '%{http_code}' -u "$user:$password" -X MKCOL \
    "$base/remote.php/dav/files/$user/$1/")
  case $code in 201|405) ;; *) echo "seed: making $1 answered $code" >&2; exit 1 ;; esac
}

dav_put() {
  local base code
  base=$(nextcloud_url)
  code=$(curl -sS -o /dev/null -w '%{http_code}' -u "$user:$password" -T - \
    "$base/remote.php/dav/files/$user/$1" < "$2")
  case $code in 200|201|204) ;; *) echo "seed: writing $1 answered $code" >&2; exit 1 ;; esac
}

# A calendar in someone's calendar home. Reading the home first makes Nextcloud
# lay it out for an account that has never answered a DAV request; 201 made it,
# 405 was already there.
dav_calendar() {
  local who=$1 name=$2 label=$3 base code
  base=$(nextcloud_url)
  curl -sS -o /dev/null -u "$who:$password" -X PROPFIND -H 'Depth: 0' \
    "$base/remote.php/dav/calendars/$who/"
  code=$(curl -sS -o /dev/null -w '%{http_code}' -u "$who:$password" -X MKCALENDAR \
    -H 'Content-Type: application/xml' --data-binary @- \
    "$base/remote.php/dav/calendars/$who/$name/" <<XML
<?xml version="1.0" encoding="utf-8"?>
<C:mkcalendar xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:set><D:prop><D:displayname>$label</D:displayname></D:prop></D:set>
</C:mkcalendar>
XML
)
  case $code in 201|405) ;; *) echo "seed: $who's $name calendar answered $code" >&2; exit 1 ;; esac
}

seed() {
  local base files
  base=$(nextcloud_url)
  files=$(mktemp -d)
  # shellcheck disable=SC2064
  trap "rm -rf '$files'" EXIT
  # Ada's address, so an invitation she sends names the account that made it.
  ocs PUT "/ocs/v1.php/cloud/users/$user" \
    --data-urlencode key=email --data-urlencode value="$ada_email"
  # Grace is a second person on the server: an invitation the calendar server
  # delivers has somewhere of hers to go, and she can sign in herself.
  ocs POST /ocs/v1.php/cloud/users \
    --data-urlencode userid="$grace" --data-urlencode password="$password"
  ocs PUT "/ocs/v1.php/cloud/users/$grace" \
    --data-urlencode key=email --data-urlencode value="$grace_email"
  # A calendar of each, so an invitation has a calendar to land in.
  dav_calendar "$user" personal Personal
  dav_calendar "$grace" personal Personal
  # The files Ada keeps in Nextcloud.
  dav_mkdir Documents
  dav_mkdir "Documents/Plans"
  dav_mkdir Photos
  printf 'Spring planting\n' > "$files/plan.txt"
  printf 'Seeds, soil, string\n' > "$files/list.txt"
  dav_put Documents/plan.txt "$files/plan.txt"
  dav_put Documents/Plans/list.txt "$files/list.txt"
  printf 'Photo of the garden\n' > "$files/garden.txt"
  dav_put Photos/garden.txt "$files/garden.txt"
  cat <<EOF
export TILO_TEST_NEXTCLOUD_SEEDED=1
export TILO_TEST_NEXTCLOUD_ADA_EMAIL=$ada_email
export TILO_TEST_NEXTCLOUD_GRACE_USER=$grace
export TILO_TEST_NEXTCLOUD_GRACE_EMAIL=$grace_email
export TILO_TEST_NEXTCLOUD_CALDAV=$base/remote.php/dav
export TILO_TEST_NEXTCLOUD_FILES=$base/remote.php/dav/files/$user
EOF
}

stop() {
  "$engine" rm -f "$prefix-webdav" "$prefix-nextcloud" "$prefix-nextcloud-db" >/dev/null 2>&1 || true
}

case "${1:-}" in
  start) start ;;
  seed) seed ;;
  stop) stop ;;
  *) echo "usage: $0 start|seed|stop" >&2; exit 2 ;;
esac
