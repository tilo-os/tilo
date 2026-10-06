#!/usr/bin/env bash
# Stream a packaging checkout into Docker without copying previous build output.
set -euo pipefail
[[ $# == 1 && -d $1 ]] || {
    echo "usage: $0 CHECKOUT" >&2
    exit 2
}

git_dir=$1/.git
# A checkout can borrow objects from a host repository through
# GIT_ALTERNATE_OBJECT_DIRECTORIES; the copy would then lack them, and
# `git archive HEAD` or the Nix flake read fails inside the container. Pack
# every object HEAD reaches into the checkout itself first.
if [[ -n ${GIT_ALTERNATE_OBJECT_DIRECTORIES:-} ]] \
    && git --git-dir="$git_dir" rev-parse --verify --quiet HEAD >/dev/null \
    && ! env -u GIT_ALTERNATE_OBJECT_DIRECTORIES git --git-dir="$git_dir" \
        rev-list --objects --missing=print HEAD 2>/dev/null | { ! grep -q '^?'; }; then
    git --git-dir="$git_dir" pack-objects --revs --quiet \
        "$git_dir/objects/pack/pack" <<<HEAD >/dev/null
fi

# Keep Git metadata (Arch uses git archive and Nix reads the flake checkout),
# modified/untracked source, and the checksum-pinned external input cache.
# Exclude only these root-level generated trees. In particular, a nested source
# directory named target remains an input. Never delete the original artifacts.
# Transform member/hardlink names, but leave relative symlink targets unchanged.
exec tar --create --file=- --directory="$1" \
    --exclude='./target' --exclude='./dist/ci-output' \
    --transform='flags=rh;s,^\.,workspace,' .
