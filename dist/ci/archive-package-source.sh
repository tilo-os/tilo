#!/usr/bin/env bash
# Stream a packaging checkout into Docker without copying previous build output.
set -euo pipefail
[[ $# == 1 && -d $1 ]] || {
    echo "usage: $0 CHECKOUT" >&2
    exit 2
}

git_dir=$1/.git
# A checkout can borrow its objects from a repository elsewhere: t names its own store in
# .git/objects/info/alternates, and a runner can through GIT_ALTERNATE_OBJECT_DIRECTORIES. The
# copy cannot open that path, so `git archive HEAD` and the Nix flake read fail inside the
# container. Pack the objects the copied commit itself reaches into the checkout first, and leave
# the borrowed path out of the copy.
borrowed=()
if git --git-dir="$git_dir" rev-parse --verify --quiet HEAD >/dev/null 2>&1; then
    if [[ -s $git_dir/objects/info/alternates || -n ${GIT_ALTERNATE_OBJECT_DIRECTORIES:-} ]]; then
        # The commit's own tree and its history's commits: `git archive HEAD` walks the first, and
        # a log or a version's commit number walks the second. Their trees and blobs stay behind,
        # where only an ancestry lookup could reach them.
        { git --git-dir="$git_dir" rev-list --objects --no-walk HEAD
          git --git-dir="$git_dir" rev-list HEAD
        } | git --git-dir="$git_dir" pack-objects --quiet "$git_dir/objects/pack/pack" >/dev/null
        borrowed=(--exclude='./.git/objects/info/alternates')
    fi
fi

# Keep Git metadata (Arch uses git archive and Nix reads the flake checkout),
# modified/untracked source, and the checksum-pinned external input cache.
# Exclude only these root-level generated trees. In particular, a nested source
# directory named target remains an input. Never delete the original artifacts.
# Transform member/hardlink names, but leave relative symlink targets unchanged.
exec tar --create --file=- --directory="$1" \
    --exclude='./target' --exclude='./dist/ci-output' "${borrowed[@]}" \
    --transform='flags=rh;s,^\.,workspace,' .
