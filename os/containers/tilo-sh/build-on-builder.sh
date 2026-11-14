#!/usr/bin/env bash
# The local stack's builds, run on the build server in one command (stack.sh
# runs it there as `$TILO_SH_CARGO --fetch STAGE exec bash
# os/containers/tilo-sh/build-on-builder.sh RUN STAGE …`, with rcargo by
# default), in that checkout's build directory, which stays from one run to
# the next, and staged in $CARGO_TARGET_DIR/STAGE for the one fetch to bring
# back:
#
#   programs   the programs every service runs (Cargo's lounge-local:
#              incremental, without the whole-program link), into
#              STAGE/programs; a program that did not change keeps its file
#              there, so the fetch passes over it
#   bundle     the desktop bundle's module (build.sh --profile local
#              --wasm-only --update): compiled, bound and split, into
#              STAGE/bundle (STAGE/bundle-names with --names)
#
# Both at once: their profiles are their own, so neither waits for the
# other's build directory lock, and one command takes one admission to the
# build server. Each one's output is prefixed with its name.
#
# usage: build-on-builder.sh RUN STAGE [programs] [bundle [--names]]
# RUN is written to STAGE/run when everything built, so the caller knows what
# it fetched is this run's.
set -euo pipefail

usage='usage: build-on-builder.sh RUN STAGE [programs] [bundle [--names]]'
run=${1:?$usage}
stage=${2:?$usage}
shift 2
[[ -n ${CARGO_TARGET_DIR:-} ]] || {
  echo 'this runs on the build server, in the build container that sets CARGO_TARGET_DIR' >&2; exit 2;
}
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
programs=0 bundle= names=()
while [[ $# -gt 0 ]]; do
  case $1 in
    programs) programs=1; shift ;;
    bundle) bundle=$stage/bundle; shift ;;
    --names) names=(--names); shift ;;
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
  cargo build --locked --profile lounge-local --features tilo-relay/agents \
    -p tilo-relay -p discord-worker -p tilo-api -p fake-discord -p tilo-cli \
    --bin tilo-relay --bin discord-worker --bin tilo-api --bin fake-discord --bin tilo
  local dir=$CARGO_TARGET_DIR/$stage/programs program
  mkdir -p "$dir"
  for program in tilo-relay discord-worker tilo-api fake-discord tilo; do
    cmp -s "$CARGO_TARGET_DIR/lounge-local/$program" "$dir/$program" && continue
    cp --reflink=auto "$CARGO_TARGET_DIR/lounge-local/$program" "$dir/.$program.next"
    mv -f "$dir/.$program.next" "$dir/$program"
  done
}

# The wasm-bindgen the bundle binds with: the crate's pinned version, built
# once into the build directory for this machine's C library. The release
# binary the build image carries is statically linked against musl, whose
# allocator spent minutes contending for memory on the module (203 s on this
# build server against 32 s for the same output), more the more threads.
bindgen_tool() {
  local pinned tool
  pinned=$(sed -n 's/^wasm-bindgen = "=\(.*\)"$/\1/p' "$repo/crates/browser/tilo-browser-runtime/Cargo.toml")
  tool=$CARGO_TARGET_DIR/tools/wasm-bindgen-$pinned
  if [[ ! -x $tool/bin/wasm-bindgen ]]; then
    cargo install --quiet --locked wasm-bindgen-cli --version "=$pinned" --root "$tool.next" \
      --target-dir "$CARGO_TARGET_DIR/tools/build" >&2 || return 1
    rm -rf "$tool"
    mv "$tool.next" "$tool"
  fi
  printf '%s\n' "$tool/bin/wasm-bindgen"
}

build_bundle() {
  local bindgen
  bindgen=$(bindgen_tool) || { echo 'could not build wasm-bindgen' >&2; return 1; }
  [[ ${#names[@]} == 0 ]] || bundle+=-names
  WASM_BINDGEN=$bindgen bash "$repo/crates/browser/tilo-browser-runtime/scripts/build-wasm-on-builder.sh" "$bundle" "$run" \
    --bundle desktop --profile local --no-wasm-opt --update "${names[@]}"
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
