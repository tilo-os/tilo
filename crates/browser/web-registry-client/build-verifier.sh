#!/usr/bin/env bash
# Build the registry verifier (registry-web through registry-web-bindgen) for
# the page: OUT/registry_verifier.js and OUT/registry_verifier_bg.wasm, for
# `registry-verifier.mjs`'s createVerifier.
#
#   crates/browser/web-registry-client/build-verifier.sh /abs/out
#
# Needs the pinned toolchain's wasm32-unknown-unknown target and
# wasm-bindgen 0.2.108; runs wasm-opt when it is installed.
set -euo pipefail

out=${1:?usage: build-verifier.sh OUT_DIR}
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
target_dir=${CARGO_TARGET_DIR:-$root/target}

CARGO_PROFILE_RELEASE_DEBUG=false CARGO_PROFILE_RELEASE_OPT_LEVEL=s \
cargo build --locked --manifest-path "$root/Cargo.toml" -j "${CARGO_BUILD_JOBS:-3}" \
  -p registry-web-bindgen --lib --release --target wasm32-unknown-unknown
wasm-bindgen --target web --no-typescript --out-dir "$out" --out-name registry_verifier \
  "$target_dir/wasm32-unknown-unknown/release/registry_web_bindgen.wasm"
if command -v wasm-opt >/dev/null; then
  wasm-opt -Oz --strip-debug --strip-producers --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
    --enable-mutable-globals --enable-reference-types --enable-multivalue \
    -o "$out/registry_verifier_bg.wasm" "$out/registry_verifier_bg.wasm"
fi
ls -l "$out"/registry_verifier*
