#!/usr/bin/env bash
# Assemble lazy worker assets; also used by the canonical runtime/desktop builder.
set -euo pipefail
repo=$(cd "$(dirname "$0")/../../../.." && pwd)
cd "$repo"
out=${1:-target/browser-compute}
[[ $# == 0 ]] || shift
profile=release
optimize=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --profile) profile=${2:?}; shift 2 ;;
    --wasm-opt) optimize=1; shift ;;
    --no-wasm-opt) optimize=0; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
case "$profile" in
  release) cargo_profile=wasm-release artifact=wasm-release ;;
  assert) cargo_profile=wasm-assert artifact=wasm-assert ;;
  dev) cargo_profile=dev artifact=debug ;;
  *) echo "unsupported profile: $profile" >&2; exit 2 ;;
esac
bindgen=${WASM_BINDGEN:-wasm-bindgen}
pinned=$(sed -n 's/^wasm-bindgen = "=\(.*\)"$/\1/p' crates/browser/tilo-browser-compute/Cargo.toml)
[[ "$("$bindgen" --version)" == "wasm-bindgen $pinned" ]] || { echo "compute needs wasm-bindgen $pinned" >&2; exit 2; }
cargo build --locked -p tilo-browser-compute --lib --target wasm32-unknown-unknown --profile "$cargo_profile"
target_dir=$(cargo metadata --format-version 1 --no-deps --offline | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')
mkdir -p "$out/pkg/compute"
"$bindgen" "$target_dir/wasm32-unknown-unknown/$artifact/tilo_browser_compute.wasm" --target web --out-dir "$out/pkg/compute" --out-name tilo_browser_compute
if [[ $optimize == 1 ]]; then
  wasm-opt -Os --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext --enable-mutable-globals --enable-reference-types --enable-multivalue \
    "$out/pkg/compute/tilo_browser_compute_bg.wasm" -o "$out/pkg/compute/tilo_browser_compute_bg.wasm"
fi
cp crates/browser/tilo-browser-compute/web/compute-{worker,domains,protocol}.mjs "$out/"
node --input-type=module - "$out/pkg/compute/tilo_browser_compute_bg.wasm" "$profile" <<'JS'
import {readFileSync} from 'node:fs';
import {inspectArtifact} from './crates/browser/tilo-browser-compute/scripts/artifact.mjs';
console.log('compute artifact:', inspectArtifact(readFileSync(process.argv[2]), {release:process.argv[3] !== 'dev'}));
JS
