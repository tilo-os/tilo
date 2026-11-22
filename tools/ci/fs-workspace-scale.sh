#!/usr/bin/env bash
# A workspace on a size ladder: does any cost grow with how big the
# workspace is, instead of with what a request touches?
#
#   tools/ci/fs-workspace-scale.sh ladder    in-process passes on production
#                                            code paths; needs no root
#
# Every rung writes the same shapes — many small files, one big folder, a few
# huge files — and reports put rate, listing a big folder, the persist a
# change costs, a join and its tree, reading content the device never
# fetched, the metadata each end holds, and restart; then the checks assert
# the work per touched item did not grow with the rung.
#
# The full ladder is a rung list (about 300 GiB in TILO_FS_LADDER_DIR):
#
#   TILO_FS_LADDER_RUNGS_MIB=10240,102400,256000 \
#     $CARGO exec -- bash tools/ci/fs-workspace-scale.sh ladder
#
# The harnesses remove what they write; a killed run leaves
# TILO_FS_LADDER_DIR behind, which are under the build's
# target directory.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.."

mode=${1:-ladder}
target=${CARGO_TARGET_DIR:-target}
export TILO_FS_LADDER_DIR=${TILO_FS_LADDER_DIR:-$target/fs-ladder}

ladder() {
    cargo test --locked -p tilo-fs --test it workspace_ladder:: \
        -- --nocapture --test-threads=1
}

case $mode in
ladder) ladder ;;
*)
    echo "usage: $0 [ladder]" >&2
    exit 2
    ;;
esac
