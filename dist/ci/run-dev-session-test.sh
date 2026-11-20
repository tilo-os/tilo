#!/usr/bin/env bash
# Run the login-wrapper contract with a non-root passwd entry. Use the engine's
# cp rather than a bind mount so this works with containerized Forgejo runners
# and with the podman a t VM installs (DOCKER=podman).
set -euo pipefail

engine=${DOCKER:-docker}
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
container=$($engine create --platform linux/amd64 \
    --entrypoint /bin/bash \
    docker.io/library/ubuntu@sha256:1e0a86e57d247923571b75e0aaf48a1449cf8c543d51fb3e07a4a7d7bfa79316 \
    -lc "useradd --create-home tester && \
         su tester -s /bin/bash -c '/session/tests/dev-session-refusals.sh'")
cleanup_container() {
    $engine rm --force "$container" >/dev/null 2>&1 || true
}
trap cleanup_container EXIT
$engine cp "$repo_root/dist/session/." "$container:/session"
$engine start --attach "$container"
