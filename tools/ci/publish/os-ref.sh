#!/usr/bin/env bash
# A branch's own dev image of one target (sign-ref-os-<target> and publish-os-ref-<target> in
# t.toml: `t dispatch publish-os-ref-<target> --ref BRANCH --publish`, which `tilo device vm
# upgrade --ref` runs), signed with the development key and published as
#
#   registry.tilo.so/tilo/tilo-vm-ref:ref-<branch>-<commit7>-TARGET
#
# for one device to follow until its channel publishes something newer
# (/usr/libexec/tilo-update-channel pin). The repository is the branches' own: a branch can never
# push to the repositories devices follow channels from, nor sign for them, so it cannot move a
# channel tag. No disk is published.
#
#   os-ref.sh sign TARGET IMAGE OUT      sign IMAGE's build for the branch repository into OUT
#   os-ref.sh publish TARGET IMAGE OUT   publish it with OUT's signature
#
# IMAGE is os-dev-image-TARGET's output for an architecture, os-board-TARGET's for a board.
set -euo pipefail
here=$(dirname -- "${BASH_SOURCE[0]}")
. "$here/lib.sh"
action=${1:?usage: os-ref.sh sign|publish TARGET IMAGE OUT}
target=${2:?}
image=${3:?}
out=${4:?}
repository=registry.tilo.so/tilo/tilo-vm-ref
[[ $(channel) == branch ]] || die "a branch's own build publishes from a branch, not $T_REF"
require_commit
[[ $(cat "$image/revision") == "$T_COMMIT" ]] || die "$image holds the image of $(cat "$image/revision"), the run is for $T_COMMIT"
# OUT holds the image by reference beside its signature, as oci-sign.sh and oci-publish.sh read it.
mkdir -p "$out"
link() {
  local name
  for name in image.oci.tar oci; do
    [[ -e $image/$name ]] && ln -sfn "$(realpath "$image/$name")" "$out/$name"
  done
  cp "$image/digest" "$image/revision" "$out/"
}
case $action in
  sign)
    link
    bash "$here/oci-sign.sh" "$repository" "$out"
    rm -f "$out/image.oci.tar" "$out/oci" ;;
  publish)
    [[ -s $out/signature ]] || die "$out holds no signature: the check needs sign-ref-os-$target"
    link
    bash "$here/oci-publish.sh" branch "$repository" "$out" "ref-$(ref_slug)-${T_COMMIT:0:7}-$target" ;;
  *) die "unknown action $action" ;;
esac
