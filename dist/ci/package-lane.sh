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

# What would fail the Fedora build after hours of compiling fails here first: the app store's
# trust root (dist/rpm/stage-app-store.sh refuses the same keys), and an RPM lint exception
# path that no longer matches (build-rpm.sh repeats this inside each container).
require_registry_owner() {
  local owner=${TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY:-}
  [[ $owner =~ ^[0-9a-fA-F]{64}$ && $owner != $(printf '0%.0s' {1..64}) ]] || {
    printf 'TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY must be a non-placeholder 32-byte hex key\n' >&2
    return 1
  }
}
require_registry_owner
python3 dist/rpm/test-rpmlint-config.py

# t runs this as a micro-VM step in the builder image, as root in a guest of its own; the
# builder carries no container engine, so the guest installs Podman from Fedora first.
if ! command -v docker >/dev/null && ! command -v podman >/dev/null \
    && [[ $(id -u) == 0 ]] && command -v dnf >/dev/null; then
  dnf -y -q install --setopt=install_weak_deps=False podman >/dev/null
fi
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
  registry.tilo.so/tilo/skopeo@sha256:7e735527d9d64c7456fc82d7f954c380b4a5dd1a8173115ce2ed7b3442ef36e0 "$@"
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
# The target list arrives on its own descriptor and each job reads no stdin: a container
# engine attached to the job reads whatever stdin holds, so the first target used to swallow
# the rest of the list and the lane ended green after one userspace.
# A broken userspace does not hide the others' results: every target builds, and the lane
# fails at the end naming each one that failed.
failed=()
while IFS='|' read -r -u 3 distro image revision; do
  [[ -n $distro ]] || continue
  printf '==> %s / %s\n' "$distro" "$arch"
  dist/ci/run-package-job.sh "$distro" "$image" "$arch" "$revision" </dev/null || failed+=("$distro")
done 3< <(dist/ci/package-targets.sh "$arch")
if ((${#failed[@]})); then
  printf 'package lane %s: failed userspaces: %s\n' "$arch" "${failed[*]}" >&2
  exit 1
fi
