#!/usr/bin/env bash
# Build the tilolang runtime binary alone, thin, as dist/dev/nested-session-build.sh builds it:
# against the shared runtime library of the default collection's runtime, the one a nested
# session already runs, so `tilolang dev --build-runtime` swaps in a binary of a few megabytes.
#
# It runs where the builds are warm: the tool has rcargo run it on an rcargo builder
# (`rcargo --fetch tilolang-runtime exec`) and fetch the target directory's tilolang-runtime,
# or has t.toml's `tilolang-runtime` check run it and downloads its output with
# `t outputs tilolang-runtime`. A thin binary is of use only with the exact libruntime_dylib.so
# of its build graph, which `t cargo build` and `rcargo build` do not copy back. That library
# travels beside the binary, so the tool can check it is the one the session runs.
#
# Usage:
#   dist/dev/tilolang-runtime-build.sh [OUT]  build into OUT (replaced), by default the target
#                                             directory's tilolang-runtime

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
(($# <= 1)) || { echo "usage: tilolang-runtime-build.sh [OUT]" >&2; exit 2; }
out=${1:+$(realpath -m "$1")}
cd "$repo_root"
target=$(realpath -m "${CARGO_TARGET_DIR:-target}")
out=${out:-$target/tilolang-runtime}
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
