#!/usr/bin/env bash
# The Lounge image's programs: tilo-relay with the feature `agents` (the host's
# agent participant links every web app, since the host mounts the apps' own
# components) and the Discord connector's outside worker, which the bridge
# starts beside the relay. usage: lounge-binaries.sh OUT [PROFILE]
#
# `release` is what the image carries. A check that only runs these two
# programs (e2e, page-tour, the tilo.sh stack) builds `lounge-gate`
# instead: the same code and the same opt-level without the whole-program link,
# which is most of a run and buys nothing a check can see.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
usage='usage: lounge-binaries.sh OUT [PROFILE]'
out=${1:?$usage}
profile=${2:-release}
cd "$repo_root"
target=${CARGO_TARGET_DIR:-$repo_root/target}
rm -rf "$out"
mkdir -p "$out"
case $profile in
  release)
    # Fat LTO over one codegen unit got the relay's link killed for memory;
    # thin LTO keeps these server binaries fast at a fraction of the memory and
    # time.
    export CARGO_PROFILE_RELEASE_LTO=thin CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 ;;
  lounge-gate) ;;
  *) die "unknown profile $profile: release or lounge-gate" ;;
esac
bins=(tilo-relay discord-worker)
cargo build --locked --profile "$profile" -p tilo-relay -p discord-worker \
  --features tilo-relay/agents "${bins[@]/#/--bin=}"
for bin in "${bins[@]}"; do cp "$target/$profile/$bin" "$out/"; done
