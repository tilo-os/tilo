#!/usr/bin/env bash
# Build the image filter library `tilolang bench --library` runs into
# target/wasm32-unknown-unknown/release/tilolang_bench_imagefilter.wasm, in the
# repository's build directory. With t, `t outputs bench-imagefilter` builds it
# there and fetches it (docs/tilolang-engines.md).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../../../.." && pwd)"
cargo build --manifest-path "$here/Cargo.toml" --target-dir "${CARGO_TARGET_DIR:-$root/target}" \
    --target wasm32-unknown-unknown --release --locked "$@"
