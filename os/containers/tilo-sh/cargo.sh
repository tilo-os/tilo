#!/usr/bin/env bash
# The cargo stack.sh builds with: `cargo.sh build ARGS…` compiles on the build
# server, with t's cached, incremental build (t build), or with rcargo when
# TILO_SH_CARGO=rcargo, and brings the programs back to ./target. Anything else
# (metadata) is this machine's cargo. It is also the CARGO the web bundle's
# build script gets (crates/browser/tilo-browser-runtime/scripts/build.sh),
# which reads the module where this leaves it.
set -euo pipefail
if [[ ${1:-} != build ]]; then
  exec cargo "$@"
fi
shift
case ${TILO_SH_CARGO:-t} in
  t) exec t build "$@" ;;
  rcargo)
    # rcargo brings back the programs a build links; a library compiled to
    # WebAssembly is fetched by name: <target>/<profile dir>/<package>.wasm.
    target='' profile=dev package='' args=("$@")
    while (($#)); do
      case $1 in
        --target) target=$2; shift ;;
        --profile) profile=$2; shift ;;
        -p|--package) package=$2; shift ;;
      esac
      shift
    done
    fetch=()
    if [[ $target == wasm32-* && -n $package ]]; then
      [[ $profile == dev ]] && profile=debug
      fetch=(--fetch "$target/$profile/${package//-/_}.wasm")
    fi
    exec rcargo "${fetch[@]}" build "${args[@]}" ;;
  *) echo "TILO_SH_CARGO is t or rcargo, not $TILO_SH_CARGO" >&2; exit 2 ;;
esac
