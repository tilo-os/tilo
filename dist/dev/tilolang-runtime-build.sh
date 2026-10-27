#!/usr/bin/env bash
# Build the tilolang runtime binary alone, thin, as dist/dev/nested-session-build.sh builds it:
# against the shared runtime library of the default collection's runtime, the one a nested
# session already runs, so `tilolang dev --build-runtime` swaps in a binary of a few megabytes.
#
# t.toml's `tilolang-runtime` check runs it, and the tool downloads its output with
# `t outputs tilolang-runtime`: a thin binary is of use only with the exact libruntime_dylib.so
# of its build graph, which `t cargo build` does not copy back. That library travels beside
# the binary, so the tool can check it is the one the session runs.
#
# Usage:
#   dist/dev/tilolang-runtime-build.sh OUT    build into OUT (replaced)

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
out=$(realpath -m "${1:?usage: tilolang-runtime-build.sh OUT}")
cd "$repo_root"
target=$(realpath -m "${CARGO_TARGET_DIR:-target}")
# Build scripts fetch their test assets into the build directory; on t the
# sources are read-only.
export TILO_TEST_ASSET_CACHE=${TILO_TEST_ASSET_CACHE:-$target/test-assets}

runtime_plan=$(python3 tools/apps/app_release.py --collection default)
INCLUDE_PACKAGES=$(jq -r '.packages | join(" ")' <<<"$runtime_plan") \
    BUILD_PACKAGES=tilolang-app \
    APP_FEATURES=$(jq -r '.features | join(",")' <<<"$runtime_plan") \
    TARGET_DIR=$target \
    os/scripts/build-thin-apps.sh

rm -rf "$out"
mkdir -p "$out"
cp -p "$target/thin-app/tilolang-app" "$target/thin-app/libruntime_dylib.so" "$out/"
printf 'built the thin tilolang runtime in %s\n' "$out"
