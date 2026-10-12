#!/usr/bin/env bash
# Compile authored units with the exact signed SDK compiler inventory.
# Cargo still supplies the checkout's original hardening flags.
set -euo pipefail
compiler=$1
shift
crate_name=""
previous=""
query=false
for argument in "$@"; do
    case "$argument" in --print|--print=*|-vV|--version) query=true ;; esac
    if [ "$previous" = "--crate-name" ]; then
        crate_name=$argument
    fi
    previous=$argument
done
if [ -z "$crate_name" ] || "$query"; then
    exec "$compiler" "$@"
fi
case ":${TILO_SDK_AUTHORED_CRATES:?authored unit inventory required}:" in
    *":$crate_name:"*) ;;
    *) echo 'SDK consumer refused a dependency rebuild' >&2; exit 1 ;;
esac
: "${TILO_SDK_COMPILER:?signed compiler bundle required}"
: "${TILO_SDK_RUNTIME:?signed runtime bundle required}"
ui=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["ui"])' \
    "$TILO_SDK_COMPILER/lib/compiler-interface/manifest.json")
case "$ui" in
    libtilo_app-*.rlib) ;;
    *) echo 'SDK compiler has an invalid public unit' >&2; exit 1 ;;
esac
exec "$compiler" "$@" \
    -L "dependency=$TILO_SDK_COMPILER/lib/compiler-interface/artifacts" \
    -L "dependency=$TILO_SDK_RUNTIME/lib" \
    --extern "ui=$TILO_SDK_COMPILER/lib/compiler-interface/artifacts/$ui" \
    --extern "runtime_dylib=$TILO_SDK_RUNTIME/lib/libruntime_dylib.so" \
    -C prefer-dynamic -C opt-level=z -C codegen-units=1 \
    -C llvm-args=--enable-merge-functions \
    -C llvm-args=--enable-machine-outliner=always \
    -C link-arg=-Wl,-z,pack-relative-relocs
