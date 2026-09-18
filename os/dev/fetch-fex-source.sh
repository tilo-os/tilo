#!/usr/bin/env bash
# Acquire/verify the immutable FEX source before an offline native build.
# Existing worktrees must already match; never reset or clean caller files.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
source "$repo/os/external-assets/fex-source.env"
destination=${1:?usage: fetch-fex-source.sh DESTINATION}
[[ "$FEX_SOURCE_REVISION" =~ ^[0-9a-f]{40}$ ]]
if [ ! -e "$destination" ]; then
    mkdir -p "$(dirname -- "$destination")"
    git clone --no-checkout "$FEX_SOURCE_URL" "$destination"
    git -C "$destination" checkout --detach "$FEX_SOURCE_REVISION"
    git -C "$destination" submodule update --init --recursive --depth 1
fi
test -d "$destination/.git"
test "$(git -C "$destination" remote get-url origin)" = "$FEX_SOURCE_URL"
test "$(git -C "$destination" rev-parse HEAD)" = "$FEX_SOURCE_REVISION"
if [ -n "$(git -C "$destination" status --porcelain --untracked-files=all)" ]; then
    echo 'FEX source has local changes or untracked files; refusing to use it' >&2
    exit 1
fi
status=$(git -C "$destination" submodule status --recursive)
if [ -z "$status" ] || grep -Eq '^[-+U]' <<< "$status"; then
    echo 'FEX submodules are missing, conflicted or differ from their pinned commits' >&2
    exit 1
fi
# Root status may not report untracked files nested inside a submodule.
git -C "$destination" submodule foreach --quiet --recursive \
    'test -z "$(git status --porcelain --untracked-files=all)"'
printf 'FEX source revision: %s\n%s\n' "$FEX_SOURCE_REVISION" "$status"
