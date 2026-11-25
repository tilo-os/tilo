#!/usr/bin/env bash
# Package an app through the image's own pack path, from the module the
# app-models check built, so the tests that apply an installed app's action do
# so against the package an image installs rather than one they staged.
#
#   tools/ci/pack-model-tapp.sh OUT_DIR APP
#
# Writes OUT_DIR/<app id>.tapp: os/scripts/pack-apps.sh packs and signs it (the
# same script and the same tools an image build calls) over a repository of the
# real apps/APP manifest and surface, with a throwaway publisher key and a
# throwaway runtime bundle the package pins. The module is not rebuilt here: it
# is the one TILO_APP_MODELS names (the app-models check's output), and packing
# fails rather than shipping a package without it.
#
#   t test test-packaged-model
set -euo pipefail
cd "$(dirname "$0")/../.."

out=${1:?usage: pack-model-tapp.sh OUT_DIR APP}
app=${2:?usage: pack-model-tapp.sh OUT_DIR APP}
id=$(sed -n 's/^id[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "apps/$app/tilo-app.toml" | head -1)
[ -n "$id" ] || { printf 'apps/%s/tilo-app.toml names no id\n' "$app" >&2; exit 1; }
mkdir -p "$out"
out=$(realpath "$out")
cargo=${CARGO:-cargo}
target=${CARGO_TARGET_DIR:-$PWD/target}
models=${TILO_APP_MODELS:-$target/app-models}
[ -f "$models/$app/model.wasm" ] \
    || { printf 'no module at %s; build it with `t outputs app-models`\n' "$models/$app/model.wasm" >&2; exit 1; }

# The packaging tools, as an image build has them.
"$cargo" build --locked -p tilo-app-package-cli -p install -p tilo-runtime-bundle -p cli-shim
bin=$target/debug

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The runtime build the package pins, signed with a key made for this packing:
# the installer refuses a package whose runtime is not there, and the image's
# own check reads the same bundles. Its version, `<release>+native.<digest>`,
# is the one its payload gives it.
runtimes=$work/runtimes
identity=$work/runtime-publisher.json
"$bin/tilo-runtime-bundle" key generate --out "$identity"
"$bin/tilo-runtime-bundle" build --release 2026.07 --arch "$(uname -m)" --from / \
    --include-bin "$bin/tilo-cli-shim" --identity "$identity" \
    --out-root "$runtimes" --version-out "$work/runtime-version"
release=$(<"$work/runtime-version")

# The repository the pack reads: only Clocks, as a release selects a subset.
repo=$work/repo
mkdir -p "$repo/apps/$app" "$repo/apps/$app-model-wasm" "$repo/bin"
cp "apps/$app/tilo-app.toml" "apps/$app/surface.json" "$repo/apps/$app/"
# The app declares its model by keeping a guest crate beside it; the module
# itself comes from the app-models output, so the crate needs no content here.
printf '[package]\nname = "%s-model-wasm"\nversion = "0.1.0"\n' "$app" \
    > "$repo/apps/$app-model-wasm/Cargo.toml"
cp "$bin/tilo-cli-shim" "$repo/bin/$app"

# The fixture's binary is not a release build; its bytes are not what the size
# budgets measure, so the pack reports the package's size instead of refusing it.
REPO="$repo" BIN_DIR="$repo/bin" APP_MODELS="$models" \
    RUNTIME_ID="tilo-runtime/$release" \
    STORE="$work/store" STAGE="$work/stage" \
    PKG_BIN="$bin/tilo-app-package" INSTALL_BIN="$bin/tilo-install" \
    BUNDLE_BIN="$bin/tilo-runtime-bundle" SHIM_BIN="$bin/tilo-cli-shim" \
    RUNTIME_ROOT="$runtimes" TILO_RUNTIME_BUNDLE_DIR="$runtimes" \
    TAPP_OUT="$out/$id.tapp" COHORT_APPS=$app REQUIRE_MIN=1 \
    TAPP_BUDGET=report \
    bash os/scripts/pack-apps.sh

"$bin/tilo-app-package" inspect "$out/$id.tapp" > "$out/inspect.txt"
grep -Eq '^  model\.wasm  \(' "$out/inspect.txt" \
    || { printf 'the packed package carries no model.wasm:\n' >&2; cat "$out/inspect.txt" >&2; exit 1; }
# The store the pack installed into holds what a device reads the actions from.
find "$work/store/pkgs" -name model.wasm -print -quit | grep -q . \
    || { printf 'the installed package carries no model.wasm\n' >&2; exit 1; }
printf 'packed %s with its model from %s\n' "$out/$id.tapp" "$models/$app/model.wasm"
