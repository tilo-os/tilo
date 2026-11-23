#!/usr/bin/env bash
# Build and install-test the desktop session for every distribution userspace of one
# architecture (dist/ci/package-targets.txt), or for the distributions named after it, one after
# another, into dist/ci-output/<distro>-<arch>: t's desktop-package-<arch> checks (every
# userspace) and package-arch / package-nix (one each), on privileged nodes with a container
# engine.
#
#   dist/ci/package-job.sh amd64|arm64 [DISTRO...]
#
# The userspaces build serially: each release link uses fat LTO and peaks at several GiB per
# compiler process. A payload that can be published never compiles through a shared compiler
# cache.
set -euo pipefail
arch=${1:?usage: package-job.sh amd64|arm64 [DISTRO...]}
shift
selected=("$@")
cd "$(dirname "${BASH_SOURCE[0]}")/../.."

# The targets this run builds, read once before any container starts: a container engine
# attached to the job drains stdin, which used to swallow every target after the first.
targets=()
while IFS='|' read -r -u 3 distro image revision; do
  [[ -n $distro ]] || continue
  if ((${#selected[@]})); then
    keep=0
    for want in "${selected[@]}"; do [[ $distro == "$want" ]] && keep=1; done
    ((keep)) || continue
  fi
  targets+=("$distro|$image|$revision")
done 3< <(dist/ci/package-targets.sh "$arch")
if ((${#targets[@]} == 0)); then
  printf 'package-job.sh: no %s userspace matches %s\n' "$arch" "${selected[*]}" >&2
  exit 2
fi

# Only the Fedora userspace stages the app store, and what would fail its build after hours of
# compiling fails here first: the store's trust root (dist/rpm/stage-app-store.sh refuses the same
# keys), and an RPM lint exception path that no longer matches (build-rpm.sh repeats this inside
# each container).
needs_app_store=0
for target in "${targets[@]}"; do
  [[ $target == fedora-44\|* ]] && needs_app_store=1
done
require_registry_owner() {
  local owner=${TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY:-}
  [[ $owner =~ ^[0-9a-fA-F]{64}$ && $owner != $(printf '0%.0s' {1..64}) ]] || {
    printf 'TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY must be a non-placeholder 32-byte hex key\n' >&2
    return 1
  }
}
if ((needs_app_store)); then
  require_registry_owner
  python3 dist/rpm/test-rpmlint-config.py
fi

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
command -v docker >/dev/null || { printf 'package-job.sh needs docker or podman on this node\n' >&2; exit 1; }
printf 'container engine: %s\n' "$engine"

rm -rf dist/ci-output
export TILO_ENABLE_REMOTE_CACHE=0 TILO_CARGO_JOBS=${TILO_CARGO_JOBS:-6}
if ((needs_app_store)); then
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
  # The cached Cosign installer reports its directory the way a workflow step would.
  export RUNNER_TEMP=${RUNNER_TEMP:-$(mktemp -d "${TMPDIR:-/tmp}/package-job.XXXXXX")}
  export GITHUB_PATH=${GITHUB_PATH:-$RUNNER_TEMP/path}
  bash dist/ci/fetch-package-env-runtime.sh
fi
# A broken userspace does not hide the others' results: every target builds, and the job fails at
# the end naming each one that failed.
failed=()
for target in "${targets[@]}"; do
  IFS='|' read -r distro image revision <<<"$target"
  printf '==> %s / %s\n' "$distro" "$arch"
  dist/ci/run-package-job.sh "$distro" "$image" "$arch" "$revision" </dev/null || failed+=("$distro")
done
if ((${#failed[@]})); then
  printf 'package job %s: failed userspaces: %s\n' "$arch" "${failed[*]}" >&2
  exit 1
fi
