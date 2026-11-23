#!/usr/bin/env bash
set -euo pipefail
repo=$(cd "$(dirname "$0")/../../../.." && pwd)
cd "$repo"
# Set CC_wasm32_unknown_unknown to a WebAssembly-capable clang, or opt into
# the included Zig wrapper with TILO_USE_ZIG=1. Native compilation is unchanged.
if [[ "${TILO_USE_ZIG:-0}" == 1 ]]; then
  export CC_wasm32_unknown_unknown="$repo/crates/core/tilo-server/scripts/wasm-cc.sh"
  export AR_wasm32_unknown_unknown="$repo/crates/core/tilo-server/scripts/wasm-ar.sh"
fi
cargo build --locked -j2 -p tilo-server --lib --target wasm32-unknown-unknown
