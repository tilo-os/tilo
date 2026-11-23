#!/usr/bin/env bash
# The cargo stack.sh builds with: `cargo.sh build ARGS…` compiles with the
# cargo drop-in TILO_SH_CARGO names (default `$CARGO`, else `cargo`; one that
# builds on a build server brings the artifacts back), run as
# `$TILO_SH_CARGO build ARGS…`, which must leave the programs in ./target as
# cargo would. Anything
# else (metadata) is this machine's cargo. It is also the CARGO the web
# bundle's build script gets (crates/core/tilo-browser-runtime/scripts/build.sh),
# which reads the module where this leaves it.
# `cargo.sh [--fetch PATH]... exec PROGRAM ARGS…` runs PROGRAM on the build
# server and mirrors PATH of its target dir back (`$TILO_SH_CARGO exec`): what
# stack.sh and that script run there rather than here (the builds,
# wasm-bindgen and the splitter, gigabytes each). A drop-in that runs no
# programs (t, whose `t build` builds and checks only) fails it, and they run
# here.
set -euo pipefail
drop_in=${TILO_SH_CARGO:-${CARGO:-cargo}}
if [[ ${1:-} == exec || ${1:-} == --fetch ]]; then
  case $drop_in in
    t) echo 'cargo.sh exec: t runs no programs on its builder; name a cargo drop-in that does in TILO_SH_CARGO' >&2; exit 2 ;;
    *) exec $drop_in "$@" ;;
  esac
fi
if [[ ${1:-} != build ]]; then
  exec cargo "$@"
fi
shift
exec $drop_in build "$@"
