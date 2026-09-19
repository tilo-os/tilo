#!/usr/bin/env bash
set -euo pipefail

image=${1:?usage: test-image.sh IMAGE SOURCE_REVISION}
source_revision=${2:?usage: test-image.sh IMAGE SOURCE_REVISION}

[[ $source_revision =~ ^[0-9a-f]{40}$ ]]
[[ $(docker image inspect --format '{{.Config.User}}' "$image") == 65532:65532 ]]
[[ $(docker image inspect --format '{{index .Config.Labels "org.opencontainers.image.revision"}}' "$image") == "$source_revision" ]]

container="tilo-registry-smoke-${source_revision:0:12}-$$"
volume="${container}-state"
cleanup() {
  docker rm -f "$container" >/dev/null 2>&1 || true
  docker volume rm -f "$volume" >/dev/null 2>&1 || true
}
trap cleanup EXIT
docker volume create "$volume" >/dev/null

# RFC 8032 test-vector public key. Only public identity material enters the
# image; the owner signing seed remains outside the long-running service.
owner_pubkey=d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a
docker run --rm \
  --read-only \
  --cap-drop ALL \
  --security-opt no-new-privileges \
  --tmpfs /tmp:rw,noexec,nosuid,size=16m \
  --mount "type=volume,source=$volume,target=/var/lib/tilo-registry" \
  "$image" \
  init \
  --owner-pubkey "$owner_pubkey" \
  --name "Tilo registry image smoke test" \
  --description "ephemeral" \
  --base-url https://registry.tilo.dev

docker run --detach --name "$container" \
  --read-only \
  --cap-drop ALL \
  --security-opt no-new-privileges \
  --tmpfs /tmp:rw,noexec,nosuid,size=16m \
  --mount "type=volume,source=$volume,target=/var/lib/tilo-registry" \
  --publish 127.0.0.1::8443 \
  "$image" >/dev/null

port=$(docker port "$container" 8443/tcp | awk -F: 'END {print $NF}')
[[ $port =~ ^[0-9]+$ ]]
for _ in $(seq 1 30); do
  if response=$(curl --fail --silent --show-error "http://127.0.0.1:$port/health" 2>/dev/null); then
    [[ $response == '{"ok":true}' ]]
    exit 0
  fi
  sleep 1
done

docker logs "$container" >&2
exit 1
