#!/usr/bin/env bash
# Export and integrity-check the recorded histories behind the vendored forks.

set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
script_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

vello_branch=vendor-upstream/vello-0.9
vello_base=8a3898449ae9608feeafdfa5469fff2a180f08d4
vello_tip=851c465d7a9a005c0d3bf1c8fca696369be79776

winit_branch=vendor-upstream/winit-0.30.13
winit_base=9b9d3bf5398c77c3ec8575c4279f01d2ddcc1d59
winit_tip=2be6b107fa58689376d4ec40fedf8b69723a02de

resolve_ref() {
    local branch=$1
    local expected_tip=$2
    local candidate actual
    local -a found=()

    # Topic branches are local in the repository where they are maintained,
    # but a normal clone receives them as remote-tracking refs. Prefer a ref
    # that already matches the immutable recorded tip; this also handles a
    # stale local branch alongside an up-to-date origin branch safely.
    for candidate in "refs/heads/$branch" "refs/remotes/origin/$branch"; do
        if git -C "$repo_root" show-ref --verify --quiet "$candidate"; then
            actual=$(git -C "$repo_root" rev-parse --verify "$candidate^{commit}")
            found+=("$candidate=$actual")
            if test "$actual" = "$expected_tip"; then
                printf '%s\n' "$candidate"
                return
            fi
        fi
    done

    if test "${#found[@]}" -eq 0; then
        echo "missing vendor history '$branch'; fetch it from origin first" >&2
    else
        echo "no ref for '$branch' matches recorded tip $expected_tip" >&2
        printf '  found %s\n' "${found[@]}" >&2
    fi
    return 1
}

verify_ref() {
    local branch=$1
    local expected_base=$2
    local expected_tip=$3
    local ref actual_base actual_tip

    ref=$(resolve_ref "$branch" "$expected_tip")
    actual_tip=$(git -C "$repo_root" rev-parse --verify "$ref^{commit}")
    actual_base=$(git -C "$repo_root" rev-list --max-parents=0 "$ref")
    test "$actual_tip" = "$expected_tip"
    if test "$actual_base" != "$expected_base"; then
        echo "history '$branch' does not have the recorded root $expected_base" >&2
        printf '  found roots: %s\n' "$actual_base" >&2
        return 1
    fi
}

verify() {
    verify_ref "$vello_branch" "$vello_base" "$vello_tip"
    verify_ref "$winit_branch" "$winit_base" "$winit_tip"
    local vello_ref winit_ref
    vello_ref=$(resolve_ref "$vello_branch" "$vello_tip")
    winit_ref=$(resolve_ref "$winit_branch" "$winit_tip")
    python3 "$script_root/tools/verify-vendor-trees.py" \
        --vello-ref "$vello_ref" \
        --winit-ref "$winit_ref"
    echo "vendor histories match their recorded roots, tips, and checked-in sources"
}

export_histories() {
    local output=$1

    test ! -e "$output" || {
        echo "refusing to overwrite existing output: $output" >&2
        exit 2
    }

    local vello_ref winit_ref

    verify
    vello_ref=$(resolve_ref "$vello_branch" "$vello_tip")
    winit_ref=$(resolve_ref "$winit_branch" "$winit_tip")
    mkdir -p "$output/vello-0.9-patches" "$output/winit-0.30.13-patches"

    git -C "$repo_root" bundle create "$output/vello-0.9.bundle" "$vello_ref"
    git -C "$repo_root" format-patch --quiet \
        --output-directory "$output/vello-0.9-patches" \
        "$vello_base..$vello_ref"

    git -C "$repo_root" bundle create "$output/winit-0.30.13.bundle" "$winit_ref"
    git -C "$repo_root" format-patch --quiet \
        --output-directory "$output/winit-0.30.13-patches" \
        "$winit_base..$winit_ref"

    cp "$script_root/docs/vendor-forks.md" "$output/README.md"

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
