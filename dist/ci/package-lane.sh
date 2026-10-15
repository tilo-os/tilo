#!/usr/bin/env bash
# Build and install-test the desktop session for every distribution userspace of one
# architecture (dist/ci/package-targets.txt), one after another, into dist/ci-output/<distro>-<arch>:
# t's desktop-package-<arch> checks, on privileged nodes with a container engine.
#
#   dist/ci/package-lane.sh amd64|arm64
#
# The userspaces build serially: each release link uses fat LTO and peaks at several GiB per
# compiler process. A payload that can be published never compiles through a shared compiler
# cache.
set -euo pipefail
arch=${1:?usage: package-lane.sh amd64|arm64}
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

engine=docker
if ! command -v docker >/dev/null && command -v podman >/dev/null; then
  # run-package-job.sh speaks Docker's CLI, which podman takes as it is.
  shim=$(mktemp -d "${TMPDIR:-/tmp}/docker-shim.XXXXXX")
  printf '#!/bin/sh\nexec podman "$@"\n' > "$shim/docker"
  chmod +x "$shim/docker"
  PATH=$shim:$PATH
  engine=podman
fi
command -v docker >/dev/null || { printf 'package-lane.sh needs docker or podman on this node\n' >&2; exit 1; }
printf 'container engine: %s\n' "$engine"

rm -rf dist/ci-output
bash dist/ci/fetch-package-env-runtime.sh
export TILO_ENABLE_REMOTE_CACHE=0 TILO_CARGO_JOBS=${TILO_CARGO_JOBS:-6}
while IFS='|' read -r distro image revision; do
  [[ -n $distro ]] || continue
  printf '==> %s / %s\n' "$distro" "$arch"
  dist/ci/run-package-job.sh "$distro" "$image" "$arch" "$revision"
done < <(dist/ci/package-targets.sh "$arch")
