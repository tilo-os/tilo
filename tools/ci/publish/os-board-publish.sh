#!/usr/bin/env bash
# Publish main's board dev image and its flashable disk (publish-os-board-<board> in t.toml),
# from what `os-image.sh board` left in OUT: unsigned, under the development-only name tilo-vm,
# which production policy rejects (os/etc/containers/policy.json).
#
#   os-board-publish.sh BOARD OUT
#
# Pushes registry.tilo.so/tilo/tilo-vm:main-<commit7>-BOARD, which `tilo-vm vm upgrade --from
# sha:<commit>` takes for that board, and moves tilo-vm:BOARD-ci, which installed boards follow
# with `bootc upgrade`, to it. Then replaces the disk in the generic package
# tilo-vm-disk-latest/aarch64-BOARD (tilo-vm-BOARD.qcow2, its .sha256 and the commit in
# `revision`), which DEVELOPMENT.md points at. Both move only ever forward along main: when the
# commit they name now is this one's ancestor. Logs in with an identity token for
# registry.tilo.so (oci-publish.sh); the disk goes up with TILO_PACKAGES_TOKEN.
set -euo pipefail
here=$(dirname -- "${BASH_SOURCE[0]}")
. "$here/lib.sh"
board=${1:?usage: os-board-publish.sh BOARD OUT}
out=${2:?usage: os-board-publish.sh BOARD OUT}
[[ $(channel) == dev ]] || die "board dev images publish from main only; this run is for $T_REF"
require_commit
[[ $(cat "$out/board") == "$board" ]] || die "$out holds the image of $(cat "$out/board"), not $board"
[[ $(cat "$out/revision") == "$T_COMMIT" ]] || die "$out holds the image of $(cat "$out/revision"), the run is for $T_COMMIT"
[[ -s $out/disk/disk.qcow2 ]] || die "no flashable disk in $out/disk"
repository=registry.tilo.so/tilo/tilo-vm
moving=$board-ci
tags=("main-${T_COMMIT:0:7}-$board")

# Whether something naming commit $1 (nothing, this commit, or an ancestor of it) may move
# here. Main's builds may finish out of order; an older one never takes it back.
forward() {
  local current=$1 ahead
  [[ -z $current || $current == "<no value>" || $current == "$T_COMMIT" ]] && return 0
  # The commits the named one has that this one lacks: none, when it is an ancestor.
  ahead=$(curl -fsS "https://code.tilo.so/api/v1/repos/tilo/tilo/compare/$T_COMMIT...$current" \
    | sed -n 's/.*"total_commits":\([0-9]*\).*/\1/p') || ahead=
  [[ $ahead == 0 ]] && return 0
  printf 'it names %s, %s; leaving it\n' "$current" \
    "${ahead:+which is not an ancestor of $T_COMMIT}${ahead:-whose history could not be compared}" >&2
  return 1
}

# With TILO_PUBLISH_DRY_RUN the tag is looked up in the layout oci-publish.sh writes instead.
ref=docker://$repository:$moving
[[ -n ${TILO_PUBLISH_DRY_RUN:-} ]] && ref=oci:$TILO_PUBLISH_DRY_RUN/$repository:$moving
current=$(skopeo inspect --format '{{index .Labels "org.opencontainers.image.revision"}}' "$ref" 2>/dev/null) || current=
forward "$current" && tags+=("$moving")
bash "$here/oci-publish.sh" dev "$repository" "$out" "${tags[@]}"

latest=https://code.tilo.so/api/packages/tilo/generic/tilo-vm-disk-latest/aarch64-$board
disk=tilo-vm-$board.qcow2
staged=$scratch/board-disk
mkdir -p "$staged"
(cd "$out/disk" && sha256sum disk.qcow2) | sed "s/disk.qcow2\$/$disk/" > "$staged/$disk.sha256"
printf '%s\n' "$T_COMMIT" > "$staged/revision"
if [[ -n ${TILO_PUBLISH_DRY_RUN:-} ]]; then
  mkdir -p "$TILO_PUBLISH_DRY_RUN/packages/aarch64-$board"
  cp "$out/disk/disk.qcow2" "$TILO_PUBLISH_DRY_RUN/packages/aarch64-$board/$disk"
  cp "$staged/$disk.sha256" "$staged/revision" "$TILO_PUBLISH_DRY_RUN/packages/aarch64-$board/"
  exit 0
fi
[[ -n ${TILO_PACKAGES_TOKEN:-} ]] || die 'TILO_PACKAGES_TOKEN is required: the check needs secrets = ["TILO_PACKAGES_TOKEN"]'
current=$(curl -fsS "$latest/revision" 2>/dev/null) || current=
forward "$current" || exit 0
auth=(-H "Authorization: token $TILO_PACKAGES_TOKEN")
code=$(curl -sS -o /dev/null -w '%{http_code}' -X DELETE "${auth[@]}" "$latest")
[[ $code == 204 || $code == 404 ]] || die "could not replace $latest (HTTP $code)"
# The revision last: a reader that finds it finds the disk it names.
printf '%s\t%s\n' "$out/disk/disk.qcow2" "$latest/$disk" "$staged/$disk.sha256" "$latest/$disk.sha256" \
  > "$staged/uploads.tsv"
TILO_PACKAGE_TOKEN=$TILO_PACKAGES_TOKEN TILO_PACKAGE_UPLOAD_WORKERS=2 \
  bash "$repo_root/tools/upload_immutable_packages.sh" "$staged/uploads.tsv"
printf '%s\t%s\n' "$staged/revision" "$latest/revision" > "$staged/uploads.tsv"
TILO_PACKAGE_TOKEN=$TILO_PACKAGES_TOKEN bash "$repo_root/tools/upload_immutable_packages.sh" "$staged/uploads.tsv"
echo "$latest/$disk ($T_COMMIT)"
