#!/usr/bin/env bash
# The cargo stack.sh builds with: `cargo.sh build ARGS…` compiles on the build
# server with t's cached, incremental build (t build), or with any cargo
# drop-in named by TILO_SH_CARGO (run as `$TILO_SH_CARGO build ARGS…`), which
# must leave the programs in ./target as cargo would. Anything else (metadata)
# is this machine's cargo. It is also the CARGO the web bundle's build script
# gets (crates/browser/tilo-browser-runtime/scripts/build.sh), which reads the
# module where this leaves it.
# `cargo.sh [--fetch PATH]... exec PROGRAM ARGS…` runs PROGRAM on the build
# server and mirrors PATH of its target dir back (`rcargo exec`): what that
# script runs there rather than here (wasm-bindgen, the splitter and wasm-opt,
# gigabytes each). t runs checks, not programs, so with t it is rcargo's;
# without a cargo that runs programs it fails, and the script runs them here.
set -euo pipefail
if [[ ${1:-} == exec || ${1:-} == --fetch ]]; then
  case ${TILO_SH_CARGO:-t} in
    t) command -v rcargo >/dev/null || { echo 'cargo.sh exec: t runs no programs on its server, and there is no rcargo' >&2; exit 2; }
       exec rcargo "$@" ;;
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
