#!/usr/bin/env bash
# Run the login-wrapper contract with a real non-root passwd entry. Use docker
# cp rather than a bind mount so this works with containerized Forgejo runners.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
container=$(docker create --platform linux/amd64 \
    --entrypoint /bin/bash \
    docker.io/library/ubuntu@sha256:1e0a86e57d247923571b75e0aaf48a1449cf8c543d51fb3e07a4a7d7bfa79316 \
    -lc "useradd --create-home tester && \
         su tester -s /bin/bash -c '/session/tests/dev-session-refusals.sh'")
cleanup_container() {
    docker rm --force "$container" >/dev/null 2>&1 || true
}
trap cleanup_container EXIT
docker cp "$repo_root/dist/session/." "$container:/session"
docker start --attach "$container"
