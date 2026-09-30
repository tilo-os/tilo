#!/bin/bash
# Build the Tilo dev image (os/Containerfile.dev) and a bootable dev VM disk.
#
#   ./os/dev/build-dev-image.sh          (or ./os/build.sh --dev)
#   TILO_DEV_DISK=0 TILO_DEV_BASE_BUILD=auto ./os/dev/build-dev-image.sh
#                                        (what tilo-vm vm upgrade --from local runs)
#
# Pipeline, which CI runs too (.forgejo/workflows/build-image.yml, dev-disk.yml):
#   0. with TILO_DEV_BASE_BUILD=auto or 1 only: the production base
#      (os/dev/build-image.sh, TILO_IMAGE_ONLY=1). auto rebuilds it only when its
#      inputs changed since it was built (see below).
#   1. rootless `podman build -f os/Containerfile.dev` FROM a production image
#      that is already in local storage. The dev binaries compile inside the
#      build, with the production builder's toolchain.
#   2. os/dev/build-image.sh with TILO_SKIP_BUILD=1 installs that image to
#      $TILO_OUTPUT/qcow2/disk.qcow2. The dev image is unsealed, so this is a
#      single `bootc install` pass with ordinary boot entries. This step needs
#      passwordless sudo (`sudo -n`) for the loop device and mounts; without it
#      the script stops after step 1 and prints the command to run later.
#
# Environment:
#   TILO_LOCAL_TAG    tag of the local images below instead of latest, to keep
#                     builds from different checkouts apart
#   TILO_DEV_BASE     production image to build FROM (default localhost/tilo:latest,
#                     what ./os/build.sh produces). A CI image works too:
#                       podman pull registry.tilo.so/tilo/tilo:<tag>
#                       TILO_DEV_BASE=registry.tilo.so/tilo/tilo:<tag> ./os/dev/build-dev-image.sh
#   TILO_DEV_BASE_BUILD  0 (default): the base must exist. 1: rebuild it first.
#                     auto (./os/build.sh --dev --local): rebuild it when it is
#                     missing or its inputs changed. The inputs are every file of
#                     the production build context except the dev-image-only
#                     sources, CI workflows and crates/dev/tilo-vm, hashed from git's
#                     index and working tree (outside a git checkout, from the
#                     files themselves), plus the build's input variables; the
#                     hash is the base's sh.tilo.base-inputs label. A base without
#                     that label (built elsewhere) is used as it is. The base is
#                     built with os/dev/local-inputs.sh, which resolves the RPM
#                     snapshot, kernel and owner key for this machine.
#   TILO_DEV_IMAGE    tag for the dev image (default localhost/tilo-vm:latest);
#                     must name tilo-vm, never the production image
#   TILO_DEV_DISK     0 builds the container only (default 1)
#   TILO_OUTPUT       disk output root (default os/output/dev)
#   TILO_BUILD_ARGS   extra `podman build` flags, e.g. --no-cache
#   TILO_CARGO_FRESHNESS  content (default here): the dev binaries' compile keeps
#                     its target cache and rebuilds only changed crates
#                     (os/scripts/cargo-freshness.sh); clean: CI's clean compile
#   TILO_DEV_CARGO_BUILD_JOBS  parallel jobs of that compile (default: a CPU per
#                     job, as far as memory holds out at 4 GiB a thin-LTO job)
#   TILO_DISK_SIZE    the dev disk's size (default 64G, as the published dev
#                     disks: the dev image alone fills most of 20G; qcow2 stays
#                     sparse)
#   TILO_TARGET_IMGREF, TILO_ACCOUNT  passed to the disk step
set -euo pipefail

for a in "$@"; do
  case "$a" in
    -h|--help) sed -n '2,52p' "$0"; exit 0 ;;
    *) printf 'build-dev-image: unknown arg: %s\n' "$a" >&2; exit 2 ;;
  esac
done

_dev_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
_production_output="${TILO_OUTPUT:-$(cd "$_dev_dir/.." && pwd)/output}"
export TILO_OUTPUT="${TILO_OUTPUT:-$(cd "$_dev_dir/.." && pwd)/output/dev}"
export TILO_DISK="${TILO_DISK:-$TILO_OUTPUT/qcow2/disk.qcow2}"
export TILO_DISK_SIZE="${TILO_DISK_SIZE:-64G}"
source "$_dev_dir/_common.sh"

