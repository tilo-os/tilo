#!/usr/bin/env bash
# The apps a Lounge runs for its workspace's people, as a device carries them:
# the launcher each app's process starts in, the runtimes the apps execute,
# the key all of them are signed with, and the default collection's tilolang
# packages. usage: lounge-apps.sh OUT THIN FONTS [PROFILE]
#
# THIN is the thin tilolang runtime build (dist/dev/tilolang-runtime-build.sh:
# the tilolang process, the shared runtime library it links, its engines and
# libstd), FONTS the product font catalog the Tilo runtime bundle carries.
#
# OUT holds:
#   bin/tilo-launcher                 what starts each app, beside the server
#   bin/tilolang-process              a task's programs, beside the server
#   bin/tilo-install                  installs the packages into a store
#   runtimes/<version>/               the Tilo and tilolang runtime bundles
#   runtime-publishers.txt            the one key the bundles and every package are signed with
#   packages/<app>.tapp               the default collection's tilolang apps
#
# The key is made for this build and kept nowhere else: a device trusts it by
# listing it beside the bundles, the image at /etc/tilo, a check through
# TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE (which only the `lounge-check`
# launcher honours: it is built with `dev-overrides`, and no program of the
# image's `release` build ever is). Neither accepts a bundle or a package
# signed by any other key.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
usage='usage: lounge-apps.sh OUT THIN FONTS [PROFILE]'
out=${1:?$usage}
thin=$(realpath "${2:?$usage}")
fonts=$(realpath "${3:?$usage}")
profile=${4:-release}
cd "$repo_root"
target=${CARGO_TARGET_DIR:-$repo_root/target}
case $profile in
  release)
    # As the image's programs are linked (lounge-binaries.sh): thin LTO keeps
    # the build inside the memory a check's machine has.
    export CARGO_PROFILE_RELEASE_LTO=thin CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
    launcher_features=() ;;
  lounge-check) launcher_features=(--features dev-overrides) ;;
  *) die "unknown profile $profile: release or lounge-check" ;;
esac
[[ -f $thin/tilolang-process && -f $thin/libruntime_dylib.so && -f $thin/runtime-engines ]] \
  || die "no thin tilolang runtime build in $thin"
[[ -f $fonts/manifest.toml ]] || die "no product font catalog in $fonts"
# The apps the default collection plans, all of them tilolang: a Lounge runs
# what a device's image ships, and every one of them is installed the way a
# device installs a package.
plan=$out.plan.json
python3 tools/apps/app_release.py --collection default > "$plan"
release=$(jq -er '[.catalog[].runtime] | unique | if length == 1 then .[0] else error("the apps name different runtimes") end | sub("^tilo-runtime/"; "")' < "$plan")

# The launcher on a build of its own: its `dev-overrides` must not reach the
# server. The tilolang command and the standard library compiler are the
# release's own build (dist/dev/nested-session-build.sh builds them the same
# way), and the tilolang process beside the server is not a thin one.
cargo build --locked --profile "$profile" -p launcher --bin tilo-launcher "${launcher_features[@]}"
cargo build --locked --profile "$profile" -p tilo-install --bin tilo-install \
  -p runtime-bundle --bin tilo-runtime-bundle -p tilolang-process --bin tilolang-process
cargo build --locked --profile "$profile" -p tilolang-cli --bin tilolang \
  -p tilolang --bin tilolang-std
built=$target/$profile

rm -rf "$out"
mkdir -p "$out/bin" "$out/runtimes" "$out/packages" "$out/work"
cp "$built/tilo-launcher" "$built/tilolang-process" "$built/tilo-install" "$out/bin/"
identity=$out/work/publisher.json
"$built/tilo-runtime-bundle" key generate --out "$identity" >/dev/null
"$built/tilo-runtime-bundle" key pubkey --identity "$identity" > "$out/runtime-publishers.txt"

# The Tilo runtime: the loader and libraries the apps run on, the shared
# runtime library, its engines and libstd, and the fonts they lay text out in.
libs=("$thin/libruntime_dylib.so")
while IFS= read -r engine; do libs+=("$thin/$engine"); done < "$thin/runtime-engines"
for std in "$thin"/libstd-*.so; do libs+=("$std"); done
[[ -f ${libs[1]} && -f ${libs[$((${#libs[@]} - 1))]} ]] || die "the thin runtime build carries no engines or libstd"
keyboard=$(ldconfig -p | awk '/libxkbcommon\.so\.0 .*(x86-64|AArch64)/ && !found {print $NF; found=1}')
[[ -f $keyboard ]] || die "libxkbcommon.so.0 is not installed"
libs+=("$keyboard")
OUT_DIR="$out/work/runtime" RUNTIME_RELEASE="$release" FROM=/ \
  VERSION_OUT="$out/work/runtime-version" ARCH="$(uname -m)" \
  IDENTITY_JSON="$identity" BUNDLE_BIN="$built/tilo-runtime-bundle" \
  FONT_CATALOG_ROOT="$fonts" EXTRA_LIBS="${libs[*]}" \
  os/scripts/bake-runtime-bundle.sh
runtime=$(<"$out/work/runtime-version")
mv "$out/work/runtime/tilo-runtime/$runtime" "$out/runtimes/$runtime"

# The tilolang runtime every package's entrypoint executes, signed by the same
# key and running on the Tilo runtime above.
# shellcheck source=/dev/null
. tools/apps/tilolang_packages.sh
tilolang=$(tilolang_runtime_version "$repo_root")
tilolang_bake_runtime "$built/tilo-runtime-bundle" "$thin/tilolang-process" "$built/tilolang-std" \
  "$repo_root" "$identity" "$out/work/tilolang" "$tilolang"
mv "$out/work/tilolang" "$out/runtimes/$tilolang"

while IFS=$'\t' read -r name dir id version; do
  tilolang_build_package "$built/tilolang" "$repo_root" "$dir" "$name" "$out/packages" \
    "$identity" "$version" "tilo-runtime/$runtime" "$id"
done < <(jq -r '.tilolang[] | [.app, .dir, .id, .version] | @tsv' < "$plan")
rm -rf "$out/work" "$plan"
echo "apps: runtimes $runtime and tilolang/$tilolang, $(cd "$out/packages" && echo ./*.tapp | wc -w) packages"
