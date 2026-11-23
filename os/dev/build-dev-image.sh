#!/bin/bash
# Build the Tilo dev image (os/Containerfile.dev) FROM a production image in local storage.
#
#   TILO_DEV_BASE=localhost/tilo:ci TILO_COMPILED_DEV=DIR ./os/dev/build-dev-image.sh
#
# What t's os-dev-image-{arch} check runs (tools/ci/publish/os-image.sh dev), the one build of
# every dev image: main's published ones, and a worktree's for `tilo-vm vm upgrade --from local`
# and `tilo-vm image build`, which ask t for it. The disk is os-dev-disk-{arch}'s
# (tools/ci/publish/os-dev-disk.sh). `podman build -f os/Containerfile.dev` runs rootless; its
# dev components come compiled (os/image/compile.sh --dev, t's os-compile-dev-<arch>), so
# nothing compiles here.
#
# Environment:
#   TILO_DEV_BASE     production image to build FROM (required), in local storage
#   TILO_COMPILED_DEV the dev components compiled already (required)
#   TILO_DEV_BASE_CREATED  the base's time tilo-vm orders dev bases by (RFC 3339;
#                     default: the base's commit time, see below). A base whose
#                     clock is its Fedora base's and RPM snapshot's
#                     (tools/ci/publish/os-image.sh) is given its commit's time here
#   TILO_DEV_IMAGE    tag for the dev image (default localhost/tilo-vm:latest);
#                     must name tilo-vm, never the production image
#   TILO_DEV_ENV_RUNTIME  a sealed env-runtime directory (install-env-runtime.sh's
#                     layout; `env-runtime.sh dev` in os-image.sh): what the dev image ships when
#                     its base ships no published runtime. A dev image with neither
#                     fails to build (os/scripts/ensure-dev-env-runtime.sh)
#   TILO_DEV_MODEL_CATALOG  a directory with a signed local model catalog and its keys
#                     (catalog.json, keys.json; `local-model-catalog.sh dev`):
#                     what the dev image carries in place of the base's
#                     catalog. Without it the dev image keeps the base's
#   TILO_OUTPUT       scratch root (default os/output/dev)
#   TILO_BUILD_ARGS   extra `podman build` flags, e.g. --no-cache
set -euo pipefail

for a in "$@"; do
  case "$a" in
    -h|--help) sed -n '2,/^set -euo/{/^set -euo/!p}' "$0"; exit 0 ;;
    *) printf 'build-dev-image: unknown arg: %s\n' "$a" >&2; exit 2 ;;
  esac
done

_dev_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export TILO_OUTPUT="${TILO_OUTPUT:-$(cd "$_dev_dir/.." && pwd)/output/dev}"
source "$_dev_dir/_common.sh"

base="${TILO_DEV_BASE:?TILO_DEV_BASE names the production image to build FROM}"
[ -n "${TILO_COMPILED_DEV:-}" ] || die "TILO_COMPILED_DEV names no compiled dev components (os/image/compile.sh --dev)"
dev_image="${TILO_DEV_IMAGE:-localhost/tilo-vm:$TILO_LOCAL_TAG}"
case "$dev_image" in
  localhost/tilo-vm:*|registry.tilo.so/tilo/tilo-vm:*) ;;
  *) die "TILO_DEV_IMAGE=$dev_image: the dev image may only be tagged localhost/tilo-vm:<tag> or
  registry.tilo.so/tilo/tilo-vm:<tag>. It runs sshd and dev-overrides binaries, so it must never
  carry a production image name." ;;
esac

python3 "$TILO_REPO/tools/policy/check_dev_image_contract.py" --build >/dev/null \
  || { python3 "$TILO_REPO/tools/policy/check_dev_image_contract.py" --build || true; die "fix the dev image contract first (see above)"; }

podman image exists "$base" || die "the production base $base is not in local storage"
base_digest="$(podman image inspect --format '{{.Digest}}' "$base" 2>/dev/null || true)"
[[ "$base_digest" =~ ^sha256:[0-9a-f]{64}$ ]] || base_digest=""
# The base's commit time (its org.tilo.source-date-epoch label, else .Created,
# which production builds clamp to it); tilo-vm compares it
# with the base of the image a VM runs, so a VM is not moved onto an older base.
base_created="${TILO_DEV_BASE_CREATED:-$(tilo_image_source_time "$base")}"