base="${TILO_DEV_BASE:-localhost/tilo:$TILO_LOCAL_TAG}"
dev_image="${TILO_DEV_IMAGE:-localhost/tilo-vm:$TILO_LOCAL_TAG}"
case "$dev_image" in
  localhost/tilo-vm:*|registry.tilo.so/tilo/tilo-vm:*) ;;
  *) die "TILO_DEV_IMAGE=$dev_image: the dev image may only be tagged localhost/tilo-vm:<tag> or
  registry.tilo.so/tilo/tilo-vm:<tag>. It runs sshd and dev-overrides binaries, so it must never
  carry a production image name." ;;
esac
base_build="${TILO_DEV_BASE_BUILD:-0}"
case "$base_build" in 0|1|auto) ;; *) die "TILO_DEV_BASE_BUILD must be 0, 1 or auto" ;; esac

python3 "$TILO_REPO/tools/policy/check_dev_image_contract.py" >/dev/null \
  || { python3 "$TILO_REPO/tools/policy/check_dev_image_contract.py" || true; die "fix the dev image contract first (see above)"; }

# The production build context, minus what cannot change the base image: the
# sources only os/Containerfile.dev reads, CI workflows, and the host-side tilo-vm.
_base_spec=(. ':(exclude)os/Containerfile.dev' ':(exclude)os/dev-image'
  ':(exclude)os/dev/build-dev-image.sh' ':(exclude)crates/dev/tilo-vmd/data'
  ':(exclude)os/scripts/unseal-rootfs.py' ':(exclude)os/scripts/check-dev-kernel.sh'
  ':(exclude)os/scripts/finalize-dev-image.py' ':(exclude)crates/dev/tilo-vm'
  ':(exclude).forgejo' ':(exclude)os/external-assets/cache')
# Outside a git checkout (a copy of the tree synced to a build host) the same
# files are hashed from the tree itself: everything but what the build context
# leaves out (.containerignore's big entries) and the excludes above.
_base_prune=(./.git ./target ./os/output ./os/output-host ./os/external-assets/cache
  ./os/Containerfile.dev ./os/dev-image ./os/dev/build-dev-image.sh ./crates/dev/tilo-vmd/data
  ./os/scripts/unseal-rootfs.py ./os/scripts/check-dev-kernel.sh
  ./os/scripts/finalize-dev-image.py ./crates/dev/tilo-vm ./.forgejo)
tree_inputs() {
  local prune=() p
  for p in "${_base_prune[@]}"; do prune+=(-path "$p" -o); done
  ( cd "$TILO_REPO" && find . \( "${prune[@]}" -false \) -prune -o -type f -print0 |
      LC_ALL=C sort -z | xargs -0 -r sha256sum )
}
base_inputs() {
  {
    # What os/dev/local-inputs.sh does not resolve by itself.
    printf '%s=%s\n' browser "${TILO_WITH_BROWSER:-0}" rpms "${RPMS_REF:-}" \
      rpms-trust "${TILO_RPMS_TRUST:-}" kernel "${KERNEL_RPMS_REF:-}" \
      kernel-trust "${TILO_KERNEL_TRUST:-}"
    if [ "$_base_from_git" = 0 ]; then
      tree_inputs
    else
      git -C "$TILO_REPO" ls-files -s -- "${_base_spec[@]}"
      # Edited and untracked files, which the index above does not describe.
      git -C "$TILO_REPO" ls-files -z -m -o --exclude-standard -- "${_base_spec[@]}" |
        while IFS= read -r -d '' f; do
          if [ -f "$TILO_REPO/$f" ]; then
            printf '%s %s\n' "$(git -C "$TILO_REPO" hash-object -- "$f")" "$f"
          else
            printf 'deleted %s\n' "$f"
          fi
        done
    fi
  } | sha256sum | cut -c1-64
}
_base_from_git=1
[ "$(git -C "$TILO_REPO" rev-parse --show-toplevel 2>/dev/null)" = "$TILO_REPO" ] || _base_from_git=0

