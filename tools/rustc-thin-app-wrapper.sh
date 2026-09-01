#!/usr/bin/env bash
# Add prefer-dynamic only to the selected application binaries and the shared
# runtime dylib. Cargo invokes this as RUSTC_WORKSPACE_WRAPPER, so third-party
# dependencies and build scripts retain the workspace's ordinary hardening and
# never become host tools that unexpectedly require Rust shared libraries.
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

case ":${QOS_THIN_BIN_CRATES:-}:" in
    *":${crate_name}:"*) exec "$compiler" "$@" -C prefer-dynamic ;;
esac

if [ "$crate_name" = "runtime_dylib" ]; then
    exec "$compiler" "$@" -C prefer-dynamic
fi
exec "$compiler" "$@"
