#!/usr/bin/env bash
# Optional wasm32 C compiler for hosts whose system clang lacks WebAssembly.
# ring uses freestanding C; translate Rust's target spelling for Zig's clang.
set -euo pipefail
args=()
for arg in "$@"; do
  if [[ "$arg" == '--target=wasm32-unknown-unknown' ]]; then
    args+=('--target=wasm32-freestanding')
  else
    args+=("$arg")
  fi
done
exec "${QOS_ZIG:-zig}" cc "${args[@]}"
