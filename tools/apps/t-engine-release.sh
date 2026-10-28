#!/usr/bin/env bash
# The Browser's Chromium engine runtime (tilo-runtime/<RUNTIME_VERSION>, package
# so.tilo.runtime.chromium) as t's checks release it (t.toml, "Engine"; docs/publishing-on-t.md).
# Apps name it in `optional-runtimes`, so the app collections do not carry it: it is built, sealed
# and offered on its own, from the same stable commit as the apps. Each step is its own check,
# holding only what it needs:
#
#   build ARCH OUT       no credentials: compile the pinned CEF host and the release tools for this
#                        machine's architecture (tools/apps/browser_runtime_release.py compile) into
#                        OUT/compiled, then seal a copy with throwaway keys, exactly as `sign` will
#   sign ARCH BUILT OUT  TAPP_PUBLISHER_IDENTITY and RUNTIME_BUNDLE_SIGNING_KEY: seal BUILT/compiled
#                        as the signed runtime bundle and its runtime package, compiling nothing
#   dev ARCH BUILT OUT   no credentials: seal BUILT/compiled with a key thrown away, as build's
#                        rehearsal does, and keep its runtime bundle in OUT/bundles/<version>: the
#                        engine a development layer ships (tilo-vm's apps+runtime pack on t, through
#                        TILO_RUNTIME_BUNDLE_DIR) where the registry doesn't offer it. Never published
#   upload SIGNED..      REGISTRY_TOKEN: upload each package's thin form and payload objects
#   submit SIGNED..      TAPP_PUBLISHER_IDENTITY: sign and submit each package's registry entry
#
# `sign`, `upload` and `submit` run only in trusted runs of stable or a release tag (T_REF).
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
source "$repo_root/tools/ci/publish/lib.sh"
source "$repo_root/tools/apps/release-lib.sh"
cd "$repo_root"

package_base=https://code.tilo.so/api/packages/tilo/generic
registry_url=https://apps.tilo.so
release_tool() { python3 -I tools/ci/python.py tools/apps/browser_runtime_release.py "$@"; }

# Seal BUILT's compiled engine into OUT for ARCH from SOURCE at release sequence SEQUENCE.
seal() {
  local arch=$1 built=$2 out=$3 source=$4 sequence=$5
  rm -rf "$out"
  mkdir -p "$out"
  cp -a "$built/compiled/." "$out/"
  T_COMMIT=$source release_tool seal --arch "$arch" --source "$source" --sequence "$sequence" --out "$out"
  # What upload and submit read; the engine tree, the bundle and the work files stay behind.
  rm -rf "$out/engine" "$out/runtime" "$out/work"
}

# The package version sign sealed into SIGNED, which names its generic package.
package_version() {
  jq -er '.package_version' "$1/package/provenance.json"
}

