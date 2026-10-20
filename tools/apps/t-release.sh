#!/usr/bin/env bash
# Release / apps as t's checks run it (t.toml, "Apps"; docs/publishing-on-t.md). Each step is its
# own check, holding only what it needs:
#
#   build ARCH OUT       no credentials: compile every app of the default collection and the
#                        release tools for this machine's architecture (ARCH, x86_64 or
#                        aarch64), package them with throwaway keys, run the release smoke
#                        tests and the reproducibility check, and keep what signing needs in
#                        OUT/compiled
#   sizes ARCH           no credentials: the whole collection's size gate. Compile every app of
#                        the default collection as `build` does, package them with throwaway keys,
#                        and fail when a tapp or the runtime is over its budget
#                        (tools/perf/tapp-size-budget/, tools/perf/runtime-size-budget/). It runs
#                        on each new head of stable and when dispatched, not on every commit.
#   thin-runtime ARCH    no credentials: what every app-size and runtime-size check shares, built
#                        once. Compile the runtime and its engines in the default collection's
#                        graph (as `sizes` does) and the packing tools, in a build directory of its
#                        own ($CARGO_TARGET_DIR/thin-runtime), which keeps only this build's units
#                        and lists them; t hands that directory to the checks that need this one.
#   app-size ARCH APP    no credentials: one app's size, as `sizes` measures it. In the build
#                        directory `thin-runtime` left, compile APP (an app directory) against the
#                        shared runtime, package it with throwaway keys and fail when its tapp is
#                        over its budget. The tapp names the runtime it links, so the runtime
#                        bundle is baked and signed too.
#   runtime-size ARCH APP...
#                        no credentials: the shared runtime's size, as `sizes` measures it. In the
#                        build directory `thin-runtime` left, compile APPs (the apps the runtime
#                        budget lists as playing no media, whose loaded libraries it also checks),
#                        and fail when the runtime is over its budget. Nothing is packaged.
#   binaries ARCH OUT    no credentials: compile the default collection as `build` does but keep
#                        the symbol tables, and leave the apps, the runtime and its engines in
#                        OUT, for reading what makes an app large (`t outputs app-binaries-ARCH`;
#                        docs/measurements/app-package-size.md)
#   sign ARCH BUILT OUT  TAPP_PUBLISHER_IDENTITY and RUNTIME_BUNDLE_SIGNING_KEY: package and sign
#                        BUILT/compiled without compiling anything, into OUT
#   publish OUT SIGNED.. REGISTRY_TOKEN: upload the signed tapps, collections, registry media and
#                        runtime objects, and write the collection channel to OUT/channel.json
#   submit SIGNED..      TAPP_PUBLISHER_IDENTITY: sign each app's registry entry and submit it
#
# `sign`, `publish` and `submit` run only in trusted runs of stable or a release tag (T_REF): the
# apps are released from stable, and main only builds them.
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
source "$repo_root/tools/ci/publish/lib.sh"
source "$repo_root/tools/apps/release-lib.sh"
cd "$repo_root"

# Public settings the release has always used (formerly repository variables).
package_base=https://code.tilo.so/api/packages/tilo/generic
registry_url=https://apps.tilo.so
registry_owner_pubkey=55d9a9c748bcfe34200832c18c5f0cf7932f17571612321eaa542474fa2abe06

default_apps() {
  python3 -I tools/ci/python.py tools/apps/app_release.py --collection default | jq -r '.apps | join(" ")'
}

# The environment of the size checks (sizes, app-size, runtime-size): APPS (app directories)
# compiled for ARCH in the default collection's graph, as a release compiles them, into
# $scratch/NAME.
size_build() {
  local arch=$1 apps=$2 name=$3 target
  os/dev/assert-native-arch.sh "$arch"
  target=$(realpath -m "${CARGO_TARGET_DIR:-target}")
  export TILO_TEST_ASSET_CACHE=${TILO_TEST_ASSET_CACHE:-$target/test-assets}
  export CARGO_INCREMENTAL=0
  export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$(cargo_jobs)}
  # The thin graph compiles as a release's does (build-thin-apps.sh drops the release
  # profile's overrides); the packing tools need no optimisation to pack the same bytes.
  export TILO_RELEASE_APPS=$apps TILO_APP_RELEASE_OUT=$scratch/$name TILO_APP_TOOLS_PROFILE=dev
}

