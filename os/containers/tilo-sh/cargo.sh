#!/usr/bin/env bash
# The cargo stack.sh builds with: `cargo.sh build ARGS…` compiles on the build
# server with t's cached, incremental build (t build), or with any cargo
# drop-in named by TILO_SH_CARGO (run as `$TILO_SH_CARGO build ARGS…`), which
# must leave the programs in ./target as cargo would. Anything else (metadata)
# is this machine's cargo. It is also the CARGO the web bundle's build script
# gets (crates/browser/tilo-browser-runtime/scripts/build.sh), which reads the
# module where this leaves it.
set -euo pipefail
if [[ ${1:-} != build ]]; then
  exec cargo "$@"
fi
shift
case ${TILO_SH_CARGO:-t} in
  t) exec t build "$@" ;;
  *) exec $TILO_SH_CARGO build "$@" ;;
esac
