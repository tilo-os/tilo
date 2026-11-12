#!/usr/bin/env bash
# The TypeScript SDK's suite and typecheck, run from its lock in the ci-sdk image without
# network.
#
# The image carries bun and its install cache filled from sdk/typescript/bun.lock. bun installs
# into a copy of the tree, since a job runs in the image read-only.
#
# usage: sdk-typescript.sh
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
source_tree=$root/sdk/typescript
work=$(mktemp -d "${TMPDIR:-/tmp}/sdk-typescript.XXXXXX")
trap 'rm -rf "$work"' EXIT
# node_modules is not part of the tree; an install from the image's cache fills the copy.
tar -C "$source_tree" --exclude=node_modules -cf - . | tar -C "$work" -xf -
cd "$work"
export BUN_INSTALL_CACHE_DIR=/opt/tilo-sdk/bun-cache
bun install --frozen-lockfile --offline
bun test
bun run typecheck
