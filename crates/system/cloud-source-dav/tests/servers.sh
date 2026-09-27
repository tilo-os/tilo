#!/usr/bin/env bash
# Real servers for the container layer of the cloud-source conformance suite:
# Apache mod_dav (a plain WebDAV server) and the official Nextcloud image.
#
#   eval "$(crates/system/cloud-source-dav/tests/servers.sh start)"
#   cargo test --locked -p cloud-source-dav --test conformance container_
#   crates/system/cloud-source-dav/tests/servers.sh stop
#
# `start` prints the TILO_TEST_WEBDAV_* and TILO_TEST_NEXTCLOUD_* variables
# the suite reads; without them the container tests are skipped. Uses rootless
# podman (CONTAINER_ENGINE=docker works too) and binds loopback ports only.
set -euo pipefail

engine=${CONTAINER_ENGINE:-podman}
prefix=tilo-cloud-conformance
user=ada
password=conformance-password
webdav_image=docker.io/bytemark/webdav:latest
nextcloud_image=docker.io/library/nextcloud:31-apache

port_of() {
  "$engine" port "$1" 80/tcp | head -n1 | sed 's/.*://'
}

start() {
  "$engine" run -d --rm --name "$prefix-webdav" -p 127.0.0.1::80 \
    -e AUTH_TYPE=Basic -e USERNAME="$user" -e PASSWORD="$password" \
    "$webdav_image" >/dev/null
  "$engine" run -d --rm --name "$prefix-nextcloud" -p 127.0.0.1::80 \
    -e SQLITE_DATABASE=nextcloud \
    -e NEXTCLOUD_ADMIN_USER="$user" -e NEXTCLOUD_ADMIN_PASSWORD="$password" \
    -e NEXTCLOUD_TRUSTED_DOMAINS="127.0.0.1 localhost" \
    "$nextcloud_image" >/dev/null
  local webdav nextcloud
  webdav=$(port_of "$prefix-webdav")
  nextcloud=$(port_of "$prefix-nextcloud")
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

stop() {
  "$engine" rm -f "$prefix-webdav" "$prefix-nextcloud" >/dev/null 2>&1 || true
}

case "${1:-}" in
  start) start ;;
  stop) stop ;;
  *) echo "usage: $0 start|stop" >&2; exit 2 ;;
esac
