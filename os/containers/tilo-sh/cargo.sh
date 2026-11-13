#!/usr/bin/env bash
# The cargo stack.sh builds with: `cargo.sh build ARGS…` compiles on the build
# server with t's cached, incremental build (t build), or with any cargo
# drop-in named by TILO_SH_CARGO (run as `$TILO_SH_CARGO build ARGS…`), which
# must leave the programs in ./target as cargo would. Anything else (metadata)
# is this machine's cargo. It is also the CARGO the web bundle's build script
# gets (crates/browser/tilo-browser-runtime/scripts/build.sh), which reads the
# module where this leaves it.
# `cargo.sh [--fetch PATH]... exec PROGRAM ARGS…` runs PROGRAM on the build
# server and mirrors PATH of its target dir back (`$TILO_SH_CARGO exec`): what
# that script runs there rather than here (wasm-bindgen, the splitter and
# wasm-opt, gigabytes each). t builds and checks, never programs, so exec asks
# TILO_SH_CARGO to name a cargo drop-in that runs them; without one it fails,
# and the script runs the programs here.
set -euo pipefail
if [[ ${1:-} == exec || ${1:-} == --fetch ]]; then
  case ${TILO_SH_CARGO:-} in
    ''|t) echo 'cargo.sh exec: t runs no programs on its builder; name a cargo drop-in that does in TILO_SH_CARGO' >&2; exit 2 ;;
    *) exec $TILO_SH_CARGO "$@" ;;
  esac
fi
if [[ ${1:-} != build ]]; then
  exec cargo "$@"
fi
shift
case ${TILO_SH_CARGO:-t} in
  t) exec t build "$@" ;;
  *) exec $TILO_SH_CARGO build "$@" ;;
esac