read -r -a _extra_build_args <<< "${TILO_BUILD_ARGS:-}"
if [ -z "${SOURCE_DATE_EPOCH:-}" ]; then
  SOURCE_DATE_EPOCH="$(git -C "$TILO_REPO" log -1 --format=%ct 2>/dev/null || true)"
fi
case "$SOURCE_DATE_EPOCH" in
  ''|0|*[!0-9]*)
    die "SOURCE_DATE_EPOCH must be a positive commit time. A zero clock makes DNF reject RPM headers. Set SOURCE_DATE_EPOCH=\$(git log -1 --format=%ct) from the real checkout."
    ;;
esac
export SOURCE_DATE_EPOCH
: "${TMPDIR:=$TILO_OUTPUT/.tmp}"
mkdir -p "$TMPDIR"
export TMPDIR

# The dev packages come from the snapshot the base was built from: the one its
# org.tilo.rpms.ref label records, else the published one for this commit.
rpms_ref="${RPMS_REF:-}"
if [ -z "$rpms_ref" ]; then
  rpms_ref="$(podman image inspect --format '{{index .Labels "org.tilo.rpms.ref"}}' "$base" 2>/dev/null || true)"
  case "$rpms_ref" in "<no value>") rpms_ref="" ;; esac
fi
if [ -z "$rpms_ref" ]; then
  rpms_ref="$("$TILO_OS_DIR/dev/resolve-rpm-snapshot.sh")"     || die "no fingerprint-resolved RPMS_REF; run snapshot-rpms or set RPMS_REF"
fi

# The Fedora bootc base the dev binaries compile on, as the production builder's.
dev_compile_base="$(. "$TILO_OS_DIR/base-snapshot.env" && printf '%s' "${BASE_REF:-}")"
[ -n "$dev_compile_base" ] || die "os/base-snapshot.env pins no BASE_REF"

# Each build's context holds the files its Containerfile copies and nothing else
# (os/dev/build-context.py), dated by the build's clock: this checkout's other files — the VM
# acceptance harness, CI scripts, the cards and docs — cannot change what the image holds, so a
# change to one builds neither the builder nor the image again.
stage_context() {
  local context
  context="$(python3 -I "$TILO_OS_DIR/dev/build-context.py" "$1" --repo "$TILO_REPO" --epoch "$SOURCE_DATE_EPOCH")" \
    || die "could not stage the build context of $1"
  printf '%s\n' "$context"
}

# The dev components, compiled outside the image build (t's os-compile-dev-<arch>).

# The env-runtime the dev image ships when its base ships none; an empty directory gives none.
if [ -n "${TILO_DEV_ENV_RUNTIME:-}" ]; then
  dev_env_runtime="$(realpath "$TILO_DEV_ENV_RUNTIME")"
else
  dev_env_runtime="$TILO_OUTPUT/no-env-runtime"
  rm -rf "$dev_env_runtime"
  mkdir -p "$dev_env_runtime"
fi

# The local model catalog the dev image carries in place of its base's; an empty directory keeps
# the base's.
if [ -n "${TILO_DEV_MODEL_CATALOG:-}" ]; then
  dev_model_catalog="$(realpath "$TILO_DEV_MODEL_CATALOG")"
else
  dev_model_catalog="$TILO_OUTPUT/no-model-catalog"
  rm -rf "$dev_model_catalog"
  mkdir -p "$dev_model_catalog"
fi

msg "podman build $dev_image FROM $base${base_digest:+ ($base_digest)}"
dev_started=$SECONDS
dev_context=$(stage_context "$TILO_OS_DIR/Containerfile.dev")
podman build "${TILO_PODMAN_BUILD_FLAGS[@]}" -f "$TILO_OS_DIR/Containerfile.dev" \
  --build-arg "BASE_IMAGE=$base" \
  --build-arg "DEV_PACKAGES_BASE=$base" \
  --build-arg "RPMS_REF=$rpms_ref" \
  --build-arg "BASE_REF=$dev_compile_base" \
  --build-arg "TILO_BASE_DIGEST=$base_digest" \
  --build-arg "TILO_BASE_CREATED=$base_created" \
  --build-context "compiled-dev=$(realpath "$TILO_COMPILED_DEV")" \
  --build-context "dev-env-runtime=$dev_env_runtime" \
  --build-context "dev-model-catalog=$dev_model_catalog" \
  --build-arg "SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH" \
  --source-date-epoch "$SOURCE_DATE_EPOCH" --rewrite-timestamp \
  "${_extra_build_args[@]}" \
  --tag "$dev_image" \
  "$dev_context"
msg "built $dev_image in $(( SECONDS - dev_started ))s"
