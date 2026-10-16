#!/usr/bin/env bash
# The registry server image before it is published: non-root, Tilo's trust bundle at the fixed
# path, the reviewed offline-root commands and not the removed one, and a server that
# initializes a fresh state directory and answers /health.
#
# usage: test-image.sh ARCHIVE
#        test-image.sh IMAGE SOURCE_REVISION
# ARCHIVE is an OCI archive as tools/ci/publish/oci-build.sh writes it (the t check
# image-registry-server). It runs in the buildah image, like oci-build.sh, without a daemon,
# and checks no revision label: one build serves every commit with the same inputs, so the
# digest receipt names the commit. IMAGE is a tag in the local Docker daemon, labelled with
# SOURCE_REVISION (the Forgejo workflow release-registry-server.yml).
set -euo pipefail
usage='usage: test-image.sh ARCHIVE | test-image.sh IMAGE SOURCE_REVISION'

test_archive() {
  local archive=$1
  . "$(dirname -- "${BASH_SOURCE[0]}")/../../../../tools/ci/publish/lib.sh"
  use_buildah registry-server-test
  image=$(b pull --quiet "oci-archive:$archive")
  [[ $(b inspect --type image --format '{{.OCIv1.Config.User}}' "$image") == 65532:65532 ]]

  container=$(b from --quiet "$image")
  server=
  cleanup() {
    [[ -z $server ]] || kill "$server" 2>/dev/null || true
    b rm "$container" >/dev/null 2>&1 || true
  }
  trap cleanup EXIT

  # Moderation downloads use Tilo's fixed native-root path. Verify the runtime image actually
  # carries a non-empty bundle there; distroless images use a different distro path by default.
  root=$(b mount "$container")
  [[ -s $root/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem ]] \
    || die 'runtime image is missing the Tilo CA trust bundle'

  # The deployed binary must expose the reviewed offline-root protocol. Exercise Clap from the
  # image itself so a stale or partial server build cannot publish.
  server_bin=/usr/local/bin/tilo-registry-server
  for command in \
    authorize-channel \
    collect-first-party \
    publish-staged-first-party \
    sign-delegated-snapshot
  do
    help=$(b run "$container" -- "$server_bin" "$command" --help)
    [[ $help == *"Usage:"* ]]
  done
  if b run "$container" -- "$server_bin" promote-first-party --help >/dev/null 2>&1; then
    die 'removed unsafe promote-first-party command is still present'
  fi

  # Kubernetes makes the volume writable through fsGroup=65532; model that here.
  state=$scratch/registry-state
  rm -rf "$state"
  mkdir -m 0750 "$state"
  chown 65532:65532 "$state"
  mount=(-v "$state:/var/lib/tilo-registry")

  # RFC 8032 test-vector public key. Only public identity material enters the image; the owner
  # signing seed remains outside the long-running service.
  owner_pubkey=d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a
  b run "${mount[@]}" "$container" -- "$server_bin" \
    --data-dir /var/lib/tilo-registry \
    init \
    --owner-pubkey "$owner_pubkey" \
    --name "Tilo registry image smoke test" \
    --description "ephemeral" \
    --base-url https://apps.tilo.so

  # Chroot isolation shares this check's network namespace, so the server's port is this shell's.
  b run "${mount[@]}" "$container" -- "$server_bin" \
    --data-dir /var/lib/tilo-registry serve --bind 127.0.0.1 --port 8443 &
  server=$!
  health() {
    exec 3<>/dev/tcp/127.0.0.1/8443 || return
    printf 'GET /health HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n' >&3
    cat <&3
  }
  for _ in $(seq 1 30); do
    if response=$(health 2>/dev/null); then
      body=${response##*$'\r\n\r\n'}
      [[ $body == '{"ok":true}' ]] || die "unexpected /health answer: $response"
      printf 'registry server image answers /health\n'
      exit 0
    fi
    kill -0 "$server" 2>/dev/null || die 'the registry server exited before answering /health'
    sleep 1
  done
  die 'the registry server did not answer /health within 30 seconds'
}

test_docker_image() {
  image=$1
  source_revision=$2

  [[ $source_revision =~ ^[0-9a-f]{40}$ ]]
  [[ $(docker image inspect --format '{{.Config.User}}' "$image") == 65532:65532 ]]
  [[ $(docker image inspect --format '{{index .Config.Labels "org.opencontainers.image.revision"}}' "$image") == "$source_revision" ]]

  # Moderation downloads use Tilo's fixed native-root path. Verify the runtime
  # image actually carries a non-empty bundle there; distroless images use a
  # different distro path by default.
  trust_probe=$(docker create "$image")
  trust_file=$(mktemp)
  if ! docker cp "$trust_probe:/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem" "$trust_file" >/dev/null \
    || [[ ! -s $trust_file ]]
  then
    docker rm "$trust_probe" >/dev/null 2>&1 || true
    rm -f "$trust_file"
    printf 'runtime image is missing the Tilo CA trust bundle\n' >&2
    exit 1
  fi
  docker rm "$trust_probe" >/dev/null
  rm -f "$trust_file"

  # The deployed binary must expose the reviewed offline-root protocol. Exercise
  # Clap from the image itself so a stale or partial server build cannot publish.
  for command in \
    authorize-channel \
    collect-first-party \
    publish-staged-first-party \
    sign-delegated-snapshot
  do
    help=$(docker run --rm --read-only --cap-drop ALL \
      --security-opt no-new-privileges "$image" "$command" --help)
    [[ $help == *"Usage:"* ]]
  done
  if docker run --rm --read-only --cap-drop ALL \
    --security-opt no-new-privileges "$image" promote-first-party --help >/dev/null 2>&1
  then
    printf 'removed unsafe promote-first-party command is still present\n' >&2
    exit 1
  fi

  container="tilo-registry-smoke-${source_revision:0:12}-$$"
  volume="${container}-state"
  helper_image=docker.io/library/busybox@sha256:9db7b59979c38555a39def84a31fb98b5296952f9e3afd4f6f11f05b07adfab0
  cleanup() {
    docker rm -f "$container" >/dev/null 2>&1 || true
    docker volume rm -f "$volume" >/dev/null 2>&1 || true
  }
  trap cleanup EXIT
  # Kubernetes makes the PVC writable through fsGroup=65532. Docker does not
  # implement fsGroup, so model that one kubelet operation with a tiny pinned
  # helper before any registry process sees the persistent named volume.
  docker volume create "$volume" >/dev/null
  docker run --rm \
    --user 0:0 \
    --read-only \
    --cap-drop ALL \
    --cap-add CHOWN \
    --security-opt no-new-privileges \
    --mount "type=volume,source=$volume,target=/state,volume-nocopy" \
    "$helper_image" \
    sh -ec 'chmod 0750 /state && chown 65532:65532 /state'

  # RFC 8032 test-vector public key. Only public identity material enters the
  # image; the owner signing seed remains outside the long-running service.
  owner_pubkey=d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a
  docker run --rm \
    --read-only \
    --cap-drop ALL \
    --security-opt no-new-privileges \
    --tmpfs /tmp:rw,noexec,nosuid,size=16m \
    --mount "type=volume,source=$volume,target=/var/lib/tilo-registry,volume-nocopy" \
    "$image" \
    --data-dir /var/lib/tilo-registry \
    init \
    --owner-pubkey "$owner_pubkey" \
    --name "Tilo registry image smoke test" \
    --description "ephemeral" \
    --base-url https://apps.tilo.so

  docker run --detach --name "$container" \
    --read-only \
    --cap-drop ALL \
    --security-opt no-new-privileges \
    --tmpfs /tmp:rw,noexec,nosuid,size=16m \
    --mount "type=volume,source=$volume,target=/var/lib/tilo-registry,volume-nocopy" \
    "$image" >/dev/null

  # The workflow shell runs inside the runner job container while Docker runs on
  # the host daemon. Join the registry's network namespace so this proves the
  # endpoint without assuming the daemon host is the shell's 127.0.0.1.
  if response=$(docker run --rm \
    --user 65532:65532 \
    --read-only \
    --cap-drop ALL \
    --security-opt no-new-privileges \
    --network "container:$container" \
    "$helper_image" \
    sh -ec 'for _ in $(seq 1 30); do response=$(wget -qO- http://127.0.0.1:8443/health 2>/dev/null) && { printf "%s" "$response"; exit 0; }; sleep 1; done; exit 1')
  then
    [[ $response == '{"ok":true}' ]]
    exit 0
  fi

  docker logs "$container" >&2
  exit 1
}

case $# in
  1) [[ -f $1 ]] || { printf '%s\nno such archive: %s\n' "$usage" "$1" >&2; exit 2; }
     test_archive "$1" ;;
  2) test_docker_image "$1" "$2" ;;
  *) printf '%s\n' "$usage" >&2; exit 2 ;;
esac
