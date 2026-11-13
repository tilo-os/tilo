#!/usr/bin/env bash
# build.sh's wasm phase, run on the builder the compile runs on: wasm-bindgen,
# the splitter and wasm-opt are the build's memory, not the compile's, so they
# run beside the compile and only their output travels back.
#
# usage: build-wasm-on-builder.sh STAGE RUN [build.sh arguments...]
#
# Runs in the build container as `$CARGO --fetch STAGE exec bash
# crates/browser/tilo-browser-runtime/scripts/build-wasm-on-builder.sh STAGE
# RUN ...`, which brings $CARGO_TARGET_DIR/STAGE back to this tree's
# target dir (build.sh reads it there). STAGE is rebuilt from nothing each
# time, so a stopped run leaves nothing behind for the next one to reuse;
# with --update (dev and quick loops) it is brought up to date instead, and
# a module that did not change keeps the pkg/ bound from it.
set -euo pipefail

usage='usage: build-wasm-on-builder.sh STAGE RUN [build.sh arguments...]'
stage=${1:?$usage}
run=${2:?$usage}
shift 2
[[ -n ${CARGO_TARGET_DIR:-} ]] || {
  echo 'this runs on the builder, in the build container that sets CARGO_TARGET_DIR' >&2; exit 2;
}
out=$CARGO_TARGET_DIR/$stage
update=0
for arg in "$@"; do [[ $arg != --update ]] || update=1; done
# What a stopped run or a different build left is no output to update.
if [[ $update == 0 || ! -f $out/wasm.json ]]; then rm -rf "$out"; fi
rm -f "$out/run"
# Here is the builder: the steps run beside the compile, never sent on.
bash "$(dirname -- "$0")/build.sh" "$out" --wasm-only "$@" --wasm-steps local
# RUN, which build.sh checks in what the fetch brought back: the output of this
# run and no earlier one.
printf '%s\n' "$run" > "$out/run"
