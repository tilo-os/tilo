#!/usr/bin/env bash
# Build every app's model module: the `model.wasm` a package carries so every
# device holding a workspace's keys applies the app's actions with no build of
# its own (crates/sdk/tilo-model-guest). An app keeps its model as a guest
# crate beside it (`apps/remote` -> `apps/remote-model-wasm`).
#
# The guests are linked into one module (crates/sdk/tilo-models-wasm), which
# `wasm-split --models` cuts into the model runtime every app shares
# (automerge, std, the resource layer, serde_json) and one small module per
# app, which names the runtime it links against. Under the target directory:
#
#   app-models/<app>/model.wasm         what <app>'s package carries
#   app-models/app-model-runtime/       the runtime, which the shared runtime
#                                       bundle carries once (runtime.json names
#                                       its modules and its hash)
#
# A device links an app's module after the runtime it names
# (crates/lang/tilo-model-engine; docs/architecture/apps.md).
#
#   t test app-models
set -euo pipefail
cd "$(dirname "$0")/../.."

cargo=${CARGO:-cargo}
target=${CARGO_TARGET_DIR:-$PWD/target}
combined=crates/sdk/tilo-models-wasm

# Every app's guest crate is one of the combined build's: one left out would
# ship no module, which the packs refuse, but this says why. Only an app keeps
# a model: a crate beside the apps with no `tilo-app.toml` has none.
missing=()
for guest in apps/*-model-wasm/; do
  guest=${guest%/}
  [ -f "${guest%-model-wasm}/tilo-app.toml" ] || continue
  grep -q "path = \"../../../$guest\"" "$combined/Cargo.toml" || missing+=("$guest")
done
if ((${#missing[@]})); then
  printf '%s is not in %s; add it to [dependencies] and an `extern crate` line in src/lib.rs\n' \
    "${missing[@]}" "$combined/Cargo.toml" >&2
  exit 1
fi

# Release-profile overrides a caller exports for its native builds would
# change the bytes every device applies actions with: the workspace's release
# profile and what a model module is sized with below say what they are, as
# `tilolang model` builds a guest alone (crates/lang/tilo-app-builder/src/model_wasm.rs),
# except that the symbol table stays for wasm-split.
while IFS= read -r variable; do
  unset "$variable"
done < <(env | sed -n 's/^\(CARGO_PROFILE_RELEASE_[A-Z_]*\)=.*/\1/p')

# Built in the crate's directory, whose `.cargo/config.toml` makes it a model
# build (no JavaScript glue, `tilo_model_split`, the linker's relocations), in
# a target directory of its own: those flags reach every crate it builds.
build=$target/app-model-build/combined
(cd "$combined" && CARGO_PROFILE_RELEASE_OPT_LEVEL=z CARGO_PROFILE_RELEASE_DEBUG=false \
  CARGO_PROFILE_RELEASE_PANIC=abort CARGO_PROFILE_RELEASE_STRIP=none \
  "$cargo" build --quiet --locked --release -p tilo-models-wasm \
  --target wasm32-unknown-unknown --target-dir "$build")
linked=$build/wasm32-unknown-unknown/release/tilo_models_wasm.wasm
[[ -f $linked ]] || { printf 'the combined model build wrote no %s\n' "$linked" >&2; exit 1; }

# The splitter, read from Cargo's artifact report as tools/web/wasm-split/split.sh
# does, so a cargo that compiles elsewhere is followed.
splitter=$(cd tools/web/wasm-split && "$cargo" build --quiet --release --locked \
  --target-dir "$target/wasm-split-tool" --message-format=json-render-diagnostics |
  python3 -c 'import json, sys
for line in sys.stdin:
    if not line.lstrip().startswith("{"):
        continue
    message = json.loads(line)
    if message.get("reason") == "compiler-artifact" and message["target"]["name"] == "wasm-split" \
            and message.get("executable"):
        print(message["executable"])' | tail -n 1)
[[ -x $splitter ]] || { echo 'wasm-split was not built' >&2; exit 2; }

# Written beside the output and moved into place, so a reader never sees one
# app's module beside another split's runtime.
out=$target/app-models
staged=$out.staging
rm -rf "$staged"
# Each module is optimized as every module the repository ships is, before
# the runtime is named by its bytes.
"$splitter" --models "$linked" "$staged" --wasm-opt tools/web/wasm-opt-flags
rm -rf "$out"
mv "$staged" "$out"
