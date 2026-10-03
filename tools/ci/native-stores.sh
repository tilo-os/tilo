#!/usr/bin/env bash
# Loop-mounted Btrfs store volumes for the native data plane's privileged
# suites (`fs --test native_tier1`, `fs-native --test it`).
#
#   sudo tools/ci/native-stores.sh up DIR     # prints the environment to use
#   sudo tools/ci/native-stores.sh down DIR   # unmounts and detaches them
#
# `up` makes two sparse images under DIR, attaches each to a loop device,
# formats it Btrfs and mounts its top-level subvolume at DIR/a and DIR/b: a
# dedicated store volume each, as the daemon requires (not the system's
# filesystem, mounted at its root). It prints KEY=VALUE lines for
# TILO_FS_NATIVE_STORES, TILO_FS_NATIVE_STORE and TILO_REQUIRE_FANOTIFY=1, so
# a suite that cannot run fails instead of skipping; append them to
# $GITHUB_ENV in CI, or `export` them locally. `down` is idempotent and
# removes everything `up` made. Needs root, loop devices and Btrfs.
set -euo pipefail

usage() {
    echo "usage: $0 up|down DIR" >&2
    exit 2
}
[[ $# -eq 2 ]] || usage
action=$1
dir=$2
size=${TILO_NATIVE_STORE_SIZE:-4G}

down() {
    local name loop
    for name in a b; do
        if mountpoint -q "$dir/$name"; then
            umount "$dir/$name"
        fi
        if [[ -e $dir/$name.img ]]; then
            while read -r loop; do
                [[ -n $loop ]] && losetup -d "$loop"
            done < <(losetup -j "$dir/$name.img" -O NAME -n)
            rm -f -- "$dir/$name.img"
        fi
        [[ -d $dir/$name ]] && rmdir -- "$dir/$name"
    done
    return 0
}

up() {
    local name loop
    for tool in losetup mkfs.btrfs mountpoint; do
        command -v "$tool" >/dev/null || {
            echo "$0: $tool is required" >&2
            exit 1
        }
    done
    mkdir -p -- "$dir"
    dir=$(cd -- "$dir" && pwd -P)
    trap 'down' ERR
    for name in a b; do
        if mountpoint -q "$dir/$name"; then
            echo "$0: $dir/$name is already mounted; run '$0 down $dir' first" >&2
            exit 1
        fi
        truncate -s "$size" "$dir/$name.img"
        loop=$(losetup --find --show "$dir/$name.img")
        mkfs.btrfs -q -f "$loop" >/dev/null
        mkdir -p -- "$dir/$name"
        mount -t btrfs "$loop" "$dir/$name"
    done
    trap - ERR
    echo "TILO_FS_NATIVE_STORES=$dir/a:$dir/b"
    echo "TILO_FS_NATIVE_STORE=$dir/a"
    echo "TILO_REQUIRE_FANOTIFY=1"
}

case $action in
up) up ;;
down)
    [[ -d $dir ]] || exit 0
    dir=$(cd -- "$dir" && pwd -P)
    down
    ;;
*) usage ;;
esac
