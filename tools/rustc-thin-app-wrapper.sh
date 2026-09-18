#!/usr/bin/env bash
# Add prefer-dynamic only to the selected application binaries and the shared
# runtime dylib. App-local code is optimized for size so independently
# distributed qpkgs stay below the 5 MiB safety ceiling; the shared runtime
# remains speed-optimized.
# Cargo invokes this as RUSTC_WORKSPACE_WRAPPER, so third-party dependencies and
# build scripts retain the workspace's ordinary hardening and never become host
# tools that unexpectedly require Rust shared libraries.
set -euo pipefail

compiler="$1"
shift
crate_name=""
previous=""
for argument in "$@"; do
    if [ "$previous" = "--crate-name" ]; then
        crate_name="$argument"
        break
    fi
    previous="$argument"
done

case ":${TILO_THIN_BIN_CRATES:-}:" in
    *":${crate_name}:"*)
        link_map=()
        if [ -n "${TILO_THIN_LINK_MAP_DIR:-}" ]; then
            mkdir -p "$TILO_THIN_LINK_MAP_DIR"
            link_map+=("-C" "link-arg=-Wl,-Map=$TILO_THIN_LINK_MAP_DIR/$crate_name.map")
        fi
        exec "$compiler" "$@" -C prefer-dynamic -C opt-level=z \
            -C llvm-args=--enable-merge-functions \
            -C llvm-args=--enable-machine-outliner=always \
            -C link-arg=-Wl,--icf=all "${link_map[@]}"
        ;;
esac

if [ "$crate_name" = "runtime_dylib" ]; then
    exec "$compiler" "$@" -C prefer-dynamic
fi
exec "$compiler" "$@"
