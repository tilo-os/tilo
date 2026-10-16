#!/usr/bin/env bash
# Release / apps as t's checks run it (t.toml, "Apps"; docs/publishing-on-t.md). Each step is its
# own check, holding only what it needs:
#
#   build ARCH OUT       no credentials: compile every app of the default collection and the
#                        release tools for this machine's architecture (ARCH, x86_64 or
#                        aarch64), package them with throwaway keys, run the release smoke
#                        tests and the reproducibility check, and keep what signing needs in
#                        OUT/compiled
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
cd "$repo_root"

# Public settings the release has always used (formerly repository variables).
package_base=https://code.tilo.so/api/packages/tilo/generic
registry_url=https://apps.tilo.so
registry_owner_pubkey=55d9a9c748bcfe34200832c18c5f0cf7932f17571612321eaa542474fa2abe06

default_apps() {
  python3 -I tools/ci/python.py tools/apps/app_release.py --collection default | jq -r '.apps | join(" ")'
}

release_only() {
  [[ $(channel) == release ]] || die "apps are signed and published from stable or a release tag, not ${T_REF}"
  require_commit
}

# A file holding secret $1's value, readable only by this user, removed on exit.
secret_file() {
  local name=$1 file
  [[ -n ${!name:-} ]] || die "$name is not set: the check needs secrets = [\"$name\"]"
  file=$(mktemp "$scratch/$name.XXXXXX")
  (umask 077 && printf '%s' "${!name}" > "$file")
  unset "$name"
  printf '%s' "$file"
}

# Every core compiles, bounded by memory: 2 GiB per job.
cargo_jobs() {
  local cpus memory limit quota period jobs
  cpus=$(nproc)
  if read -r quota period 2>/dev/null < /sys/fs/cgroup/cpu.max && [[ $quota != max ]]; then
    cpus=$(( (quota + period - 1) / period ))
  fi
  memory=$(( $(sed -n 's/^MemTotal: *\([0-9]*\) kB$/\1/p' /proc/meminfo) << 10 ))
  if read -r limit 2>/dev/null < /sys/fs/cgroup/memory.max && [[ $limit =~ ^[0-9]+$ ]]; then
    (( limit < memory )) && memory=$limit
  fi
  jobs=$(( memory / (2 << 30) ))
  (( jobs > cpus )) && jobs=$cpus
  (( jobs < 1 )) && jobs=1
  printf '%s' "$jobs"
}

# The commit's time in seconds, which the release sequence and the packages' timestamps are.
# The trusted steps on x86_64 run without Git (t's x86 micro-VMs have no room for a Git disk
# beside the needs' outputs), so it comes from the forge, which serves the public history.
commit_epoch() {
  local date
  if git rev-parse --verify --quiet "$T_COMMIT^{commit}" >/dev/null 2>&1; then
    git show -s --format=%ct "$T_COMMIT"
    return
  fi
  date=$(curl --fail --silent --show-error --proto '=https' --retry 3 \
    "https://code.tilo.so/api/v1/repos/tilo/tilo/git/commits/$T_COMMIT?stat=false&verification=false&files=false" \
    | jq -er '.commit.committer.date') || die "could not read $T_COMMIT's time from the forge"
  date -d "$date" +%s
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
    die "usage: t-release.sh build|sign|publish|submit ..."
    ;;
esac
