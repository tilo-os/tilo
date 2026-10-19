#!/usr/bin/env bash
# Build what tools/screenshots/run.py runs: every app of the default
# collection as a thin app, the shared runtime library they link, its engines
# and the Rust standard library, and the product font catalog beside the
# executables. OUT/bin holds the apps and fonts/, OUT/lib the libraries.
#
# Usage: tools/screenshots/build.sh OUT
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
out=$(realpath -m "${1:?usage: tools/screenshots/build.sh OUT}")
cd "$repo_root"
target=$(realpath -m "${CARGO_TARGET_DIR:-target}")
export TILO_TEST_ASSET_CACHE=${TILO_TEST_ASSET_CACHE:-$target/test-assets}

plan=$(python3 tools/apps/app_release.py --collection default)
INCLUDE_PACKAGES=$(jq -r '.packages | join(" ")' <<<"$plan") \
    BUILD_PACKAGES=$(jq -r '.packages | join(" ")' <<<"$plan") \
    APP_FEATURES=$(jq -r '.features | join(",")' <<<"$plan") \
    TARGET_DIR=$target \
    os/scripts/build-thin-apps.sh

rm -rf "$out"
mkdir -p "$out/bin" "$out/lib"
while IFS= read -r binary; do
    cp -p "$target/thin-app/$binary" "$out/bin/"
done < <(jq -r '.catalog[].binary' <<<"$plan")
cp -p "$target/thin-app/libruntime_dylib.so" "$out/lib/"
while IFS= read -r engine; do
    cp -p "$target/thin-app/$engine" "$out/lib/"
done < "$target/thin-app/runtime-engines"
needed_std=$(readelf -dW "$out/lib/libruntime_dylib.so" \
    | sed -n 's/.*Shared library: \[\(libstd-[^]]*\)\].*/\1/p')
[[ -n $needed_std ]] || { echo "libruntime_dylib.so links no libstd" >&2; exit 1; }
cp -p "$(rustc --print target-libdir)/$needed_std" "$out/lib/"
python3 tools/font-catalog.py prepare crates/sdk/ui-core/assets/fonts "$out/bin/fonts" --scope product \
    --cache "$target/font-artifacts"
printf 'staged the screenshot apps in %s\n' "$out"
