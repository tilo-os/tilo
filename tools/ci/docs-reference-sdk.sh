#!/usr/bin/env bash
# The TypeScript SDK's reference page, read from its sources with the TypeScript
# compiler API.
#
# The SDK's packages come from the ci-sdk image's install cache, offline, as the
# SDK's test suite reads them: `node_modules` is not part of the tree, so the
# install fills a copy and the generator loads `typescript` from there. The page
# itself is written from the tree's `sdk/typescript/src`, which is read-only.
#
# usage: docs-reference-sdk.sh [output directory]
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
out=${1:-/target/docs/generated}
work=$(mktemp -d "${TMPDIR:-/tmp}/docs-reference.XXXXXX")
trap 'rm -rf "$work"' EXIT
tar -C "$root/sdk/typescript" --exclude=node_modules -cf - . | tar -C "$work" -xf -
cd "$work"
export BUN_INSTALL_CACHE_DIR=/opt/tilo-sdk/bun-cache
bun install --frozen-lockfile --offline
mkdir -p "$out"
TILO_TYPESCRIPT_MODULE="$work/node_modules/typescript/lib/typescript.js" \
  bun "$root/tools/docs/reference/typescript_sdk.ts" --root "$root" --out "$out/typescript-sdk.md"