# The build directory `thin-runtime` builds the shared runtime, its engines and the packing tools
# in, and the size checks that need it receive: the same path in both, as cargo's units record it.
shared_target() {
  printf '%s/thin-runtime' "$(realpath -m "${CARGO_TARGET_DIR:-target}")"
}

# The environment of a size check that builds on `thin-runtime`'s outputs (app-size,
# runtime-size): size_build's, in its build directory. t restores those outputs with the time it
# writes each, file by file, so cargo would see a library as newer than the units built on it and
# rebuild them: the files `thin-runtime` listed get one time, now (thin_runtime_units.py stamp).
shared_size_build() {
  local arch=$1 apps=$2 name=$3 shared
  shared=$(shared_target)
  size_build "$arch" "$apps" "$name"
  export CARGO_TARGET_DIR=$shared TILO_TEST_ASSET_CACHE=$shared/test-assets
  [[ -f $shared/shared-files ]] \
    || die "$shared/shared-files is missing: this check needs thin-runtime-$arch, whose outputs t puts there"
  python3 -I tools/apps/thin_runtime_units.py stamp "$shared" --list "$shared/shared-files"
}

# The publication environment publish-collections.sh reads, for this trusted run.
publication_env() {
  local signed
  rm -rf release
  mkdir release
  for signed in "$@"; do
    [[ -f $signed/index.json ]] || die "$signed holds no signed collection"
    cp -r "$signed" "release/$(jq -er .architecture "$signed/index.json")"
  done
  TILO_SOURCE_EPOCH=$(commit_epoch)
  export TILO_SOURCE_EPOCH VERSION="0.0.0-$T_COMMIT" COLLECTION=default COMPLETE=true \
    BUILDER_IMAGE=$(sed -n 's/^image = "\(.*\)"$/\1/p' t.toml | head -1) PRIOR_X86_URL= PRIOR_X86_SHA256= PRIOR_ARM64_URL= PRIOR_ARM64_SHA256= \
    TILO_PACKAGE_PUBLIC_BASE_URL=$package_base TILO_REGISTRY_URL=$registry_url \
    TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY=$registry_owner_pubkey \
    GITHUB_SERVER_URL=https://code.tilo.so GITHUB_REPOSITORY=tilo/tilo GITHUB_SHA=$T_COMMIT \
    RUNNER_TEMP=$scratch
  case $T_REF in
    tag:*) export GITHUB_REF=refs/tags/${T_REF#tag:} ;;
    *) export GITHUB_REF=refs/heads/$T_REF ;;
  esac
}

