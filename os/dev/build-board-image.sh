#!/usr/bin/env bash
# Build, seal and push one board dev image (os/Containerfile.board) with buildx.
#
#   build-board-image.sh BOARD BASE_IMAGE IMAGE
#
# BASE_IMAGE is the generic dev image of the same commit
# (registry.tilo.so/tilo/tilo-vm@sha256:...); IMAGE is the tag to push
# (registry.tilo.so/tilo/tilo-vm:<series>-<board>). The UKI seals the composefs
# digest `bootc install` computes from the pushed layers:
#
#   1. build with a placeholder digest and push by digest only;
#   2. compute the digest from those layers in the registry (cfs-digest-out);
#   3. build again with it: only the UKI changes, the rootfs layers are the
#      cached ones of step 1, and push IMAGE.
#
# Environment: BUILDER_NAME (buildx builder), RPMS_REF, BOOTC_HELPER_IMAGE (the
# resealing bootc, os/Containerfile.bootc-helper), and optionally
# REGISTRY_AUTH_FILE (default: the Docker client's config). Prints the pushed
# digest on stdout.
set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

[ "$#" -eq 3 ] || die "usage: $0 BOARD BASE_IMAGE IMAGE"
board="$1"; base="$2"; image="$3"
: "${BUILDER_NAME:?}" "${RPMS_REF:?}" "${BOOTC_HELPER_IMAGE:?}"
auth="${REGISTRY_AUTH_FILE:-${DOCKER_CONFIG:-$HOME/.docker}/config.json}"
[ -s "$auth" ] || die "no registry credentials at $auth"
repository="${image%:*}"

work="$(mktemp -d "${TMPDIR:-/tmp}/tilo-board-$board.XXXXXX")"
trap 'rm -rf -- "$work"' EXIT

common=(--builder "$BUILDER_NAME" --provenance=false -f "$TILO_OS_DIR/Containerfile.board"
  --build-arg "BASE_IMAGE=$base"
  --build-arg "RPMS_REF=$RPMS_REF"
  --build-arg "TILO_HARDWARE_PROFILE=$board"
  --build-arg "TILO_TARGET_IMGREF=code.tilo.so/tilo/tilo:$board-dev"
  --label "org.opencontainers.image.version=${image##*:}")
push="push=true,compression=zstd,compression-level=1"
placeholder="$(printf '0%.0s' $(seq 128))"

# The BuildKit steps of one build that took 2 s or more, for the lane log.
slow_steps() {
  awk -v board="$board" '
    $1 !~ /^#[0-9]+$/ { next }
    $2 == "DONE" && $3 ~ /^[0-9.]+s$/ { secs = $3; sub(/s$/, "", secs); if (secs + 0 >= 2) printf "%s:   %6.1fs %s\n", board, secs, name[$1]; next }
    $NF == "done" && $(NF - 1) ~ /^[0-9.]+s$/ {
      secs = $(NF - 1); sub(/s$/, "", secs); line = $0; sub(/^#[0-9]+ /, "", line); sub(/ [0-9.]+s done$/, "", line)
      if (secs + 0 >= 2) printf "%s:   %6.1fs   %s (%s)\n", board, secs, line, name[$1]; next }
    $2 !~ /^[0-9.]+$/ && !($1 in name) { line = $0; sub(/^#[0-9]+ /, "", line); name[$1] = substr(line, 1, 70) }
  ' "$1" >&2
}

started=$(date +%s)
docker buildx build --progress=plain "${common[@]}" --build-arg "TILO_CFS_DIGEST=$placeholder" \
  --metadata-file "$work/unsealed.json" \
  --output "type=image,name=$repository,push-by-digest=true,$push" "$TILO_REPO" 2> "$work/unsealed.log" \
  || { cat "$work/unsealed.log" >&2; die "$board: build failed"; }
slow_steps "$work/unsealed.log"
unsealed="$(jq -er '."containerimage.digest"' "$work/unsealed.json")"
msg "$board: layers pushed in $(( $(date +%s) - started ))s ($unsealed)"

started=$(date +%s)
docker buildx build --progress=plain "${common[@]}" --target cfs-digest-out \
  --build-arg "BOOTC_HELPER_IMAGE=$BOOTC_HELPER_IMAGE" \
  --build-arg "TILO_CFS_SOURCE=docker://$repository@$unsealed" \
  --secret "id=registry_auth,src=$auth" \
  --output "type=local,dest=$work/cfs" "$TILO_REPO" 2> "$work/cfs.log" \
  || { cat "$work/cfs.log" >&2; die "$board: composefs digest failed"; }
slow_steps "$work/cfs.log"
cfs="$(tr -d '[:space:]' < "$work/cfs/digest")"
[[ $cfs =~ ^[0-9a-f]{128}$ ]] || die "$board: no composefs digest computed"
msg "$board: composefs digest computed in $(( $(date +%s) - started ))s (${cfs:0:16}…)"

started=$(date +%s)
docker buildx build --progress=plain "${common[@]}" --build-arg "TILO_CFS_DIGEST=$cfs" \
  --metadata-file "$work/sealed.json" \
  --output "type=image,name=$image,$push" "$TILO_REPO" 2> "$work/sealed.log" \
  || { cat "$work/sealed.log" >&2; die "$board: sealed build failed"; }
slow_steps "$work/sealed.log"
sealed="$(jq -er '."containerimage.digest"' "$work/sealed.json")"
msg "$board: sealed image pushed in $(( $(date +%s) - started ))s ($image@$sealed)"
printf '%s\n' "$sealed"
