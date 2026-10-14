#!/usr/bin/env bash
# Smoke test of a room-service image: non-root, labelled, carrying the Tilo
# app's entry pages, and its persistent mode refuses to start without the room root (so a
# misconfigured deployment can never mint a different owner).
set -euo pipefail
usage='usage: test-image.sh IMAGE SOURCE_REVISION'
image=${1:?$usage}
source_revision=${2:?$usage}
[[ $source_revision =~ ^[0-9a-f]{40}$ ]] || { echo "not a commit: $source_revision" >&2; exit 2; }
[[ $(docker image inspect --format '{{.Config.User}}' "$image") == 10001 ]] \
  || { echo 'the image does not run as uid 10001' >&2; exit 1; }
[[ $(docker image inspect --format '{{index .Config.Labels "org.opencontainers.image.revision"}}' "$image") == "$source_revision" ]] \
  || { echo 'the image is not labelled with its source revision' >&2; exit 1; }
# The running process reports the same revision as the label (/metrics, /mode).
env=$(docker image inspect --format '{{range .Config.Env}}{{println .}}{{end}}' "$image")
grep -qx "TILO_SOURCE_REVISION=$source_revision" <<<"$env" \
  || { echo 'TILO_SOURCE_REVISION does not equal the revision label' >&2; exit 1; }
for file in /usr/local/bin/tilo-relay /usr/local/bin/tilo-room-service /usr/local/bin/fonts/manifest.toml \
  /usr/local/share/tilo/bots/discord-bridge.json \
  /srv/tilo-room-assets/index.html /srv/tilo-room-assets/app/index.html /srv/tilo-room-assets/workspaces.mjs /srv/tilo-room-assets/root-identity.mjs /srv/tilo-room-assets/identity-actions.mjs \
  /srv/tilo-room-assets/locales/en-US.ftl /srv/tilo-room-assets/locales/de-DE.ftl; do
  docker run --rm --entrypoint /bin/sh "$image" -c "test -s $file" || { echo "missing $file" >&2; exit 1; }
done
# Captured first: under pipefail, grep -q closing the pipe early fails the run.
help=$(docker run --rm --entrypoint /usr/local/bin/tilo-relay "$image" room --help)
grep -q -- '--persistent' <<<"$help" || { echo 'tilo-relay room has no --persistent mode' >&2; exit 1; }
persistent=(--read-only --tmpfs /run/tilo-rooms:mode=1777
  -e TILO_ROOMS_PERSISTENT=plaza -e TILO_ROOMS_RELAY_ORIGIN=https://rooms.example
  -e TILO_ROOMS_WEB_ENTRY=https://app.example/ -e TILO_ROOMS_CHAT_CHANNELS=general,show-and-tell)
output=$(docker run --rm "${persistent[@]}" -e TILO_ROOMS_ROOT_FILE=/run/secrets/absent "$image" 2>&1 || true)
grep -q 'refuses to start without its room root' <<<"$output" \
  || { printf 'persistent mode started without its root:\n%s\n' "$output" >&2; exit 1; }

# The persistent room end to end: a watching page gets a spectator
# certificate, a visitor is pre-admitted as a member, presence answers and
# the front door leads to the web entry. HTTP is spoken from inside the
# container with bash's /dev/tcp, so this works when the Docker daemon is
# remote too.
hex32() { od -An -tx1 -v -N32 /dev/urandom | tr -d ' \n'; }
container=$(docker run -d "${persistent[@]}" -e TILO_ROOMS_ROOT_FILE=/run/tilo-rooms/root \
  -e ROOM_ROOT="$(hex32)" --entrypoint /bin/sh "$image" \
  -c 'printf %s "$ROOM_ROOT" > /run/tilo-rooms/root && unset ROOM_ROOT && exec tilo-room-service')
trap 'docker logs "$container" 2>&1 | tail -40; docker rm -f "$container" >/dev/null' EXIT
# shellcheck disable=SC2016 # expanded inside the container
http='exec 3<>/dev/tcp/127.0.0.1/8790 || exit 1
printf "%s %s HTTP/1.0\r\nContent-Type: application/json\r\nContent-Length: %d\r\n\r\n%s" "$1" "$2" "${#3}" "$3" >&3
cat <&3'
request() { docker exec "$container" bash -c "$http" request "$1" "$2" "${3:-}"; }
device=$(hex32)
for _ in $(seq 60); do
  answer=$(request POST /spectate "{\"devicePublicKey\":\"$device\"}" 2>/dev/null || true)
  grep -q '"certificate"' <<<"$answer" && break
  sleep 2
done
grep -q '"certificate"' <<<"$answer" || { printf 'no spectator certificate:\n%s\n' "$answer" >&2; exit 1; }
answer=$(request POST /join "{\"userPublicKey\":\"$(hex32)\",\"displayName\":\"Smoke Test\"}")
grep -q '"admissionToken"' <<<"$answer" || { printf 'the guest was not pre-admitted:\n%s\n' "$answer" >&2; exit 1; }
answer=$(request GET /presence)
grep -q '"online"' <<<"$answer" || { printf 'no presence:\n%s\n' "$answer" >&2; exit 1; }
answer=$(request GET /mode)
grep -q "\"revision\":\"$source_revision\"" <<<"$answer" \
  || { printf '/mode does not report the source revision:\n%s\n' "$answer" >&2; exit 1; }
answer=$(request GET /)
grep -qi 'location: https://app.example/'<<<"$answer" || { printf '/ does not lead to the web entry:\n%s\n' "$answer" >&2; exit 1; }
trap - EXIT
docker rm -f "$container" >/dev/null
echo "room-service image ok"
