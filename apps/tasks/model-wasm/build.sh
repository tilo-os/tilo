#!/usr/bin/env bash
# Build the Tasks model module and write the fixture the workspace service's
# packaged-model test loads: zstd-compressed and base64, because the repository
# carries no binary blobs. Build it on a build server first, then encode it:
#
#   rcargo --fetch wasm32-unknown-unknown/release/tasks_model_wasm.wasm build \
#     --config apps/tasks/model-wasm/model-wasm.toml \
#     --manifest-path apps/tasks/model-wasm/Cargo.toml \
#     --target wasm32-unknown-unknown --release
#   apps/tasks/model-wasm/build.sh
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
wasm="${CARGO_TARGET_DIR:-$root/target}/wasm32-unknown-unknown/release/tasks_model_wasm.wasm"
fixture="$root/crates/server/tilo-workspace-service/tests/fixtures/tasks-model.wasm.zst.b64"
zstd -19 -c "$wasm" | base64 -w 76 > "$fixture"
