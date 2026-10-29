#!/usr/bin/env bash
# Messages' optional Signal service as t's checks release it (t.toml, "Signal"; docs/publishing-on-t.md):
# the provider so.tilo.signal-provider and the runtime it runs in, so.tilo.runtime.signal, both
# x86_64 only. Messages offers the service; the collections don't carry it. It is released from
# the same stable commit as the apps, and each step is its own check, holding only what it needs:
#
#   build OUT            no credentials: compile the provider, its Java adaptations and the release
#                        tools (tools/apps/signal_provider_release.py compile) into OUT/compiled,
#                        then seal a copy with throwaway keys (`seal --rehearsal`), as `sign` will
#   sign BUILT OUT       TAPP_PUBLISHER_IDENTITY and RUNTIME_BUNDLE_SIGNING_KEY: seal BUILT/compiled
#                        as the signed runtime and provider packages, compiling nothing. The runtime
#                        identity must be the publisher installed systems trust
#                        (os/registry-runtime-publisher.hex)
#   upload SIGNED        REGISTRY_TOKEN: upload both packages' thin forms and payload objects
#   submit SIGNED        TAPP_PUBLISHER_IDENTITY: verify both packages against that identity, then
#                        sign and submit their registry entries
#
# `sign`, `upload` and `submit` run only in trusted runs of stable or a release tag (T_REF). A
# runtime build the registry already offers is left alone (publish-runtime-package.sh).
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
source "$repo_root/tools/ci/publish/lib.sh"
source "$repo_root/tools/apps/release-lib.sh"
cd "$repo_root"

package_base=https://code.tilo.so/api/packages/tilo/generic
registry_url=https://apps.tilo.so
release_tool() { python3 -I tools/ci/python.py tools/apps/signal_provider_release.py "$@"; }

# The packages sign sealed into SIGNED, as `directory package-id artifact name` lines.
packages() {
  printf '%s\n' 'runtime-package so.tilo.runtime.signal tilo-app-runtime-signal Signal runtime' \
    'provider-package so.tilo.signal-provider tilo-app-signal-provider Signal provider'
}

# The release sequence and commit sign sealed into SIGNED.
sealed() {
  jq -er ".$2" "$1/release.json"
}

