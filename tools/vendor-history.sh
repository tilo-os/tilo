#!/usr/bin/env bash
# Export and integrity-check the reviewable histories behind the vendored forks.

set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)

vello_ref=refs/heads/vendor-upstream/vello-0.9
vello_base=f21d2c703175f5387315506865343c52f69b5b39
vello_tip=92f96aff358bd01034d50bfd0de8d97b59dde028

winit_ref=refs/heads/vendor-upstream/winit-0.30.13
winit_base=967e2ec045e9485c10cfbd2323a4ede1a3751845
winit_tip=b4ea19a1c6cccfe268ae2fb30f098629d926e5da

verify_ref() {
    local ref=$1
    local expected_base=$2
    local expected_tip=$3
    local actual_base actual_tip

    actual_tip=$(git -C "$repo_root" rev-parse --verify "$ref")
    actual_base=$(git -C "$repo_root" rev-list --max-parents=0 "$ref")
    test "$actual_tip" = "$expected_tip"
    test "$actual_base" = "$expected_base"
}

verify() {
    verify_ref "$vello_ref" "$vello_base" "$vello_tip"
    verify_ref "$winit_ref" "$winit_base" "$winit_tip"
    echo "vendor histories match their recorded roots and tips"
}

export_histories() {
    local output=$1

    test ! -e "$output" || {
        echo "refusing to overwrite existing output: $output" >&2
        exit 2
    }

    verify
    mkdir -p "$output/vello-0.9-patches" "$output/winit-0.30.13-patches"

    git -C "$repo_root" bundle create "$output/vello-0.9.bundle" "$vello_ref"
    git -C "$repo_root" format-patch --quiet \
        --output-directory "$output/vello-0.9-patches" \
        "$vello_base..$vello_ref"

    git -C "$repo_root" bundle create "$output/winit-0.30.13.bundle" "$winit_ref"
    git -C "$repo_root" format-patch --quiet \
        --output-directory "$output/winit-0.30.13-patches" \
        "$winit_base..$winit_ref"

    echo "exported bundles and topic patches to $output"
}

case ${1:-verify} in
    verify)
        verify
        ;;
    export)
        test "$#" -eq 2 || {
            echo "usage: $0 export OUTPUT_DIRECTORY" >&2
            exit 2
        }
        export_histories "$2"
        ;;
    *)
        echo "usage: $0 [verify | export OUTPUT_DIRECTORY]" >&2
        exit 2
        ;;
esac
