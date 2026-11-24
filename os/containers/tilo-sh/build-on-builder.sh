#!/usr/bin/env bash
# The local stack's builds, run on the build server in one command (stack.sh
# runs it there as `$TILO_SH_CARGO --fetch STAGE exec bash
# os/containers/tilo-sh/build-on-builder.sh RUN STAGE …`, with the drop-in
# `TILO_SH_CARGO` names by default), in that checkout's build directory, which
# stays from one run to the next, and staged in $CARGO_TARGET_DIR/STAGE for the
# one fetch to bring back:
#
#   programs   the programs every service runs (Cargo's lounge-local:
#              incremental, without the whole-program link), into
#              STAGE/programs; a program that did not change keeps its file
#              there, so the fetch passes over it
#   bundle     the web entry's bundle module (build.sh --profile quick
#              --wasm-only --update): compiled and bound, into
#              STAGE/bundle
#
# Both at once: their profiles are their own, so neither waits for the
# other's build directory lock, and one command takes one admission to the
# build server. Each one's output is prefixed with its name.
#
# usage: build-on-builder.sh RUN STAGE [programs] [bundle]
# RUN is written to STAGE/run when everything built, so the caller knows what
# it fetched is this run's.
set -euo pipefail

usage='usage: build-on-builder.sh RUN STAGE [programs] [bundle]'
run=${1:?$usage}
stage=${2:?$usage}
shift 2
[[ -n ${CARGO_TARGET_DIR:-} ]] || {
  echo 'this runs on the build server, in the build container that sets CARGO_TARGET_DIR' >&2; exit 2;
}
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
programs=0 bundle=
while [[ $# -gt 0 ]]; do
  case $1 in
    programs) programs=1; shift ;;
    bundle) bundle=$stage/bundle; shift ;;
    *) echo "$usage" >&2; exit 2 ;;
  esac
done

# Each build's lines, named, and its time.
labelled() {
  local name=$1 began=$SECONDS status=0
  shift
  "$@" > >(sed -u "s/^/$name: /") 2>&1 || status=$?
  echo "$name: $( ((status == 0)) && echo built || echo failed) in $((SECONDS - began))s" >&2
  return "$status"
}

build_programs() {
  cd "$repo"
  # The server and its worker are built apart from the rest, as the Lounge image builds them:
  # in one build Cargo turns on every feature any of the programs asks of a shared crate for
  # all of them, and the server would link the desktop libraries the daemon needs.
  cargo build --locked --profile lounge-local --features tilo-runtime/agents \
    -p tilo-runtime -p discord-worker --bin tilo-runtime --bin discord-worker || return
  cargo build --locked --profile lounge-local \
    -p tilo-api -p fake-discord -p fake-model-providers -p tilo-cli -p tilolang-process \
    --bin tilo-api --bin fake-discord --bin fake-model-providers --bin tilo --bin tilolang-process || return
  # An example is built with its package's dev-dependencies, and Cargo turns their features on
  # for every package in the same build: built beside the server, it would build the server with
  # its test-only features (fixture fonts its app workers cannot load). It is built on its own.
  cargo build --locked --profile lounge-local -p tilo-account-probe --example mail_stand_in || return
  local dir=$CARGO_TARGET_DIR/$stage/programs program
  mkdir -p "$dir" "$dir/examples"
  for program in tilo-runtime discord-worker tilo-api fake-discord fake-model-providers tilo tilolang-process; do
    cmp -s "$CARGO_TARGET_DIR/lounge-local/$program" "$dir/$program" && continue
    cp --reflink=auto "$CARGO_TARGET_DIR/lounge-local/$program" "$dir/.$program.next"
    mv -f "$dir/.$program.next" "$dir/$program"
  done
  # The stand-in mail server is an example, since it makes a certificate with the
  # crate's test-time dependencies; the stack starts it as mail-stand-in.
  program=$CARGO_TARGET_DIR/lounge-local/examples/mail_stand_in
  if ! cmp -s "$program" "$dir/examples/mail_stand_in"; then
    cp --reflink=auto "$program" "$dir/examples/.mail_stand_in.next"
    mv -f "$dir/examples/.mail_stand_in.next" "$dir/examples/mail_stand_in"
  fi
}

# The wasm-bindgen the bundle binds with, at the crate's pinned version. The build image carries
# it (os/Containerfile.builder takes it from this crate's manifest; dist/dev/Containerfile.fedora44
# carries the same version), so a build container, which has no network, never installs one. A
# build whose container does have a network may build the native tool into the build directory
# once: the release binary the image carries is statically linked against musl, whose allocator
# spent minutes contending for memory on the module (203 s on this build server against 32 s for
# the same output), more the more threads.
bindgen_tool() {
  local pinned tool carried
  pinned=$(sed -n 's/^wasm-bindgen = "=\(.*\)"$/\1/p' "$repo/crates/core/tilo-browser-runtime/Cargo.toml")
  # A tool an earlier build made here is this tree's own, and native where the image's is musl.
  tool=$CARGO_TARGET_DIR/tools/wasm-bindgen-$pinned
  if [[ -x $tool/bin/wasm-bindgen ]]; then
    printf '%s\n' "$tool/bin/wasm-bindgen"
    return
  fi
  if carried=$(command -v wasm-bindgen 2>/dev/null) &&
    [[ $("$carried" --version 2>/dev/null) == "wasm-bindgen $pinned" ]]; then
    printf '%s\n' "$carried"
    return
  fi
  if ! cargo install --quiet --locked wasm-bindgen-cli --version "=$pinned" --root "$tool.next" \
    --target-dir "$CARGO_TARGET_DIR/tools/build" >&2; then
    # Refused rather than bound with another version: the CLI must be the crate's own.
    echo "wasm-bindgen $pinned is not in this build container and cannot be installed without a network:" \
      "the image carries ${carried:+$("$carried" --version 2>/dev/null)}${carried:-none}." \
      "Build the image from this commit (os/Containerfile.builder, dist/dev/Containerfile.fedora44)," \
      "which pins the crate's version, or build with a network once." >&2
    return 1
  fi
  rm -rf "$tool"
  mv "$tool.next" "$tool"
  printf '%s\n' "$tool/bin/wasm-bindgen"
}

build_bundle() {
  local bindgen
  bindgen=$(bindgen_tool) || { echo 'could not build wasm-bindgen' >&2; return 1; }
  WASM_BINDGEN=$bindgen bash "$repo/crates/core/tilo-browser-runtime/scripts/build-wasm-on-builder.sh" "$bundle" "$run" \
    --profile quick --no-wasm-opt --update
}

mkdir -p "$CARGO_TARGET_DIR/$stage"
rm -f "$CARGO_TARGET_DIR/$stage/run"
pids=()
if [[ $programs == 1 ]]; then labelled programs build_programs & pids+=($!); fi
if [[ -n $bundle ]]; then labelled bundle build_bundle & pids+=($!); fi
failed=0
for pid in "${pids[@]}"; do wait "$pid" || failed=1; done
[[ $failed == 0 ]] || exit 1
printf '%s\n' "$run" > "$CARGO_TARGET_DIR/$stage/run"
