#!/usr/bin/env bash
# Build what tools/screenshots/run.py runs: every app of the default
# collection as a thin app, the runtime they and the tilolang packages run on
# (the shared runtime library, its engines, the Rust standard library and the
# product font catalog), and the programs a capture runs through. OUT/bin holds
# the daemon, the launcher, the installer, the tilolang tool, the runtime bundle
# tool and fonts/, OUT/lib the runtime's libraries, OUT/runtime the bundles a
# profile mounts (the apps' runtime, the tilolang runtime and Browser's Chromium
# engine), and OUT/install one development build per native app, the form a
# daemon installs.
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
    BUILD_PACKAGES="$(jq -r '.packages | join(" ")' <<<"$plan") tilolang-process" \
    APP_FEATURES="$samples" \
    TARGET_DIR=$target \
    os/scripts/build-thin-apps.sh

rm -rf "$out"
mkdir -p "$out/bin" "$out/lib"
cp -p "$target/thin-app/libruntime_dylib.so" "$out/lib/"
while IFS= read -r engine; do
    cp -p "$target/thin-app/$engine" "$out/lib/"
done < "$target/thin-app/runtime-engines"
needed_std=$(readelf -dW "$out/lib/libruntime_dylib.so" \
    | sed -n 's/.*Shared library: \[\(libstd-[^]]*\)\].*/\1/p')
[[ -n $needed_std ]] || { echo "libruntime_dylib.so links no libstd" >&2; exit 1; }
cp -p "$(rustc --print target-libdir)/$needed_std" "$out/lib/"
# The product font catalog every app's text is set in: the runtime bundle carries
# it as `lib/fonts`, as a device's runtime does.
python3 tools/font-catalog.py prepare crates/ui/ui-core/assets/fonts "$out/bin/fonts" --scope product \
    --cache "$target/font-artifacts"

# The programs a capture runs through, in a development build so the launcher and
# the runtime read the paths this build names (the device's overrides): the one
# daemon every app is started by, the launcher that starts it, the installer that
# stages a build into a disposable profile, and the tool that bakes a runtime.
"${CARGO:-cargo}" build --locked -p tilolang-cli --bin tilolang \
    -p tilo-daemon --bin tilo-runtime -p launcher --bin tilo-launcher \
    -p install --bin tilo-install -p tilo-runtime-bundle --bin tilo-runtime-bundle \
    -p tilolang --bin tilolang-std
for program in tilolang tilolang-std tilo-runtime tilo-launcher tilo-install tilo-runtime-bundle; do
    cp -p "$target/debug/$program" "$out/bin/"
done

# The publisher the runtime bundles are signed by, and the list a launcher trusts:
# a disposable profile has no device certificate, so it is told whose runtime to
# mount and whose packages to install.
identity=$out/publisher.json
[[ -f $identity ]] || "$out/bin/tilo-runtime-bundle" key generate --out "$identity" >/dev/null
pubkey=$("$out/bin/tilo-runtime-bundle" key pubkey --identity "$identity")
[[ $pubkey =~ ^[0-9a-fA-F]{64}$ ]] || { echo "cannot read the build's publisher key" >&2; exit 1; }
printf '%s\n' "$pubkey" > "$out/runtime-publishers.txt"
runtimes=$out/runtime
mkdir -p "$runtimes"

# The runtime bundle the thin apps and every tilolang app run on: the toolkit's
# library, libstd, the engines and the host's loader set, signed by the build.
release=$(jq -er '[.catalog[].runtime] | unique | if length == 1 then .[0] else error("apps require different runtimes") end | sub("^tilo-runtime/"; "")' <<<"$plan")
keyboard_library=$(ldconfig -p | awk '/libxkbcommon\.so\.0 .*(x86-64|AArch64)/ && !found {print $NF; found=1}')
[[ -f $keyboard_library ]] || { echo "libxkbcommon.so.0 is not installed" >&2; exit 1; }
runtime_libs=("$out/lib/libruntime_dylib.so" "$out/lib/$needed_std" "$keyboard_library")
while IFS= read -r engine; do runtime_libs+=("$out/lib/$engine"); done < "$target/thin-app/runtime-engines"
mkdir -p "$out/work"
OUT_DIR=$out/work/runtime RUNTIME_RELEASE=$release FROM=/ \
    VERSION_OUT=$out/work/runtime-version ARCH=$(uname -m) \
    IDENTITY_JSON=$identity BUNDLE_BIN=$out/bin/tilo-runtime-bundle \
    FONT_CATALOG_ROOT=$out/bin/fonts EXTRA_LIBS="${runtime_libs[*]}" \
    bash "$repo_root/os/scripts/bake-runtime-bundle.sh"
