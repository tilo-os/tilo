#!/bin/bash
# Build the Tilo image and a bootable qcow2.
#
#   ./os/build.sh                        the full image
#   TILO_WITH_BROWSER=0 ./os/build.sh     without the Browser and its Chromium
#                                        engine runtime
#   ./os/build.sh --local                the full image from inputs resolved for
#                                        this machine (os/dev/local-inputs.sh):
#                                        the published RPM snapshot or a local
#                                        one, the pinned or newest CI kernel or a
#                                        local one, the non-production owner key,
#                                        no browser. Nothing to set by hand.
#   ./os/build.sh --dev                  the dev VM image (os/Containerfile.dev)
#                                        and its disk, FROM an already built
#                                        localhost/tilo:latest; see
#                                        os/dev/build-dev-image.sh
#   ./os/build.sh --dev --local          the same, building localhost/tilo:latest
#                                        first with --local's inputs, only when
#                                        its inputs changed. TILO_DEV_DISK=0 for
#                                        the container alone (what
#                                        tilo-vm vm upgrade --from local runs).
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

dev=0; local_inputs=0; rest=()
for a in "$@"; do
  case "$a" in
    --dev) dev=1 ;;
    --local) local_inputs=1 ;;
    *) rest+=("$a") ;;
  esac
done

if [ "$dev" = 1 ]; then
  echo "=== Building Tilo Dev Image ==="
  [ "$local_inputs" = 0 ] || export TILO_DEV_BASE_BUILD="${TILO_DEV_BASE_BUILD:-auto}"
  exec "$SCRIPT_DIR/dev/build-dev-image.sh" "${rest[@]}"
fi

echo "=== Building Tilo Image ==="
if [ "$local_inputs" = 1 ]; then
  # shellcheck disable=SC1091
  source "$SCRIPT_DIR/dev/_common.sh"
  # shellcheck disable=SC1091
  source "$SCRIPT_DIR/dev/local-inputs.sh"
fi
exec "$SCRIPT_DIR/dev/build-image.sh" "${rest[@]}"
