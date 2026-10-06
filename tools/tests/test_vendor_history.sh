#!/usr/bin/env bash
# Exercise vendor-history.sh as it appears in a fresh clone: the standalone
# histories exist only below refs/remotes/origin, not as local branches.

set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/tilo-vendor-history-test.XXXXXX")
trap 'chmod -R u+w "$test_root" 2>/dev/null || true; rm -rf -- "$test_root"' EXIT

# The repository check covers byte equivalence and the documented Tilo
# overlay. The artificial repository below exercises remote-ref resolution.
"$repo_root/tools/vendor-history.sh" verify

isolated_repo=$test_root/repository
export_dir=$test_root/export
git init --quiet "$isolated_repo"

source_objects=$(git -C "$repo_root" rev-parse --path-format=absolute --git-path objects)
printf '%s\n' "$source_objects" >"$isolated_repo/.git/objects/info/alternates"

# Materialize the current main tree as a fresh clone would. The
# standalone histories themselves remain remote-tracking refs below.
main_tip=$(git -C "$repo_root" rev-parse HEAD)
git -C "$isolated_repo" update-ref refs/heads/main "$main_tip"
git -C "$isolated_repo" checkout --quiet main

recorded() {
    sed -n "s/^$1=//p" "$repo_root/tools/vendor-history.sh"
}
vello_base=$(recorded vello_base)
vello_tip=$(recorded vello_tip)
winit_base=$(recorded winit_base)
winit_tip=$(recorded winit_tip)
git -C "$isolated_repo" update-ref \
    refs/remotes/origin/vendor-upstream/vello-0.9 "$vello_tip"
git -C "$isolated_repo" update-ref \
    refs/remotes/origin/vendor-upstream/winit-0.30.13 "$winit_tip"

(
    cd "$isolated_repo"
    "$repo_root/tools/vendor-history.sh" verify

    # A stale local topic branch must not mask the immutable origin ref.
    git update-ref refs/heads/vendor-upstream/vello-0.9 "$vello_base"
    git update-ref refs/heads/vendor-upstream/winit-0.30.13 "$winit_base"
    "$repo_root/tools/vendor-history.sh" verify
    "$repo_root/tools/vendor-history.sh" export "$export_dir"
)

test "$(find "$export_dir/vello-0.9-patches" -type f -name '*.patch' | wc -l)" -eq \
    "$(git -C "$repo_root" rev-list --count "$vello_base..$vello_tip")"
test "$(find "$export_dir/winit-0.30.13-patches" -type f -name '*.patch' | wc -l)" -eq \
    "$(git -C "$repo_root" rev-list --count "$winit_base..$winit_tip")"
git bundle verify "$export_dir/vello-0.9.bundle" >/dev/null
git bundle verify "$export_dir/winit-0.30.13.bundle" >/dev/null
git bundle list-heads "$export_dir/vello-0.9.bundle" | rg -q "^$vello_tip "
git bundle list-heads "$export_dir/winit-0.30.13.bundle" | rg -q "^$winit_tip "
cmp "$repo_root/docs/vendor-forks.md" "$export_dir/README.md"

echo "vendor history remote-ref verification and export passed"
