#!/usr/bin/env bash
# Derive the image to deploy from a freshly built web entry image (INCOMING) and the image it
# replaces (OUTGOING): INCOMING plus OUTGOING's own v/<version>/ (Containerfile.carry), written
# as an OCI archive with its manifest digest beside it (OUT/image.oci.tar, OUT/digest), as
# tools/ci/publish/oci-build.sh writes a build. A tab still on the outgoing version can then open
# an app it has not loaded yet without a reload.
#
# Only the outgoing image's own version is carried, the one its version.json names, never a
# version it carried itself; and INCOMING must serve exactly its own version. So an image holds
# at most two versions, and a tab survives one deploy, not two. When both images hold the same
# version there is nothing to carry and the archive is INCOMING's.
#
# Prints the carried version, or nothing.
# usage: carry.sh INCOMING OUTGOING OUT
# Each image is an OCI archive or a reference buildah pulls (docker://…@sha256:…). It runs in
# the buildah image as root in a micro-VM step (the t check carry-web-entry), without a daemon.
set -euo pipefail
usage='usage: carry.sh INCOMING OUTGOING OUT'
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
. "$here/../../../../tools/ci/publish/lib.sh"
use_buildah web-entry overlay
fail() { echo "carry: $*" >&2; exit 1; }
reference() { if [[ -f $1 ]]; then printf 'oci-archive:%s' "$1"; else printf '%s' "$1"; fi; }
incoming_ref=${1:?$usage}
outgoing_ref=${2:?$usage}
out=${3:?$usage}
incoming=$(b pull --quiet "$(reference "$incoming_ref")")
outgoing=$(b pull --quiet "$(reference "$outgoing_ref")")

containers=()
trap 'for c in "${containers[@]}"; do b rm "$c" >/dev/null 2>&1 || true; done' EXIT
# mount_files IMAGE: mount its files at $files.
mount_files() {
  local c
  c=$(b from --pull=never --quiet "$1")
  containers+=("$c")
  files=$(b mount "$c")
}
version_of() {
  local version
  version=$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["version"])' "$1/srv/tilo-web/version.json")
  [[ $version =~ ^[0-9a-f]{20}$ ]] || fail "$2: version.json names no version: $version"
  echo "$version"
}
versions_in() { ls "$1/srv/tilo-web/v" | sort; }
platform_of() { b inspect --type image --format '{{.OCIv1.OS}}/{{.OCIv1.Architecture}}' "$1"; }

mount_files "$incoming"
incoming_root=$files
mount_files "$outgoing"
outgoing_root=$files
new=$(version_of "$incoming_root" "$incoming_ref")
[[ $(versions_in "$incoming_root") == "$new" ]] \
  || fail "$incoming_ref must serve only its own version $new, not: $(versions_in "$incoming_root" | tr '\n' ' ')"
old=$(version_of "$outgoing_root" "$outgoing_ref")
[[ -f $outgoing_root/srv/tilo-web/v/$old/build.json ]] || fail "$outgoing_ref does not hold its own version $old"

mkdir -p "$out"
rm -f "$out/image.oci.tar"
if [[ $old == "$new" ]]; then
  echo "carry: $outgoing_ref serves $new as well; nothing to carry" >&2
  if [[ -f $incoming_ref ]]; then
    cp "$incoming_ref" "$out/image.oci.tar"
  else
    b push --quiet --compression-format gzip --force-compression "$incoming" "oci-archive:$out/image.oci.tar"
  fi
else
  platform=$(platform_of "$incoming")
  [[ $(platform_of "$outgoing") == "$platform" ]] || fail "$outgoing_ref is $(platform_of "$outgoing"), $incoming_ref is $platform"
  b tag "$incoming" localhost/web-entry-carry:incoming
  b tag "$outgoing" localhost/web-entry-carry:outgoing
  context=$scratch/web-entry-carry
  rm -rf "$context" && mkdir -p "$context"
  # A fixed timestamp, as oci-build.sh's: the same two images make the same image.
  b build --quiet --pull=never --layers=false --timestamp 0 --format oci --platform "$platform" \
    -f "$here/Containerfile.carry" --build-arg INCOMING=localhost/web-entry-carry:incoming \
    --build-arg OUTGOING=localhost/web-entry-carry:outgoing --build-arg OUTGOING_VERSION="$old" \
    -t localhost/web-entry-carry:carried "$context" >/dev/null
  mount_files localhost/web-entry-carry:carried
  carried_root=$files
  [[ $(versions_in "$carried_root") == "$(printf '%s\n' "$new" "$old" | sort)" ]] \
    || fail "the carried image serves $(versions_in "$carried_root" | tr '\n' ' '), not $new and $old"
  b push --quiet --compression-format gzip --force-compression localhost/web-entry-carry:carried "oci-archive:$out/image.oci.tar"
fi
archive_digest "$out/image.oci.tar" > "$out/digest"
[[ $(cat "$out/digest") =~ ^sha256:[0-9a-f]{64}$ ]] || fail 'no manifest digest in the archive'
[[ $old == "$new" ]] || echo "$old"
