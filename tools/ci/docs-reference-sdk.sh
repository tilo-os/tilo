#!/usr/bin/env bash
# The TypeScript SDK's reference page, read from its sources with the TypeScript
# compiler API.
#
# The SDK's client is generated from the API's description and never committed, so
# the page is read from a copy of the tree's sdk/typescript with it in place: the types
# the sdk-gen check wrote (sdk-gen.sh), and the SDK's packages installed from the
# ci-sdk image's cache, offline, where the generator loads `typescript` from.
#
# usage: docs-reference-sdk.sh [output directory]
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
out=${1:-/target/docs/generated}
work=$(mktemp -d "${TMPDIR:-/tmp}/docs-reference.XXXXXX")
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/sdk/typescript"
tar -C "$root/sdk/typescript" --exclude=node_modules -cf - . | tar -C "$work/sdk/typescript" -xf -
bash "$root/tools/ci/sdk-gen.sh" place "$work/sdk"
cd "$work/sdk/typescript"
export BUN_INSTALL_CACHE_DIR=/opt/tilo-sdk/bun-cache
bun install --frozen-lockfile --offline
mkdir -p "$out"
TILO_TYPESCRIPT_MODULE="$work/sdk/typescript/node_modules/typescript/lib/typescript.js" \
  bun "$root/tools/docs/reference/typescript_sdk.ts" --root "$work" --out "$out/typescript-sdk.md"
