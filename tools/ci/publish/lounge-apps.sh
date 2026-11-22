#!/usr/bin/env bash
# The apps a Lounge runs for its workspace's people: the launcher that starts each
# app's process in its sandbox, the runtime bundles the apps execute, and the
# signed tilolang packages the workspace installs at its start
# (TILO_WORKSPACES_PACKAGES). usage: lounge-apps.sh OUT THIN FONTS [PROFILE]
#
# THIN is the thin tilolang runtime build (dist/dev/tilolang-runtime-build.sh:
# the tilolang-app binary, the shared runtime library it links, its engines and
# libstd), FONTS the product font catalog the Tilo runtime bundle carries.
#
# OUT holds:
#   bin/tilo-launcher                 found beside tilo-server
#   runtimes/<version>/               the Tilo and tilolang runtime bundles
#   runtime-publishers.txt            the one key both bundles and every package are signed with
#   packages/<app>.tapp               what the workspace installs
#
# The key is made for this build and kept nowhere else: whoever runs these
# trusts it by the list beside them. `lounge-check` (the checks that run the
# Lounge's programs directly) builds the launcher with `dev-overrides`, so it
# reads that list from TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE; the image's
# `release` launcher never honours that variable and reads the image's own
# /etc/tilo/runtime-bundle-trusted-publishers.txt. Neither accepts a bundle or a
# package signed by any other key.
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
  release) launcher_features=() ;;
  lounge-check) launcher_features=(--features dev-overrides) ;;
  *) die "unknown profile $profile: release or lounge-check" ;;
esac
[[ -f $thin/tilolang-app && -f $thin/libruntime_dylib.so && -f $thin/runtime-engines ]] \
  || die "no thin tilolang runtime build in $thin"
[[ -f $fonts/manifest.toml ]] || die "no product font catalog in $fonts"
# The apps the Lounge's people open in their first hour, as the release plans them.
apps='tl-notes tl-mail tl-tube tl-tasks'
plan=$(python3 tools/apps/app_release.py --apps "$apps")
release=$(jq -er '[.catalog[].runtime] | unique | if length == 1 then .[0] else error("the apps name different runtimes") end | sub("^tilo-runtime/"; "")' <<<"$plan")

# The launcher on a build of its own: its `dev-overrides` must not reach the server.
cargo build --locked --profile "$profile" -p launcher --bin tilo-launcher "${launcher_features[@]}"
cargo build --locked --profile "$profile" -p tilolang-process --bin tilolang-process \
  -p tilolang-cli --bin tilolang -p runtime-bundle --bin tilo-runtime-bundle
built=$target/$profile

rm -rf "$out"
mkdir -p "$out/bin" "$out/runtimes" "$out/packages" "$out/work"
cp "$built/tilo-launcher" "$out/bin/"
identity=$out/work/publisher.json
"$built/tilo-runtime-bundle" key generate --out "$identity" >/dev/null
"$built/tilo-runtime-bundle" key pubkey --identity "$identity" > "$out/runtime-publishers.txt"

# The Tilo runtime: the loader and libraries the apps run on, the shared
# runtime library, its engines and libstd, and the fonts they lay text out in.
libs=("$thin/libruntime_dylib.so" "$thin"/libstd-*.so)
while IFS= read -r engine; do libs+=("$thin/$engine"); done < "$thin/runtime-engines"
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

# The tilolang runtime every package's entrypoint executes, and the packages.
. tools/apps/tilolang_packages.sh
tilolang=$(tilolang_runtime_version "$repo_root")
tilolang_bake_runtime "$built/tilo-runtime-bundle" "$thin/tilolang-app" "$built/tilolang-process" \
  "$identity" "$out/runtimes/$tilolang" "$tilolang"
while IFS=$'\t' read -r name dir id version; do
  tilolang_build_package "$built/tilolang" "$repo_root" "$dir" "$name" "$out/packages" \
    "$identity" "$version" "tilo-runtime/$runtime" "$id"
done < <(jq -r --arg apps "$apps" '.tilolang[] | select(.app | IN($apps | split(" ")[])) | [.app, .dir, .id, .version] | @tsv' <<<"$plan")
rm -rf "$out/work"
echo "apps: runtimes $runtime and tilolang/$tilolang, packages $(cd "$out/packages" && echo *.tapp)"
