#!/usr/bin/env bash
# The Lounge image's programs: tilo-relay with the feature `agents` (the host's
# agent participant links every web app, since the host mounts the apps' own
# components) and the Discord connector's outside worker, which the bridge
# starts beside the relay. usage: lounge-binaries.sh OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
out=${1:?usage: lounge-binaries.sh OUT}
cd "$repo_root"
target=${CARGO_TARGET_DIR:-$repo_root/target}
rm -rf "$out"
mkdir -p "$out"
# Fat LTO over one codegen unit got the relay's link killed for memory; thin
# LTO keeps these server binaries fast at a fraction of the memory and time.
export CARGO_PROFILE_RELEASE_LTO=thin CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
bins=(tilo-relay discord-worker)
cargo build --locked --release -p tilo-relay -p discord-worker --features tilo-relay/agents "${bins[@]/#/--bin=}"
for bin in "${bins[@]}"; do cp "$target/release/$bin" "$out/"; done
