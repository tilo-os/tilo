#!/usr/bin/env bash
# The room service image's programs: tilo-relay (feature `agents`, whose agent participant links
# every web app, since the host mounts the apps' own components), tilo-room-service and, once
# the Discord connector ships one, its outside worker. A check of its own, so it compiles beside
# the desktop bundle (web-desktop-bundle.sh) instead of before or after it; room-service-context.sh
# assembles both. usage: room-service-binaries.sh OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
out=${1:?usage: room-service-binaries.sh OUT}
cd "$repo_root"
target=${CARGO_TARGET_DIR:-$repo_root/target}
rm -rf "$out"
mkdir -p "$out"
# Fat LTO over one codegen unit got the relay's link killed for memory; thin LTO keeps these
# server binaries fast at a fraction of the memory and time.
export CARGO_PROFILE_RELEASE_LTO=thin CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
# One cargo invocation for every binary: separate ones resolve features per package set, and the
# room service's own build then compiled hundreds of the relay's crates over again.
bins=(tilo-relay tilo-room-service)
packages=(-p tilo-relay -p tilo-room-service)
worker=$(cargo metadata --locked --no-deps --format-version 1 \
  | jq -r '.packages[] | select(any(.targets[]; .name == "discord-worker" and (.kind | index("bin")))) | .name' | head -1)
if [[ -n $worker ]]; then
  bins+=(discord-worker)
  packages+=(-p "$worker")
fi
cargo build --locked --release "${packages[@]}" --features tilo-relay/agents "${bins[@]/#/--bin=}"
for bin in "${bins[@]}"; do cp "$target/release/$bin" "$out/"; done
