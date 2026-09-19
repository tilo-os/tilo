#!/usr/bin/env bash
# Stream a packaging checkout into Docker without copying previous build output.
set -euo pipefail
[[ $# == 1 && -d $1 ]] || {
    echo "usage: $0 CHECKOUT" >&2
    exit 2
}

# Keep Git metadata (Arch uses git archive and Nix reads the flake checkout),
# modified/untracked source, and the checksum-pinned external input cache.
# Exclude only these root-level generated trees. In particular, a nested source
# directory named target remains an input. Never delete the original artifacts.
# Transform member/hardlink names, but leave relative symlink targets unchanged.
exec tar --create --file=- --directory="$1" \
    --exclude='./target' --exclude='./dist/ci-output' \
    --transform='flags=rh;s,^\.,workspace,' .