command=${1:-}
shift || true
case $command in
  build)
    out=$(realpath -m "${1:?usage: t-signal-release.sh build OUT}")
    os/dev/assert-native-arch.sh x86_64
    # The Java adaptations compile with JDK 25 and the zero-assembler JVM builds against these
    # headers; the builder image carries only the JDK's runtime.
    command -v javac >/dev/null && [[ -f /usr/include/ffi.h && -f /usr/include/alsa/asoundlib.h && -f /usr/include/cups/cups.h ]] \
      || dnf install -y java-25-openjdk-devel libffi-devel alsa-lib-devel cups-devel
    export PATH=/usr/lib/jvm/java-25-openjdk/bin:$PATH
    export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$(cargo_jobs)}
    export CARGO_PROFILE_RELEASE_LTO=thin CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16
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
    rm -rf "$out"
    mkdir -p "$out"
    T_COMMIT=$source release_tool compile --source "$source" --out "$out/compiled"
    # Sealed once with throwaway keys, as `sign` will, and probed; the result is thrown away.
    rehearsal=$scratch/signal-rehearsal
    rm -rf "$rehearsal"
    cp -a "$out/compiled" "$rehearsal"
    for name in app runtime; do
      "$out/compiled/bin/tilo-runtime-bundle" key generate --out "$scratch/$name-throwaway.json"
    done
    T_COMMIT=$source TAPP_PUBLISHER_IDENTITY_FILE=$scratch/app-throwaway.json \
      RUNTIME_BUNDLE_IDENTITY_FILE=$scratch/runtime-throwaway.json \
      release_tool seal --rehearsal --source "$source" --sequence "$sequence" --out "$rehearsal"
    printf 'Built the Signal provider %s from %s\n' "$(sealed "$rehearsal" package_version)" "$source"
    ;;
  sign)
    built=${1:?usage: t-signal-release.sh sign BUILT OUT} out=$(realpath -m "${2:?}")
    release_only
    [[ $(uname -m) == x86_64 ]] || die "the Signal service is x86_64 only, not $(uname -m)"
    app_identity=$(secret_file TAPP_PUBLISHER_IDENTITY)
    runtime_identity=$(secret_file RUNTIME_BUNDLE_SIGNING_KEY)
    trap 'rm -f "$app_identity" "$runtime_identity"' EXIT
    rm -rf "$out"
    cp -a "$built/compiled" "$out"
    # Registry versions only ever increase: the release sequence is the commit's time.
    TAPP_PUBLISHER_IDENTITY_FILE=$app_identity RUNTIME_BUNDLE_IDENTITY_FILE=$runtime_identity \
      release_tool seal --source "$T_COMMIT" --sequence "$(commit_epoch "$T_COMMIT")" --out "$out"
    # What upload and submit read; the payload, its sources and the bundle stay behind.
    find "$out" -mindepth 1 -maxdepth 1 ! -name bin ! -name runtime-package ! -name provider-package \
      ! -name release.json ! -name build.json -exec rm -rf {} +
    printf 'Signed the Signal provider %s\n' "$(sealed "$out" package_version)"
    ;;
  upload)
    signed=${1:?usage: t-signal-release.sh upload SIGNED}
    release_only
    token=${REGISTRY_TOKEN:?the package token is not set: the check needs secrets = [\"REGISTRY_TOKEN\"]}
    unset REGISTRY_TOKEN
    [[ $(sealed "$signed" source) == "$T_COMMIT" ]] || die "$signed was sealed from another commit"
    while read -r directory package artifact _; do
      RUNTIME_PACKAGE_STAGE=upload VERSION=$(sealed "$signed" package_version) TILO_PACKAGE_TOKEN=$token \
        PACKAGE_API=$package_base REGISTRY_URL=$registry_url OWNER=tilo \
        REGISTRY_CLI=$signed/bin/tilo-app-package RUNNER_TEMP=$scratch \
        tools/apps/publish-runtime-package.sh "$signed/$directory" x86_64 "$package" "$artifact"
    done < <(packages)
    ;;
  submit)
    signed=${1:?usage: t-signal-release.sh submit SIGNED}
    release_only
    identity=$(secret_file TAPP_PUBLISHER_IDENTITY)
    trap 'rm -f "$identity"' EXIT
    [[ $(sealed "$signed" source) == "$T_COMMIT" ]] || die "$signed was sealed from another commit"
    # Both packages verify against the identity that signs their entries before either is offered.
    trusted_key=$("$signed/bin/tilo-runtime-bundle" key pubkey --identity "$identity")
    sequence=$(sealed "$signed" package_version)
    sequence=${sequence#0.1.} sequence=${sequence%%+*}
    TILO_SIGNAL_VERIFIER_BIN_DIR=$signed/bin release_tool verify --out "$signed" --source "$T_COMMIT" \
      --sequence "$sequence" --trusted-key "$trusted_key"
    metadata=$scratch/signal-metadata.json
    while read -r directory package artifact name; do
      jq -n --arg name "$name" '{name: {en: $name},
        description: {en: "Optional private Signal service for Messages, started only after protected account and network review."},
        tagline: {en: "Private Messages service"}, supported_languages: ["en"],
        source_url: "https://code.tilo.so/tilo/tilo"}' > "$metadata"
      RUNTIME_PACKAGE_STAGE=submit VERSION=$(sealed "$signed" package_version) \
        PACKAGE_API=$package_base REGISTRY_URL=$registry_url OWNER=tilo \
        REGISTRY_CLI=$signed/bin/tilo-app-package REGISTRY_IDENTITY=$identity RUNNER_TEMP=$scratch \
        tools/apps/publish-runtime-package.sh "$signed/$directory" x86_64 "$package" "$artifact" "$metadata"
    done < <(packages)
    printf 'Submitted the Signal provider of %s to %s\n' "$T_COMMIT" "$registry_url"
    ;;
  *)
    die "usage: t-signal-release.sh build|sign|upload|submit ..."
    ;;
esac
