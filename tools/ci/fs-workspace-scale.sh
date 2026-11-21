#!/usr/bin/env bash
# A workspace on a size ladder: does any cost grow with how big the
# workspace is, instead of with what a request touches?
#
#   tools/ci/fs-workspace-scale.sh ladder    in-process passes on production
#                                            code paths; needs no root
#   tools/ci/fs-workspace-scale.sh daemons   two real fs daemons and mounts,
#                                            a second device joining and
#                                            fetching on demand
#   tools/ci/fs-workspace-scale.sh photo     one photo-sized file (256 KiB)
#                                            opened for the first time on the
#                                            second device
#   tools/ci/fs-workspace-scale.sh copy      a folder copied in with `cp -r`
#                                            (TILO_FS_COPY_FILES and
#                                            TILO_FS_COPY_BYTES), read back and
#                                            restarted
#   tools/ci/fs-workspace-scale.sh all       the first two
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
# and, per run, one rung through the daemons (needs /dev/fuse; run it in a
# user namespace as CI does, or as a user who may mount FUSE):
#
#   TILO_FS_LADDER_TOTAL_MIB=10240 \
#     $CARGO exec -- bash tools/ci/fs-workspace-scale.sh daemons
#
# The harnesses remove what they write; a killed run leaves
# TILO_FS_LADDER_DIR / TILO_FS_BENCH_DIR behind, which are under the build's
# target directory.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.."

mode=${1:-all}
target=${CARGO_TARGET_DIR:-target}
export TILO_FS_LADDER_DIR=${TILO_FS_LADDER_DIR:-$target/fs-ladder}

ladder() {
    cargo test --locked -p tilo-fs --test it workspace_ladder:: \
        -- --nocapture --test-threads=1
}

daemons() {
    bash scripts/ci-fuse-preflight.sh
    export TILO_FS_SYNC_BENCH=1 TILO_FS_BENCH_BACKEND=fuse TILO_FS_BENCH_ONLY=ladder
    export TILO_FS_BENCH_DIR=${TILO_FS_BENCH_DIR:-$target/fs-scale-daemons}
    # A device's work is what ships, and a debug daemon writes files an order of
    # magnitude slower; the small-file count stays modest because one fsync per
    # file, not the disk, is what bounds it today.
    export TILO_FS_LADDER_SMALL_FILES=${TILO_FS_LADDER_SMALL_FILES:-1000}
    cargo test --release --locked -p tilo-fs --test it \
        native_sync_bench::a_workspace_of_files_on_a_size_ladder \
        -- --nocapture --test-threads=1
}

case $mode in
ladder) ladder ;;
daemons) daemons ;;
photo)
    bash scripts/ci-fuse-preflight.sh
    export TILO_FS_SYNC_BENCH=1 TILO_FS_BENCH_BACKEND=fuse TILO_FS_BENCH_ONLY=photo
    export TILO_FS_BENCH_DIR=${TILO_FS_BENCH_DIR:-$target/fs-photo}
    cargo test --release --locked -p tilo-fs --test it \
        native_sync_bench::a_photo_the_second_device_never_fetched_opens \
        -- --nocapture --test-threads=1
    ;;
copy)
    # A folder copied in the way a person copies one (`cp -r` through the
    # mount), at the size and file size the caller names.
    bash scripts/ci-fuse-preflight.sh
    export TILO_FS_SYNC_BENCH=1 TILO_FS_BENCH_BACKEND=fuse TILO_FS_BENCH_ONLY=copy
    export TILO_FS_BENCH_DIR=${TILO_FS_BENCH_DIR:-$target/fs-copy}
    cargo test --release --locked -p tilo-fs --test it \
        native_sync_bench::copying_a_folder_in_and_restarting \
        -- --nocapture --test-threads=1
    ;;
all)
    ladder
    daemons
    ;;
*)
    echo "usage: $0 [ladder|daemons|photo|copy|all]" >&2
    exit 2
    ;;
esac
