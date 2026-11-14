#!/usr/bin/env bash
# The programs the local tilo.sh stack runs natively (os/containers/tilo-sh/stack.sh), in one
# build: the relay and the Discord worker as the Lounge image carries them (feature `agents`),
# the API server, the fake Discord and the tilo command, at Cargo.toml's `lounge-gate` (the
# release's code and opt-level without the whole-program link). One cargo build, so a change
# to the relay compiles it once and relinks every program beside it, instead of one check
# waiting on another's build. Reads no git: a commit that changes none of what it compiles
# takes the last build. usage: tilo-sh-programs.sh OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
out=${1:?usage: tilo-sh-programs.sh OUT}
cd "$repo_root"
target=${CARGO_TARGET_DIR:-$repo_root/target}
bins=(tilo-relay discord-worker tilo-api fake-discord tilo)
rm -rf "$out"
mkdir -p "$out"
cargo build --locked --profile lounge-gate --features tilo-relay/agents \
  -p tilo-relay -p discord-worker -p tilo-api -p fake-discord -p tilo-cli "${bins[@]/#/--bin=}"
for bin in "${bins[@]}"; do cp "$target/lounge-gate/$bin" "$out/"; done
