#!/usr/bin/env bash
# The local stack's programs, built on the build server in one command
# (stack.sh runs it there as `$TILO_SH_CARGO --fetch STAGE exec bash
# os/containers/tilo-sh/build-on-builder.sh RUN STAGE programs`, with the
# drop-in `TILO_SH_CARGO` names by default), in that checkout's build
# directory, which stays from one run to the next, and staged in
# $CARGO_TARGET_DIR/STAGE for the one fetch to bring back:
#
#   programs   the programs every service runs (Cargo's lounge-local:
#              incremental, without the whole-program link), into
#              STAGE/programs; a program that did not change keeps its file
#              there, so the fetch passes over it
#
# The web entry's bundle is t's web-entry-bundle check, which stack.sh runs
# and fetches itself.
#
# usage: build-on-builder.sh RUN STAGE programs
# RUN is written to STAGE/run when everything built, so the caller knows what
# it fetched is this run's.
set -euo pipefail

usage='usage: build-on-builder.sh RUN STAGE programs'
run=${1:?$usage}
stage=${2:?$usage}
shift 2
[[ -n ${CARGO_TARGET_DIR:-} ]] || {
  echo 'this runs on the build server, in the build container that sets CARGO_TARGET_DIR' >&2; exit 2;
}
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
programs=0
while [[ $# -gt 0 ]]; do
  case $1 in
    programs) programs=1; shift ;;
    *) echo "$usage" >&2; exit 2 ;;
  esac
done
[[ $programs == 1 ]] || { echo "$usage" >&2; exit 2; }

# The build's lines, named, and its time.
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
  cargo build --locked --profile lounge-local --features tilo-daemon/agents \
    -p tilo-daemon -p discord-worker --bin tilo-runtime --bin discord-worker || return
  cargo build --locked --profile lounge-local \
    -p tilo-api -p fake-discord -p fake-model-providers -p tilo-cli -p tilolang-process -p launcher \
    --bin tilo-api --bin fake-discord --bin fake-model-providers --bin tilo --bin tilolang-process \
    --bin tilo-launcher || return
  # An example is built with its package's dev-dependencies, and Cargo turns their features on
  # for every package in the same build: built beside the server, it would build the server with
  # its test-only features (fixture fonts its app workers cannot load). It is built on its own.
  cargo build --locked --profile lounge-local -p tilo-account-probe --example mail_stand_in || return
  local dir=$CARGO_TARGET_DIR/$stage/programs program
  mkdir -p "$dir" "$dir/examples"
  for program in tilo-runtime discord-worker tilo-api fake-discord fake-model-providers tilo tilolang-process tilo-launcher; do
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

mkdir -p "$CARGO_TARGET_DIR/$stage"
rm -f "$CARGO_TARGET_DIR/$stage/run"
labelled programs build_programs
printf '%s\n' "$run" > "$CARGO_TARGET_DIR/$stage/run"
