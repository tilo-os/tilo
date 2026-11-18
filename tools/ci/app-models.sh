#!/usr/bin/env bash
# Build every app's model module: the `model.wasm` a package carries so every
# device holding a workspace's keys applies the app's actions with no build of
# its own (crates/sdk/tilo-model-guest). One mechanism for every app: an app
# keeps its model as a guest crate beside it (`apps/tasks` →
# `apps/tasks-model-wasm`), and `tilo-app model` builds that crate for wasm32
# and writes the module under the target directory. An app without a guest
# crate answers none, so this needs no list of apps.
#
#   t test app-models
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo=${CARGO:-cargo}
target=${CARGO_TARGET_DIR:-$PWD/target}
"$cargo" build --locked -p tilo-app-cli --bin tilo-app
app=$target/debug/tilo-app
for directory in apps/*/; do
  directory=${directory%/}
  # Only an app keeps a model: a crate beside the apps (`tasks-model`, a
  # generated app's library) has no `tilo-app.toml`.
  [ -f "$directory/tilo-app.toml" ] || continue
  # The module lands under the target directory as the app's own output, which
  # is where the checks that read one take it from.
  "$app" model "$PWD/$directory" >/dev/null
done
