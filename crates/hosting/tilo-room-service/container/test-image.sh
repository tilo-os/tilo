#!/usr/bin/env bash
# Smoke test of a room-service image before it is published: non-root, labelled, carrying the
# Tilo app's entry pages, its persistent mode refuses to start without the room root (so a
# misconfigured deployment can never mint a different owner), and the persistent room answers
# end to end.
#
# usage: test-image.sh ARCHIVE SOURCE_REVISION
# ARCHIVE is an OCI archive as tools/ci/publish/oci-build.sh writes it (the t check
# image-room-service). It runs in the buildah image as root in a micro-VM step, without a
# daemon; buildah's chroot containers share the step's network, so the service is reached on
# its loopback.
set -euo pipefail
usage='usage: test-image.sh ARCHIVE SOURCE_REVISION'
archive=${1:?$usage}
source_revision=${2:?$usage}
[[ $source_revision =~ ^[0-9a-f]{40}$ ]] || { echo "not a commit: $source_revision" >&2; exit 2; }
. "$(dirname -- "${BASH_SOURCE[0]}")/../../../../tools/ci/publish/lib.sh"
use_buildah room-service-test
image=$(b pull --quiet "oci-archive:$archive")
config() { b inspect --type image --format "$1" "$image"; }
[[ $(config '{{.OCIv1.Config.User}}') == 10001 ]] \
  || { echo 'the image does not run as uid 10001' >&2; exit 1; }
[[ $(config '{{index .OCIv1.Config.Labels "org.opencontainers.image.revision"}}') == "$source_revision" ]] \
  || { echo 'the image is not labelled with its source revision' >&2; exit 1; }
# The running process reports the same revision as the label (/metrics, /mode).
grep -qx "TILO_SOURCE_REVISION=$source_revision" <<<"$(config '{{range .OCIv1.Config.Env}}{{println .}}{{end}}')" \
  || { echo 'TILO_SOURCE_REVISION does not equal the revision label' >&2; exit 1; }

containers=()
service=
cleanup() {
  [[ -z $service ]] || kill "$service" 2>/dev/null || true
  for c in "${containers[@]}"; do b rm "$c" >/dev/null 2>&1 || true; done
}
trap cleanup EXIT
fresh() { local c; c=$(b from --quiet "$image"); containers+=("$c"); printf '%s' "$c"; }

container=$(fresh)
root=$(b mount "$container")
for file in /usr/local/bin/tilo-relay /usr/local/bin/tilo-room-service /usr/local/bin/fonts/manifest.toml \
  /usr/local/share/tilo/bots/discord-bridge.json \
  /srv/tilo-room-assets/index.html /srv/tilo-room-assets/app/index.html /srv/tilo-room-assets/workspaces.mjs /srv/tilo-room-assets/root-identity.mjs /srv/tilo-room-assets/identity-actions.mjs \
  /srv/tilo-room-assets/locales/en-US.ftl /srv/tilo-room-assets/locales/de-DE.ftl; do
  [[ -s $root$file ]] || { echo "missing $file" >&2; exit 1; }
done
b umount "$container" >/dev/null
# Captured first: under pipefail, grep -q closing the pipe early fails the run.
help=$(b run "$container" -- /usr/local/bin/tilo-relay room --help)
grep -q -- '--persistent' <<<"$help" || { echo 'tilo-relay room has no --persistent mode' >&2; exit 1; }

persistent=(--user 10001 --mount type=tmpfs,target=/run/tilo-rooms,tmpfs-mode=1777
  --env TILO_ROOMS_PERSISTENT=plaza --env TILO_ROOMS_RELAY_ORIGIN=https://rooms.example
  --env TILO_ROOMS_WEB_ENTRY=https://app.example/ --env TILO_ROOMS_CHAT_CHANNELS=general,show-and-tell)
output=$(b run "${persistent[@]}" --env TILO_ROOMS_ROOT_FILE=/run/secrets/absent "$(fresh)" -- tilo-room-service 2>&1 || true)
grep -q 'refuses to start without its room root' <<<"$output" \
  || { printf 'persistent mode started without its root:\n%s\n' "$output" >&2; exit 1; }

# The persistent room end to end: a watching page gets a spectator certificate, a visitor is
# pre-admitted as a member, presence answers and the front door leads to the web entry.
hex32() { od -An -tx1 -v -N32 /dev/urandom | tr -d ' \n'; }
log=$scratch/room-service.log
b run "${persistent[@]}" --env TILO_ROOMS_ROOT_FILE=/run/tilo-rooms/root --env "ROOM_ROOT=$(hex32)" \
  "$(fresh)" -- /bin/sh -c 'printf %s "$ROOM_ROOT" > /run/tilo-rooms/root && unset ROOM_ROOT && exec tilo-room-service' \
  > "$log" 2>&1 &
service=$!
fail() { printf '%s\n' "$1" >&2; tail -40 "$log" >&2; exit 1; }
request() {
  local body=${3:-}
  exec 3<>/dev/tcp/127.0.0.1/8790 || return 1
  printf '%s %s HTTP/1.0\r\nContent-Type: application/json\r\nContent-Length: %d\r\n\r\n%s' "$1" "$2" "${#body}" "$body" >&3
  cat <&3
  exec 3<&-
}
device=$(hex32)
answer=
for _ in $(seq 60); do
  answer=$(request POST /spectate "{\"devicePublicKey\":\"$device\"}" 2>/dev/null || true)
  grep -q '"certificate"' <<<"$answer" && break
  kill -0 "$service" 2>/dev/null || fail 'the room service exited'
  sleep 2
done
grep -q '"certificate"' <<<"$answer" || fail "no spectator certificate: $answer"
answer=$(request POST /join "{\"userPublicKey\":\"$(hex32)\",\"displayName\":\"Smoke Test\"}")
grep -q '"admissionToken"' <<<"$answer" || fail "the guest was not pre-admitted: $answer"
answer=$(request GET /presence)
grep -q '"online"' <<<"$answer" || fail "no presence: $answer"
answer=$(request GET /mode)
grep -q "\"revision\":\"$source_revision\"" <<<"$answer" || fail "/mode does not report the source revision: $answer"
answer=$(request GET /)
grep -qi 'location: https://app.example/' <<<"$answer" || fail "/ does not lead to the web entry: $answer"
echo "room-service image ok"
