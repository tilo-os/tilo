#!/usr/bin/env bash
# Publish main's board dev image (publish-os-board-<board> in t.toml), from what
# `os-image.sh board` left in OUT: unsigned, under the development-only name tilo-vm, which
# production policy rejects (os/etc/containers/policy.json).
#
#   os-board-publish.sh BOARD OUT
#
# Pushes registry.tilo.so/tilo/tilo-vm:main-<commit7>-BOARD, which `tilo-vm vm upgrade --from
# sha:<commit>` takes for that board, and moves tilo-vm:BOARD-ci, which installed boards follow
# with `bootc upgrade`, to it, only ever forward along main: when the commit it names now is
# this one's ancestor. Logs in with an identity token for registry.tilo.so (oci-publish.sh).
set -euo pipefail
here=$(dirname -- "${BASH_SOURCE[0]}")
. "$here/lib.sh"
board=${1:?usage: os-board-publish.sh BOARD OUT}
out=${2:?usage: os-board-publish.sh BOARD OUT}
[[ $(channel) == dev ]] || die "board dev images publish from main only; this run is for $T_REF"
require_commit
[[ $(cat "$out/board") == "$board" ]] || die "$out holds the image of $(cat "$out/board"), not $board"
[[ $(cat "$out/revision") == "$T_COMMIT" ]] || die "$out holds the image of $(cat "$out/revision"), the run is for $T_COMMIT"
repository=registry.tilo.so/tilo/tilo-vm
moving=$board-ci
tags=("main-${T_COMMIT:0:7}-$board")

# Whether the moving tag may move here: it names no image yet, this commit, or an ancestor of
# it. Main's builds may finish out of order; an older one never takes the tag back.
# With TILO_PUBLISH_DRY_RUN the tag is looked up in the layout oci-publish.sh writes instead.
forward() {
  local current ahead ref=docker://$repository:$moving
  [[ -n ${TILO_PUBLISH_DRY_RUN:-} ]] && ref=oci:$TILO_PUBLISH_DRY_RUN/$repository:$moving
  current=$(skopeo inspect --format '{{index .Labels "org.opencontainers.image.revision"}}' "$ref" 2>/dev/null) \
    || return 0
  [[ -z $current || $current == "<no value>" || $current == "$T_COMMIT" ]] && return 0
  # The commits the tag's commit has that this one lacks: none, when it is an ancestor.
  ahead=$(curl -fsS "https://code.tilo.so/api/v1/repos/tilo/tilo/compare/$T_COMMIT...$current" \
    | sed -n 's/.*"total_commits":\([0-9]*\).*/\1/p') || ahead=
  if [[ $ahead == 0 ]]; then
    return 0
  fi
  printf '%s:%s names %s, %s; leaving it\n' "$repository" "$moving" "$current" \
    "${ahead:+which is not an ancestor of $T_COMMIT}${ahead:-whose history could not be compared}" >&2
  return 1
}
forward && tags+=("$moving")
bash "$here/oci-publish.sh" dev "$repository" "$out" "${tags[@]}"
