#!/usr/bin/env bash
# The registry server image before it is published: non-root, Tilo's trust bundle at the fixed
# path, the reviewed offline-root commands and not the removed one, and a server that
# initializes a fresh state directory and answers /health.
#
# usage: test-image.sh ARCHIVE
# ARCHIVE is an OCI archive as tools/ci/publish/oci-build.sh writes it (the t check
# image-registry-server). It runs in the buildah image, like oci-build.sh, without a daemon,
# and checks no revision label: one build serves every commit with the same inputs, so the
# digest receipt names the commit.
set -euo pipefail
usage='usage: test-image.sh ARCHIVE'

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

case $# in
  1) [[ -f $1 ]] || { printf '%s\nno such archive: %s\n' "$usage" "$1" >&2; exit 2; }
     test_archive "$1" ;;
  *) printf '%s\n' "$usage" >&2; exit 2 ;;
esac
