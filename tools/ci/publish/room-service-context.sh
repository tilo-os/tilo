#!/usr/bin/env bash
# The room service image's build context (crates/hosting/tilo-room-service/container): the
# relay, room service and connector binaries, the product fonts, the desktop bundle and pages.
# Built in the CI builder, which has the Rust, wasm and Node toolchains, so the Containerfile
# only copies them. usage: room-service-context.sh OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
out=${1:?usage: room-service-context.sh OUT}
cd "$repo_root"
target=${CARGO_TARGET_DIR:-$repo_root/target}
rm -rf "$out"
mkdir -p "$out/bin" "$out/bots"
# Fat LTO over one codegen unit got the relay's link killed for memory; thin LTO keeps these
# server binaries fast at a fraction of the memory and time.
export CARGO_PROFILE_RELEASE_LTO=thin CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
cargo build --locked --release -p tilo-relay --features agents --bin tilo-relay
cargo build --locked --release -p tilo-room-service
cp "$target/release/tilo-relay" "$target/release/tilo-room-service" "$out/bin/"
export TILO_FONT_ASSET_CACHE=$target/font-artifacts
TILO_FONT_ASSETS_OFFLINE=0 python3 tools/font-catalog.py prepare crates/sdk/ui-core/assets/fonts \
  "$out/bin/fonts" --scope product
package=$(cargo metadata --locked --no-deps --format-version 1 \
  | jq -r '.packages[] | select(any(.targets[]; .name == "discord-worker" and (.kind | index("bin")))) | .name' | head -1)
if [[ -n $package ]]; then
  cargo build --locked --release -p "$package" --bin discord-worker
  cp "$target/release/discord-worker" "$out/bin/"
fi
cp apps/discord/bridge/program.json "$out/bots/discord-bridge.json"
moq=$scratch/moq-browser
rm -rf "$moq" && cp -r scripts/moq-browser "$moq"
TILO_MOQ_BROWSER_DIR=$moq crates/browser/tilo-browser-runtime/scripts/build.sh "$out/assets" --bundle desktop >/dev/null
cp crates/hosting/tilo-room-service/web/* "$out/assets/"
cp crates/hosting/tilo-room-service/container/Containerfile "$out/"