base_seconds=""
if [ "$base_build" != 0 ]; then
  case "$base" in
    localhost/tilo:*) ;;
    *) die "TILO_DEV_BASE_BUILD=$base_build rebuilds the base, but TILO_DEV_BASE=$base is not a local
  production image. Use localhost/tilo:<tag>, or TILO_DEV_BASE_BUILD=0 for a CI base." ;;
  esac
  [ "$_base_from_git" = 1 ] \
    || msg "0/2 base: $TILO_REPO is not a git checkout; fingerprinting its files instead"
  want="$(base_inputs)"
  reason=""
  if [ "$base_build" = 1 ]; then
    reason="TILO_DEV_BASE_BUILD=1"
  elif ! podman image exists "$base"; then
    reason="$base is not in local storage"
  else
    have="$(podman image inspect --format '{{index .Labels "sh.tilo.base-inputs"}}' "$base" 2>/dev/null || true)"
    case "$have" in
      "$want") msg "0/2 base: $base is current (inputs ${want:0:12}); not rebuilding it" ;;
      ""|"<no value>") msg "0/2 base: $base carries no sh.tilo.base-inputs label (built outside this script); using it as it is. TILO_DEV_BASE_BUILD=1 rebuilds it" ;;
      *) reason="its inputs changed (${have:0:12} -> ${want:0:12})" ;;
    esac
  fi
  if [ -n "$reason" ]; then
    msg "0/2 base: building $base, because $reason"
    base_started=$SECONDS
    (
      export TILO_OUTPUT="$_production_output"
      # shellcheck disable=SC1091
      source "$TILO_OS_DIR/dev/local-inputs.sh"
      TILO_IMAGE="$base" TILO_IMAGE_ONLY=1 \
        TILO_BUILD_ARGS="${TILO_BUILD_ARGS:-} --label sh.tilo.base-inputs=$want" \
        "$TILO_OS_DIR/dev/build-image.sh"
    )
    base_seconds=$(( SECONDS - base_started ))
  fi
fi

podman image exists "$base" || die "the production base $base is not in local storage.
  Build it with ./os/build.sh (or TILO_DEV_BASE_BUILD=auto $0), or use a CI image:
    podman pull registry.tilo.so/tilo/tilo:<tag>
    TILO_DEV_BASE=registry.tilo.so/tilo/tilo:<tag> $0"
base_digest="$(podman image inspect --format '{{.Digest}}' "$base" 2>/dev/null || true)"
[[ "$base_digest" =~ ^sha256:[0-9a-f]{64}$ ]] || base_digest=""
# The base's commit time (its org.tilo.source-date-epoch label, else .Created,
# which production builds clamp to it); tilo-vm compares it
# with the base of the image a VM runs, so a VM is not moved onto an older base.
base_created="$(tilo_image_source_time "$base")"

read -r -a _extra_build_args <<< "${TILO_BUILD_ARGS:-}"
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "$TILO_REPO" log -1 --format=%ct 2>/dev/null || echo 0)}"
export SOURCE_DATE_EPOCH
: "${TMPDIR:=$TILO_OUTPUT/.tmp}"
mkdir -p "$TMPDIR"
export TMPDIR

# The dev packages come from the snapshot the base was built from: the one its
# org.tilo.rpms.ref label records (a base built here may use a local snapshot,
# which nothing else resolves), else the published one for this commit.
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

msg "1/2 podman build $dev_image FROM $base${base_digest:+ ($base_digest)}"
dev_started=$SECONDS
podman build "${TILO_PODMAN_BUILD_FLAGS[@]}" -f "$TILO_OS_DIR/Containerfile.dev" \
  --build-arg "BASE_IMAGE=$base" \
  --build-arg "RPMS_REF=$rpms_ref" \
  --build-arg "BASE_REF=$dev_compile_base" \
  --build-arg "TILO_BASE_DIGEST=$base_digest" \
  --build-arg "TILO_BASE_CREATED=$base_created" \
  --build-arg "TILO_CARGO_FRESHNESS=${TILO_CARGO_FRESHNESS:-content}" \
  --build-arg "TILO_CARGO_BUILD_JOBS=${TILO_DEV_CARGO_BUILD_JOBS:-$(cargo_build_jobs 4)}" \
  --build-arg "SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH" \
  "${_extra_build_args[@]}" \
  --tag "$dev_image" \
  "$TILO_REPO"
msg "built $dev_image in $(( SECONDS - dev_started ))s${base_seconds:+ (base: ${base_seconds}s)}"

disk_command="TILO_IMAGE=$dev_image TILO_SKIP_BUILD=1 TILO_OUTPUT=$TILO_OUTPUT $TILO_OS_DIR/dev/build-image.sh"
if [ "${TILO_DEV_DISK:-1}" = 0 ]; then
  msg "DONE: $dev_image (TILO_DEV_DISK=0: no disk). Write one later with:
  $disk_command
  or move a running dev VM onto it: tilo-vm vm upgrade --from local:$dev_image"
  exit 0
fi
sudo -n true 2>/dev/null || die "the disk step needs passwordless sudo (sudo -n) for its loop device and
  mounts. $dev_image is built; write the disk from a shell that has it:
  $disk_command"

msg "2/2 disk image of $dev_image -> $TILO_DISK"
TILO_IMAGE="$dev_image" TILO_SKIP_BUILD=1 exec "$TILO_OS_DIR/dev/build-image.sh"