command=${1:-}
shift || true
case $command in
  build)
    arch=${1:?usage: t-engine-release.sh build ARCH OUT} out=$(realpath -m "${2:?}")
    os/dev/assert-native-arch.sh "$arch"
    bash tools/ci/ensure-browser-dependencies.sh
    command -v cmake >/dev/null || dnf install -y cmake
    export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$(cargo_jobs)}
    # The provenance names the commit built: a trusted run's, or a Git checkout's clean HEAD. A
    # working tree on t has neither, so its rehearsal names none (zeros), which `sign` refuses.
    if [[ -n ${T_COMMIT:-} ]]; then
      require_commit
      source=$T_COMMIT sequence=$(commit_epoch "$source")
    elif git rev-parse --git-dir >/dev/null 2>&1; then
      source=$(git rev-parse HEAD) sequence=$(commit_epoch "$source")
    else
      source=$(printf '0%.0s' {1..40}) sequence=1
    fi
    export SOURCE_DATE_EPOCH=$sequence
    rm -rf "$out"
    mkdir -p "$out"
    T_COMMIT=$source release_tool compile --arch "$arch" --source "$source" --out "$out/compiled"
    # Sealed once with throwaway keys, exactly as `sign` will, and checked.
    REQUESTED_PUBLISH=false seal "$arch" "$out" "$scratch/engine-rehearsal" "$source" "$sequence"
    printf 'Built the engine %s for %s from %s\n' "$(package_version "$scratch/engine-rehearsal")" "$arch" "$source"
    ;;
  sign)
    arch=${1:?usage: t-engine-release.sh sign ARCH BUILT OUT} built=${2:?} out=$(realpath -m "${3:?}")
    release_only
    [[ $(uname -m) == "$arch" ]] || die "signing $arch on $(uname -m)"
    app_identity=$(secret_file TAPP_PUBLISHER_IDENTITY)
    runtime_identity=$(secret_file RUNTIME_BUNDLE_SIGNING_KEY)
    trap 'rm -f "$app_identity" "$runtime_identity"' EXIT
    REQUESTED_PUBLISH=true TAPP_PUBLISHER_IDENTITY_FILE=$app_identity \
      RUNTIME_BUNDLE_IDENTITY_FILE=$runtime_identity seal "$arch" "$built" "$out" "$T_COMMIT" \
      "$(commit_epoch "$T_COMMIT")"
    printf 'Signed the engine %s for %s\n' "$(package_version "$out")" "$arch"
    ;;
  dev)
    arch=${1:?usage: t-engine-release.sh dev ARCH BUILT OUT} built=${2:?} out=$(realpath -m "${3:?}")
    os/dev/assert-native-arch.sh "$arch"
    # The commit the build's provenance names (zeros for a working tree's build on t): sealing
    # checks the compiled files against it.
    source=$(jq -er '.source' "$built/compiled/build.json")
    work=$scratch/engine-dev
    rm -rf "$work" "$out"
    mkdir -p "$work" "$out/bundles"
    cp -a "$built/compiled/." "$work/"
    REQUESTED_PUBLISH=false T_COMMIT=$source release_tool seal --arch "$arch" --source "$source" \
      --sequence 1 --out "$work"
    version=$(python3 -I -c 'import sys, tomllib; print(tomllib.load(open(sys.argv[1], "rb"))["version"])' \
      "$work/runtime/bundle.toml")
    cp -a "$work/runtime" "$out/bundles/$version"
    printf 'Sealed the engine %s for %s from %s for development layers\n' "$version" "$arch" "$source"
    ;;
  upload)
    release_only
    token=${REGISTRY_TOKEN:?the package token is not set: the check needs secrets = [\"REGISTRY_TOKEN\"]}
    unset REGISTRY_TOKEN
    for signed in "$@"; do
      arch=$(jq -er '.architecture' "$signed/package/provenance.json")
      RUNTIME_PACKAGE_STAGE=upload VERSION=$(package_version "$signed") TILO_PACKAGE_TOKEN=$token \
        PACKAGE_API=$package_base REGISTRY_URL=$registry_url OWNER=tilo \
        REGISTRY_CLI=$signed/bin/tilo-app-package RUNNER_TEMP=$scratch \
        tools/apps/publish-runtime-package.sh "$signed/package" "$arch" so.tilo.runtime.chromium \
        tilo-app-runtime-chromium
    done
    ;;
  submit)
    release_only
    identity=$(secret_file TAPP_PUBLISHER_IDENTITY)
    trap 'rm -f "$identity"' EXIT
    metadata=$scratch/engine-metadata.json
    jq -n '{name: {en: "Browser engine"},
            description: {en: "The Chromium engine the Browser renders web pages with. The Store installs it together with the Browser."},
            tagline: {en: "Web engine for the Browser"}, supported_languages: ["en"],
            source_url: "https://code.tilo.so/tilo/tilo"}' > "$metadata"
    for signed in "$@"; do
      arch=$(jq -er '.architecture' "$signed/package/provenance.json")
      RUNTIME_PACKAGE_STAGE=submit VERSION=$(package_version "$signed") \
        PACKAGE_API=$package_base REGISTRY_URL=$registry_url OWNER=tilo \
        REGISTRY_CLI=$signed/bin/tilo-app-package REGISTRY_IDENTITY=$identity RUNNER_TEMP=$scratch \
        tools/apps/publish-runtime-package.sh "$signed/package" "$arch" so.tilo.runtime.chromium \
        tilo-app-runtime-chromium "$metadata"
    done
    printf 'Submitted the engine of %s to %s\n' "$T_COMMIT" "$registry_url"
    ;;
  *)
    die "usage: t-engine-release.sh build|dev|sign|upload|submit ..."
    ;;
esac
