#!/usr/bin/env bash
# The programs the local tilo.sh stack runs natively (os/containers/tilo-sh/stack.sh), at
# Cargo.toml's `lounge-check` (the release's code and opt-level without the whole-program
# link): the server and the Discord worker as the Lounge image carries them (feature `agents`),
# then the API server and the program it starts a task's tilolang programs in (found beside
# it), the fake Discord, the fake model providers, the tilo command and its daemon. Two cargo
# builds, as the images build them: built in one, Cargo would turn on every feature any of
# these programs asks of a shared crate for all of them, and the server would link the
# desktop's GStreamer, input and graphics libraries that the daemon needs and the Lounge image
# does not carry, so it would not start on a machine without them. The mail stand-in is an
# example, built apart for the same reason: an example brings its package's dev-dependencies.
# Reads no git: a commit that changes none of what it compiles takes the last build.
# usage: tilo-sh-programs.sh OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
out=${1:?usage: tilo-sh-programs.sh OUT}
cd "$repo_root"
target=${CARGO_TARGET_DIR:-$repo_root/target}
bins=(tilo-server discord-worker tilo-api fake-discord fake-model-providers tilo tilo-runtime tilolang-process)
rm -rf "$out"
mkdir -p "$out"
server_bins=(tilo-server discord-worker)
cargo build --locked --profile lounge-check --features tilo-server/agents \
  -p tilo-server -p discord-worker "${server_bins[@]/#/--bin=}"
other_bins=(tilo-api fake-discord fake-model-providers tilo tilo-runtime tilolang-process)
cargo build --locked --profile lounge-check \
  -p tilo-api -p fake-discord -p fake-model-providers -p tilo-cli -p tilo-runtime -p tilolang-process \
  "${other_bins[@]/#/--bin=}"
cargo build --locked --profile lounge-check -p tilo-account-probe --example mail_stand_in
for bin in "${bins[@]}"; do cp "$target/lounge-check/$bin" "$out/"; done
# The stand-in mail server is an example, since it makes a certificate with the
# crate's test-time dependencies; the stack starts it as mail-stand-in.
mkdir -p "$out/examples"
cp "$target/lounge-check/examples/mail_stand_in" "$out/examples/"
