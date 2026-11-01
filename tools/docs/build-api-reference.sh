#!/usr/bin/env bash
# Build the tilo-app API reference into OUT. `tilo-app` re-exports the
# toolkit, so this documents the whole app surface. The tilo/site image build
# runs this script to serve the reference at /docs/developers/api.
set -euo pipefail

out=${1:?usage: build-api-reference.sh OUT}
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$repo"

"${CARGO:-cargo}" doc --locked -p tilo-app --no-deps
doc="${CARGO_TARGET_DIR:-$repo/target}/doc"
[[ -f $doc/tilo_app/index.html ]] || {
  printf 'rustdoc wrote no tilo_app/index.html under %s\n' "$doc" >&2
  exit 1
}
rm -rf -- "$out"
mkdir -p -- "$out"
cp -a -- "$doc/." "$out/"
