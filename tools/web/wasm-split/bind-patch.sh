#!/usr/bin/env bash
# Keep and reuse what wasm-bindgen made of a module, so a change to one body
# binds only that body (src/bin/bind_patch.rs).
#
#   bind-patch.sh record LINKED BOUND CACHE
#   bind-patch.sh apply  LINKED CACHE OUT
#
# `record` runs after a real bind and keeps the bound module with a map of
# what its functions and types became; `apply` patches the kept bound module
# for a LINKED module that changed in bodies alone, writing the bound module a
# full bind would have written. When it cannot (anything but bodies changed,
# a body changed size or callees, a function it cannot map), it exits 3 and
# the caller binds fully.
#
# The tool compiles with `cargo`, or with the cargo `$CARGO` names, as
# split.sh does; it is read from Cargo's own artifact report so a cargo that
# compiles elsewhere is followed wherever it puts binaries.
set -euo pipefail

usage='usage: bind-patch.sh record LINKED BOUND CACHE | bind-patch.sh apply LINKED CACHE OUT'
command=${1:?$usage}
tool=$(cd "$(dirname "$0")" && pwd)
cargo=${CARGO:-cargo}
target_dir=${CARGO_TARGET_DIR:-$tool/../../../target}/wasm-split-tool
patched=$(cd "$tool" && "$cargo" build --quiet --release --locked \
  --target-dir "$target_dir" --message-format=json-render-diagnostics |
  python3 -c 'import json, sys
for line in sys.stdin:
    if not line.lstrip().startswith("{"):
        continue
    message = json.loads(line)
    if message.get("reason") == "compiler-artifact" and message["target"]["name"] == "bind_patch" \
            and message.get("executable"):
        print(message["executable"])' | tail -n 1)
[[ -x $patched ]] || { echo 'bind_patch was not built' >&2; exit 2; }
# Run a copy, never the shared build's own file (split.sh says why).
tool_copy=${TMPDIR:-/tmp}/.wasm-bind-patch.$$
trap 'rm -f "$tool_copy"' EXIT
cp -- "$patched" "$tool_copy"
if [[ $command == record ]]; then
  "$tool_copy" record --linked "$2" --bound "$3" --cache "$4"
else
  "$tool_copy" apply --linked "$2" --cache "$3" --out "$4"
fi
