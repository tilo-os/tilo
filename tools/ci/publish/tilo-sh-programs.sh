#!/usr/bin/env bash
# The programs the local tilo.sh stack runs natively (os/containers/tilo-sh/stack.sh), in one
# build: the server and the Discord worker as the Lounge image carries them (feature `agents`),
# the API server and the program it starts a task's tilolang programs in (found beside it),
# the fake Discord and the tilo command, at Cargo.toml's `lounge-check` (the
# release's code and opt-level without the whole-program link). One cargo build, so a change
# to the server compiles it once and relinks every program beside it, instead of one check
# waiting on another's build. Reads no git: a commit that changes none of what it compiles
# takes the last build. usage: tilo-sh-programs.sh OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
out=${1:?usage: tilo-sh-programs.sh OUT}
cd "$repo_root"
target=${CARGO_TARGET_DIR:-$repo_root/target}
bins=(tilo-server discord-worker tilo-api fake-discord fake-model-providers tilo tilolang-process)
rm -rf "$out"
mkdir -p "$out"
cargo build --locked --profile lounge-check --features tilo-server/agents \
  -p tilo-server -p discord-worker -p tilo-api -p fake-discord -p fake-model-providers -p tilo-cli \
  -p tilolang-process \
  -p tilo-account-probe --example mail_stand_in \
  "${bins[@]/#/--bin=}"
for bin in "${bins[@]}"; do cp "$target/lounge-check/$bin" "$out/"; done
# The stand-in mail server is an example, since it makes a certificate with the
# crate's test-time dependencies; the stack starts it as mail-stand-in.
mkdir -p "$out/examples"
cp "$target/lounge-check/examples/mail_stand_in" "$out/examples/"
