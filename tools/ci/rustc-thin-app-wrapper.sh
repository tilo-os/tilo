#!/usr/bin/env bash
# Add prefer-dynamic only to the selected application binaries and the shared
# runtime dylib. App-local code is optimized for size so independently
# distributed tapps stay below the 5 MiB safety ceiling; the shared runtime
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
        # Release validation raises the parent profile's codegen-unit count to
        # keep the shared runtime build fast. A thin app inherits that parent
        # override unless it is reset here, which made Music's tapp exceed the
        # 5 MiB wire limit despite the declared thin-app profile using one
        # unit. Keep parallel codegen for the large runtime, but optimize each
        # independently shipped executable as one unit.
        exec "$compiler" "$@" -C prefer-dynamic -C opt-level=z -C codegen-units=1 \
            -C llvm-args=--enable-merge-functions \
            -C llvm-args=--enable-machine-outliner=always \
            "${link_map[@]}"
        ;;
esac

if [ "$crate_name" = "runtime_dylib" ]; then
    exec "$compiler" "$@" -C prefer-dynamic
fi
exec "$compiler" "$@"