runtime_version=$(<"$out/work/runtime-version")
mv "$out/work/runtime/tilo-runtime/$runtime_version" "$runtimes/$runtime_version"
rm -rf "$out/work"
runtime_id=tilo-runtime/$runtime_version

# The tilolang runtime bundle the launcher executes every tilolang package's
# entrypoint from, baked from the thin binary against the runtime above.
# shellcheck source=/dev/null
. "$repo_root/tools/apps/tilolang_packages.sh"
tilolang_version=$(tilolang_runtime_version "$repo_root")
tilolang_bake_runtime "$out/bin/tilo-runtime-bundle" "$target/thin-app/tilolang-process" \
    "$out/bin/tilolang-std" "$repo_root" "$identity" "$runtimes/$tilolang_version" \
    "$tilolang_version"

# Browser's Chromium engine, as its own signed runtime bundle: the browser's manifest requires it,
# and a device installs the app and its runtimes in one step. An image installs the bundle; a
# session signs it from the engine build it is given (`TILO_CHROMIUM_ENGINE`, the `chromium-engine`
# check's output) or, for a developer's run of this build, builds it here.
engine_tree=${TILO_CHROMIUM_ENGINE:+$TILO_CHROMIUM_ENGINE/chromium}
if [[ -z $engine_tree ]]; then
    python3 scripts/browser/build-host.py --work "$target" \
        --runtime "$target/screenshots-chromium" --jobs "${CARGO_BUILD_JOBS:-$(nproc)}"
    engine_tree=$target/screenshots-chromium/chromium
fi
[[ -x $engine_tree/tilo-chromium-host ]] \
    || { echo "no Chromium engine in $engine_tree: set TILO_CHROMIUM_ENGINE to the tree it is in" >&2; exit 1; }
engine_version=$(sed -n 's/^pub const RUNTIME_VERSION: &str = "\(.*\)";$/\1/p' \
    "$repo_root/crates/engines/tilo-webview/src/chromium/runtime.rs")
[[ -n $engine_version ]] || { echo "the Chromium runtime names no RUNTIME_VERSION" >&2; exit 1; }
"$out/bin/tilo-runtime-bundle" build --version "$engine_version" --arch "$(uname -m)" \
    --from / --include "$engine_tree" --identity "$identity" --out "$runtimes/$engine_version"

# Each app of the collection as the development build a disposable profile
# installs: the manifest with the runtime this build baked, and its payload files,
# the form the device signs and starts.
mkdir -p "$out/install"
while IFS=$'\t' read -r name manifest binary; do
    dir=$out/install/$name
    rm -rf "$dir"
    mkdir -p "$dir/bin"
    sed "s|^runtime *= *\"tilo-runtime/[^\"]*\"|runtime = \"$runtime_id\"|" \
        "$repo_root/$manifest" > "$dir/tilo-app.toml"
    grep -q "^runtime = \"$runtime_id\"" "$dir/tilo-app.toml" \
        || { echo "$manifest names no runtime to point at $runtime_id" >&2; exit 1; }
    cp -p "$target/thin-app/$binary" "$dir/bin/$binary"
    # The app's public surface ships at the payload root, as the image's own
    # packing places it: the window bar, search and slots the app declares.
    if [[ -f $(dirname "$repo_root/$manifest")/surface.toml ]]; then
        cp -p "$(dirname "$repo_root/$manifest")/surface.toml" "$dir/surface.toml"
    fi
done < <(jq -r '.catalog[] | select(.kind == "native") | [.name, .manifest, .binary] | @tsv' <<<"$plan")
# A tilolang app's development build is compiled for the id the device it installs on names
# (`<app>.dev.<device key>`), which only that device's daemon answers: the runner builds it per
# profile, beside the daemon it installs on. What it needs of this build is where the sources are
# and the runtime the package names.
: > "$out/apps.tsv"
while IFS=$'\t' read -r name dir id; do
    printf '%s\t%s\t%s\n' "$name" "$repo_root/$dir" "$runtime_id" >> "$out/apps.tsv"
done < <(jq -r '.catalog[] | select(.kind == "tilolang") | [.name, .dir, .id] | @tsv' <<<"$plan")
printf 'staged the screenshot apps in %s\n' "$out"