command=${1:-}
shift || true
case $command in
  build)
    arch=${1:?usage: t-release.sh build ARCH OUT} out=${2:?}
    os/dev/assert-native-arch.sh "$arch"
    target=$(realpath -m "${CARGO_TARGET_DIR:-target}")
    apps=$(default_apps)
    # Build scripts fetch their test assets into the build directory; the sources are read-only.
    export TILO_TEST_ASSET_CACHE=${TILO_TEST_ASSET_CACHE:-$target/test-assets}
    export CARGO_INCREMENTAL=0 CARGO_PROFILE_RELEASE_LTO=thin CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
    export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$(cargo_jobs)}
    export TILO_RELEASE_APPS=$apps TILO_APP_RELEASE_OUT=$scratch/app-release
    TILO_APP_COMPILE_ONLY=1 dist/apps/package-apps.sh
    # What `sign` packages: the release tools, every app's binary, the runtime and its engines.
    rm -rf "$out/compiled"
    mkdir -p "$out/compiled/release" "$out/compiled/thin-app"
    cp -p "$target/release/tilo-app-package" "$target/release/tilo-runtime-bundle" "$out/compiled/release/"
    cp -p "$target/thin-app/libruntime_dylib.so" "$target/thin-app/runtime-engines" "$out/compiled/thin-app/"
    while IFS= read -r file; do
      cp -p "$target/thin-app/$file" "$out/compiled/thin-app/"
    done < <(cat "$target/thin-app/runtime-engines"
             python3 -I tools/ci/python.py tools/apps/app_release.py --apps "$apps" | jq -r '.catalog[].binary')
    # Packaged once with throwaway keys, exactly as `sign` will, then checked.
    dist/apps/package-apps.sh
    tools/apps/release-smoke.sh "$arch"
    # Two builds of one source must give the same bytes, or every OS image and update re-sends
    # apps that did not change.
    tools/apps/check-reproducible-tapp.sh calculator
    ;;
  sizes)
    arch=${1:?usage: t-release.sh sizes ARCH}
    apps=$(default_apps)
    size_build "$arch" "$apps" app-sizes
    dist/apps/package-apps.sh
    RELEASE_SMOKE_STEPS="tapp_size runtime_size" tools/apps/release-smoke.sh "$arch"
    ;;
  thin-runtime)
    arch=${1:?usage: t-release.sh thin-runtime ARCH}
    (( $# == 1 )) || die "usage: t-release.sh thin-runtime ARCH"
    size_build "$arch" "" thin-runtime
    shared=$(shared_target)
    export CARGO_TARGET_DIR=$shared TILO_TEST_ASSET_CACHE=$shared/test-assets
    rm -f "$shared/shared-files"
    python3 -I tools/apps/thin_runtime_units.py mark "$shared/thin-app"
    TILO_APP_COMPILE_ONLY=1 TILO_APP_RUNTIME_ONLY=1 dist/apps/package-apps.sh
    # What the size checks find in the profile directory besides the units: the runtime, its
    # engines, their list and the programs build-thin-apps.sh checks them with.
    top=(libruntime_dylib.so libruntime_dylib.d runtime-engines tilo-runtime-account-capability
         tilo-runtime-account-capability.d)
    while IFS= read -r engine; do
      top+=("$engine" "${engine%.so}.d")
    done < "$shared/thin-app/runtime-engines"
    for manifest in crates/runtime/engines/*/Cargo.toml; do
      engine=$(sed -n 's/^name *= *"\([^"]*\)".*/\1/p' "$manifest" | head -1)
      top+=("$engine-check" "$engine-check.d")
    done
    python3 -I tools/apps/thin_runtime_units.py keep "$shared/thin-app" "${top[@]/#/--top=}" \
      --list "$shared/shared-files"
    ;;
  app-size)
    arch=${1:?usage: t-release.sh app-size ARCH APP} app=${2:?usage: t-release.sh app-size ARCH APP}
    (( $# == 2 )) || die "usage: t-release.sh app-size ARCH APP"
    shared_size_build "$arch" "$app" app-size
    TILO_APP_TOOLS_BUILT=1 TILO_APP_SKIP_LANGUAGE_PACKS=1 dist/apps/package-apps.sh
    RELEASE_SMOKE_STEPS=tapp_size tools/apps/release-smoke.sh "$arch"
    ;;
  runtime-size)
    arch=${1:?usage: t-release.sh runtime-size ARCH APP...}
    shift
    (( $# > 0 )) || die "usage: t-release.sh runtime-size ARCH APP..."
    shared_size_build "$arch" "$*" runtime-size
    # The runtime's measurement reads only the compiled graph: no packing tools, no bundle.
    TILO_APP_COMPILE_ONLY=1 TILO_APP_SKIP_TOOLS=1 dist/apps/package-apps.sh
    RELEASE_SMOKE_STEPS=runtime_size tools/apps/release-smoke.sh "$arch"
    ;;
  binaries)
    arch=${1:?usage: t-release.sh binaries ARCH OUT} out=${2:?}
    os/dev/assert-native-arch.sh "$arch"
    target=$(realpath -m "${CARGO_TARGET_DIR:-target}")
    apps=$(default_apps)
    export TILO_TEST_ASSET_CACHE=${TILO_TEST_ASSET_CACHE:-$target/test-assets}
    export CARGO_INCREMENTAL=0
    export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$(cargo_jobs)}
    export TILO_RELEASE_APPS=$apps TILO_APP_RELEASE_OUT=$scratch/app-binaries TILO_APP_TOOLS_PROFILE=dev
    TILO_THIN_KEEP_SYMBOLS=1 TILO_APP_COMPILE_ONLY=1 TILO_APP_SKIP_TOOLS=1 dist/apps/package-apps.sh
    rm -rf "$out"
    mkdir -p "$out"
    cp -p "$target/thin-app/libruntime_dylib.so" "$target/thin-app/runtime-engines" "$out/"
    while IFS= read -r file; do
      cp -p "$target/thin-app/$file" "$out/"
    done < <(cat "$target/thin-app/runtime-engines"
             python3 -I tools/ci/python.py tools/apps/app_release.py --apps "$apps" | jq -r '.catalog[].binary')
    ;;
  sign)
    arch=${1:?usage: t-release.sh sign ARCH BUILT OUT} built=${2:?} out=$(realpath -m "${3:?}")
    release_only
    [[ $(uname -m) == "$arch" ]] || die "signing $arch on $(uname -m)"
    app_identity=$(secret_file TAPP_PUBLISHER_IDENTITY)
    runtime_identity=$(secret_file RUNTIME_BUNDLE_SIGNING_KEY)
    trap 'rm -f "$app_identity" "$runtime_identity"' EXIT
    # Registry versions only ever increase: the release sequence is the commit's time, which grows
    # along stable's history.
    sequence=$(commit_epoch)
    [[ $sequence =~ ^[0-9]+$ ]] || die "no time for $T_COMMIT"
    TILO_APP_PREBUILT=$built/compiled TILO_RELEASE_APPS=$(default_apps) TILO_APP_RELEASE_OUT=$out \
      TILO_APP_RELEASE_SOURCE=$T_COMMIT TILO_APP_RELEASE_SEQUENCE=$sequence GITHUB_SHA=$T_COMMIT \
      TAPP_PUBLISHER_IDENTITY_FILE=$app_identity RUNTIME_BUNDLE_IDENTITY_FILE=$runtime_identity \
      dist/apps/package-apps.sh
    python3 tools/apps/create_app_receipt.py --collection "$out" \
      --pkg-binary "$built/compiled/release/tilo-app-package" \
      --runtime-binary "$built/compiled/release/tilo-runtime-bundle" \
      --signers "$out/work/receipt-signers.json" --source-sha "$T_COMMIT" --architecture "$arch"
    # The publishing steps run this exact client to make registry requests from the final bytes.
    install -m 0755 "$built/compiled/release/tilo-app-package" "$out/tilo-app-package"
    rm -rf "$out/work"
    printf 'Signed %s apps for %s from %s\n' "$(jq '.apps | length' "$out/index.json")" "$arch" "$T_COMMIT"
    ;;
  publish)
    out=$(realpath -m "${1:?usage: t-release.sh publish OUT SIGNED...}")
    shift
    release_only
    token=${REGISTRY_TOKEN:?the package token is not set: the check needs secrets = [\"REGISTRY_TOKEN\"]}
    unset REGISTRY_TOKEN
    publication_env "$@"
    mkdir -p "$out"
    TILO_APP_PUBLISH_STAGE=upload TILO_PACKAGE_TOKEN=$token TILO_APP_CHANNEL_OUT=$out/channel.json \
      tools/apps/publish-collections.sh
    jq -c . "$out/channel.json"
    ;;
  submit)
    release_only
    publication_env "$@"
    : "${TAPP_PUBLISHER_IDENTITY:?the publisher identity is not set: the check needs secrets = [\"TAPP_PUBLISHER_IDENTITY\"]}"
    TILO_APP_PUBLISH_STAGE=submit tools/apps/publish-collections.sh
    printf 'Submitted the apps of %s to %s\n' "$T_COMMIT" "$registry_url"
    ;;
  *)
    die "usage: t-release.sh build|sizes|thin-runtime|app-size|runtime-size|binaries|sign|publish|submit ..."
    ;;
esac
