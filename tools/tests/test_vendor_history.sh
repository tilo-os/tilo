#!/usr/bin/env bash
# Exercise vendor-history.sh as it appears in a fresh clone: the standalone
# histories exist only below refs/remotes/origin, not as local branches.

set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/qos-vendor-history-test.XXXXXX")
trap 'chmod -R u+w "$test_root" 2>/dev/null || true; rm -rf -- "$test_root"' EXIT

isolated_repo=$test_root/repository
export_dir=$test_root/export
git init --quiet "$isolated_repo"

source_objects=$(git -C "$repo_root" rev-parse --path-format=absolute --git-path objects)
printf '%s\n' "$source_objects" >"$isolated_repo/.git/objects/info/alternates"

vello_tip=108405981ee51fd9b2c3ee7ef9c3b8b1c6a9d223
winit_tip=b4ea19a1c6cccfe268ae2fb30f098629d926e5da
git -C "$isolated_repo" update-ref \
    refs/remotes/origin/vendor-upstream/vello-0.9 "$vello_tip"
git -C "$isolated_repo" update-ref \
    refs/remotes/origin/vendor-upstream/winit-0.30.13 "$winit_tip"

(
    cd "$isolated_repo"
    "$repo_root/tools/vendor-history.sh" verify

    # A stale local topic branch must not mask the immutable origin ref.
    git update-ref refs/heads/vendor-upstream/vello-0.9 \
        f21d2c703175f5387315506865343c52f69b5b39
    git update-ref refs/heads/vendor-upstream/winit-0.30.13 \
        967e2ec045e9485c10cfbd2323a4ede1a3751845
    "$repo_root/tools/vendor-history.sh" verify
    "$repo_root/tools/vendor-history.sh" export "$export_dir"
)

test "$(find "$export_dir/vello-0.9-patches" -type f -name '*.patch' | wc -l)" -eq 3
test "$(find "$export_dir/winit-0.30.13-patches" -type f -name '*.patch' | wc -l)" -eq 6
git bundle verify "$export_dir/vello-0.9.bundle" >/dev/null
git bundle verify "$export_dir/winit-0.30.13.bundle" >/dev/null
git bundle list-heads "$export_dir/vello-0.9.bundle" | rg -q "^$vello_tip "
git bundle list-heads "$export_dir/winit-0.30.13.bundle" | rg -q "^$winit_tip "

echo "vendor history remote-ref verification and export passed"
