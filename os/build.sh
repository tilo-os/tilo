#!/bin/bash
# Build the Tilo image and a bootable qcow2.
#
#   ./os/build.sh                        the full image
#   TILO_WITH_BROWSER=0 ./os/build.sh     without the browser + its WPE WebKit
#                                        engine stage (~45 of the ~60 minutes)
#
# This is a wrapper around os/dev/build-image.sh, which is the one implementation.
# It used to be a second one, and that is a hazard rather than a duplication:
# building a sealed image takes a generation number resolved from the published
# series and a second pass when `bootc install` disagrees with `bootc container
# ukify` about the composefs digest. A copy of the pipeline that did not know
# about either would still produce a qcow2 — one carrying generation 0, whose
# rollback floor can never move, discovered months later on a machine that stops
# unsealing.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "=== Building Tilo Image ==="
exec "$SCRIPT_DIR/dev/build-image.sh" "$@"
