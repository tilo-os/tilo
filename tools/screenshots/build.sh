#!/usr/bin/env bash
# Build what tools/screenshots/run.py runs: every app of the default
# collection as a thin app, the tilolang packages beside them (each compiled
# into the payload the one tilolang runtime binary reads), the shared runtime
# library they link, its engines and the Rust standard library, and the
# product font catalog beside the executables. OUT/bin holds the apps, the
# modules and fonts/, OUT/lib the libraries.
#
# Usage: tools/screenshots/build.sh OUT
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
out=$(realpath -m "${1:?usage: tools/screenshots/build.sh OUT}")
cd "$repo_root"
target=$(realpath -m "${CARGO_TARGET_DIR:-target}")
export TILO_TEST_ASSET_CACHE=${TILO_TEST_ASSET_CACHE:-$target/test-assets}

plan=$(python3 tools/apps/app_release.py --collection default)
# A screenshot plan may name one of the app's own scenarios, through actions
# over sample data the app keeps behind its dev-only `samples` feature (never
# in a release): a capture build turns it on for the apps whose plan names one.
samples=$(
    jq -r '.catalog[] | select(.kind == "native") | [.name, .package] | @tsv' <<<"$plan" |
    while IFS=$'\t' read -r name package; do
        if grep -q '^scenario *=' "tools/screenshots/$name.toml" 2>/dev/null &&
            grep -q '^samples *=' "apps/$name/Cargo.toml" 2>/dev/null; then
            printf '%s/samples,' "$package"
        fi
    done
)
INCLUDE_PACKAGES=$(jq -r '.packages | join(" ")' <<<"$plan") \
    BUILD_PACKAGES="$(jq -r '.packages | join(" ")' <<<"$plan") tilolang-app" \
    APP_FEATURES="$(jq -r '.features | join(",")' <<<"$plan")${samples:+,$samples}" \
    TARGET_DIR=$target \
    os/scripts/build-thin-apps.sh

rm -rf "$out"
mkdir -p "$out/bin" "$out/lib"
while IFS= read -r binary; do
    cp -p "$target/thin-app/$binary" "$out/bin/"
done < <(jq -r '.catalog[].binary | select(length > 0)' <<<"$plan")
cp -p "$target/thin-app/tilolang-app" "$out/bin/"
cp -p "$target/thin-app/libruntime_dylib.so" "$out/lib/"
while IFS= read -r engine; do
    cp -p "$target/thin-app/$engine" "$out/lib/"
done < "$target/thin-app/runtime-engines"
needed_std=$(readelf -dW "$out/lib/libruntime_dylib.so" \
    | sed -n 's/.*Shared library: \[\(libstd-[^]]*\)\].*/\1/p')
[[ -n $needed_std ]] || { echo "libruntime_dylib.so links no libstd" >&2; exit 1; }
cp -p "$(rustc --print target-libdir)/$needed_std" "$out/lib/"
# Each tilolang package of the collection, compiled into the payload the runtime
# runs it from: the plan drives it through the same headless spec as a native app.
"${CARGO:-cargo}" build --locked -p tilolang-cli --bin tilolang
mkdir -p "$out/bin/modules"
while IFS=$'\t' read -r name dir id; do
    "$target/debug/tilolang" build "$repo_root/$dir" --out "$out/tilolang" \
        --identity "$out/tilolang-publisher.json"
    cp -a "$out/tilolang/${id##*.}/payload" "$out/bin/modules/$name"
done < <(jq -r '.catalog[] | select(.kind == "tilolang") | [.name, .dir, .id] | @tsv' <<<"$plan")
python3 tools/font-catalog.py prepare crates/ui/ui-core/assets/fonts "$out/bin/fonts" --scope product \
    --cache "$target/font-artifacts"
printf 'staged the screenshot apps in %s\n' "$out"
