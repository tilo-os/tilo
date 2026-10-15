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

# fetch-package-env-runtime.sh copies the env-runtime image with skopeo, which a node with only
# a container engine runs from its pinned image.
if ! command -v skopeo >/dev/null; then
  shim=${shim:-$(mktemp -d "${TMPDIR:-/tmp}/docker-shim.XXXXXX")}
  cat > "$shim/skopeo" <<'SH'
#!/bin/sh
work=${TMPDIR:-/tmp}
exec docker run --rm --network host -v "$PWD:$PWD" -v "$work:$work" -w "$PWD" \
  quay.io/skopeo/stable@sha256:edad588fe8391ce37d5ec5e06d280dd1a666a77da302ceaf3cafa019676c8a3f "$@"
SH
  chmod +x "$shim/skopeo"
  PATH=$shim:$PATH
fi

rm -rf dist/ci-output
# The cached Cosign installer reports its directory the way a workflow step would.
export RUNNER_TEMP=${RUNNER_TEMP:-$(mktemp -d "${TMPDIR:-/tmp}/package-lane.XXXXXX")}
export GITHUB_PATH=${GITHUB_PATH:-$RUNNER_TEMP/path}
bash dist/ci/fetch-package-env-runtime.sh
export TILO_ENABLE_REMOTE_CACHE=0 TILO_CARGO_JOBS=${TILO_CARGO_JOBS:-6}
while IFS='|' read -r distro image revision; do
  [[ -n $distro ]] || continue
  printf '==> %s / %s\n' "$distro" "$arch"
  dist/ci/run-package-job.sh "$distro" "$image" "$arch" "$revision"
done < <(dist/ci/package-targets.sh "$arch")
