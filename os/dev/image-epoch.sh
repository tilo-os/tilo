#!/usr/bin/env bash
# The SOURCE_DATE_EPOCH an OS image build takes: the newest creation time of
# the images it starts from (the base image and the frozen RPM snapshot).
#
#   image-epoch.sh BASE_REF RPMS_REF...
#
# The value moves only when one of those images does, so two builds of
# different commits share every layer whose content is the same; the commit
# time would put itself into content (the UKI's initramfs, the rpmdb, the
# fontconfig caches) and renew those layers in every build. It never precedes
# an RPM signature the build's offline dnf clock checks, because every
# package is older than the snapshot that holds it. The build steps, chunkah's
# clamp ("Rechunk the image") and the release provenance all take it from here.
set -euo pipefail

[ "$#" -ge 1 ] || { echo "usage: $0 IMAGE_REF..." >&2; exit 2; }
epoch=0
for ref in "$@"; do
  [ -n "$ref" ] || { echo "image-epoch.sh: empty image reference" >&2; exit 2; }
  # A localhost/ image is one this build made (an RPM snapshot not yet published).
  case $ref in
    localhost/*) transport=containers-storage: ;;
    *) transport=docker:// ;;
  esac
  created="$(skopeo inspect "$transport$ref" | jq -er .Created)"
  seconds="$(date -u -d "$created" +%s)"
  if (( seconds > epoch )); then epoch=$seconds; fi
done
(( epoch > 0 )) || { echo "image-epoch.sh: no creation time for $*" >&2; exit 1; }
echo "$epoch"
