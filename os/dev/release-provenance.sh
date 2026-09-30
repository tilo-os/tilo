#!/usr/bin/env bash
# Emit the reproducible OS release's SLSA v1 provenance predicate. The caller
# signs it as an in-toto attestation bound to RELEASE_DIGEST.
set -euo pipefail

out="${1:-provenance.json}"
: "${RELEASE_DIGEST:?RELEASE_DIGEST is required}"
: "${TILO_GENERATION:?TILO_GENERATION is required}"
: "${SOURCE_DATE_EPOCH:?SOURCE_DATE_EPOCH is required}"
: "${BASE_REF:?BASE_REF is required}"
: "${RPMS_REF:?RPMS_REF is required}"
: "${GITHUB_SHA:?GITHUB_SHA is required}"

require_digest_ref() {
  local digest
  case "$2" in
    *@sha256:*) ;;
    *) printf 'release provenance: %s is not digest-pinned: %s\n' "$1" "$2" >&2; exit 1 ;;
  esac
  digest="${2##*@sha256:}"
  [ "${#digest}" -eq 64 ] && case "$digest" in *[!0-9a-f]*) false ;; *) true ;; esac || {
    printf 'release provenance: %s has an invalid sha256 digest\n' "$1" >&2
    exit 1
  }
}
require_digest_ref BASE_REF "$BASE_REF"
require_digest_ref RPMS_REF "$RPMS_REF"
kernel_refs="${TILO_KERNEL_REFS_JSON:-}"
if [ -n "$kernel_refs" ]; then
  printf '%s' "$kernel_refs" | jq -e '
    type == "array" and length == 2
    and (map(.architecture) | sort) == ["aarch64", "x86_64"]
    and (map(.ref) | unique | length) == 2
    and all(.[].ref;
      type == "string"
      and test("^registry\\.tilo\\.so/tilo/tilo-kernel@sha256:[0-9a-f]{64}$"))
  ' >/dev/null || {
    printf 'release provenance: invalid TILO_KERNEL_REFS_JSON\n' >&2
    exit 1
  }
else
  : "${KERNEL_RPMS_REF:?KERNEL_RPMS_REF is required}"
  require_digest_ref KERNEL_RPMS_REF "$KERNEL_RPMS_REF"
  kernel_refs="$(jq -cn --arg ref "$KERNEL_RPMS_REF" '[{ref:$ref}]')"
fi
release_hash="${RELEASE_DIGEST#sha256:}"
[ "$release_hash" != "$RELEASE_DIGEST" ] && [ "${#release_hash}" -eq 64 ] \
  && case "$release_hash" in *[!0-9a-f]*) false ;; *) true ;; esac \
  || { printf 'release provenance: invalid RELEASE_DIGEST\n' >&2; exit 1; }
app_collection="${TILO_APP_COLLECTION_SHA256:-}"
if [ -n "$app_collection" ]; then
  [ "${#app_collection}" -eq 64 ] \
    && case "$app_collection" in *[!0-9a-f]*) false ;; *) true ;; esac \
    || { printf 'release provenance: invalid TILO_APP_COLLECTION_SHA256\n' >&2; exit 1; }
fi
desktop_payload="${TILO_DESKTOP_PAYLOAD_SHA256:-}"
if [ -n "$desktop_payload" ]; then
  [ "${#desktop_payload}" -eq 64 ] \
    && case "$desktop_payload" in *[!0-9a-f]*) false ;; *) true ;; esac \
    || { printf 'release provenance: invalid TILO_DESKTOP_PAYLOAD_SHA256\n' >&2; exit 1; }
fi
sbom_hash="${TILO_SBOM_SHA256:-}"
if [ -n "$sbom_hash" ]; then
  [[ "$sbom_hash" =~ ^[0-9a-f]{64}$ ]] \
    || { printf 'release provenance: invalid TILO_SBOM_SHA256\n' >&2; exit 1; }
fi
release_children="${TILO_RELEASE_CHILDREN_JSON:-[]}"
hardware_profile="${TILO_HARDWARE_PROFILE:-generic}"
release_architecture="${TILO_RELEASE_ARCHITECTURE:-}"
firmware_candidate="${TILO_FIRMWARE_CANDIDATE:-}"
firmware_manifest="${TILO_FIRMWARE_MANIFEST_SHA256:-}"
windows_input_class="${TILO_WINDOWS_PROVIDER_INPUT_CLASS:-repository-pinned}"
fex_payload_ref="${FEX_PAYLOAD_REF:-}"
windows_guest_ref="${WINDOWS_GUEST_REF:-}"
case "$hardware_profile" in
  generic)
    [ -z "$firmware_candidate$firmware_manifest" ] \
      || { printf 'generic provenance cannot carry board firmware\n' >&2; exit 1; } ;;
  lenovo-83ed)
    [ "$release_architecture" = aarch64 ] \
      || { printf 'Lenovo provenance requires native aarch64 build\n' >&2; exit 1; }
    require_digest_ref TILO_FIRMWARE_CANDIDATE "$firmware_candidate"
    [[ "$firmware_candidate" =~ ^code\.tilo\.so/tilo/[a-z0-9][a-z0-9._-]*@sha256:[0-9a-f]{64}$ \
       && "$firmware_manifest" =~ ^[0-9a-f]{64}$ ]] \
      || { printf 'Lenovo provenance requires pinned board firmware\n' >&2; exit 1; } ;;
  *) printf 'unsupported hardware profile in provenance\n' >&2; exit 1 ;;
esac
case "$windows_input_class" in
  repository-pinned) ;;
  development-only)
    [ "$hardware_profile" = lenovo-83ed ] && [ "$release_architecture" = aarch64 ] \
      || { printf 'development Windows provider requires Lenovo aarch64 provenance\n' >&2; exit 1; }
    require_digest_ref FEX_PAYLOAD_REF "$fex_payload_ref"
    require_digest_ref WINDOWS_GUEST_REF "$windows_guest_ref"
    [[ "$fex_payload_ref" =~ ^(registry|code)\.tilo\.so/tilo/[a-z0-9][a-z0-9._/-]*@sha256:[0-9a-f]{64}$ \
       && "$windows_guest_ref" =~ ^(registry|code)\.tilo\.so/tilo/[a-z0-9][a-z0-9._/-]*@sha256:[0-9a-f]{64}$ ]] \
      || { printf 'development Windows provider needs paired registry digests\n' >&2; exit 1; } ;;
  *) printf 'unsupported Windows provider input class in provenance\n' >&2; exit 1 ;;
esac
printf '%s' "$release_children" | jq -e '
  type == "array"
  and (length == 0 or (map(.architecture) | sort) == ["aarch64", "x86_64"])
  and (length == 0 or (map(.digest) | unique | length) == 2)
  and all(.[];
    (.architecture == "x86_64" or .architecture == "aarch64")
    and (.digest | type == "string" and test("^sha256:[0-9a-f]{64}$")))
' >/dev/null || {
  printf 'release provenance: invalid TILO_RELEASE_CHILDREN_JSON\n' >&2
  exit 1
}

jq -n \
  --arg source "git+ssh://code.tilo.so/tilo/tilo@$GITHUB_SHA" \
  --arg digest "$RELEASE_DIGEST" \
  --arg base "$BASE_REF" \
  --arg rpms "$RPMS_REF" \
  --argjson kernels "$kernel_refs" \
  --arg app_collection "$app_collection" \
  --arg desktop_payload "$desktop_payload" \
  --arg sbom_hash "$sbom_hash" \
  --arg hardware_profile "$hardware_profile" \
  --arg release_architecture "$release_architecture" \
  --arg firmware_candidate "$firmware_candidate" \
  --arg firmware_manifest "$firmware_manifest" \
  --arg windows_input_class "$windows_input_class" \
  --arg fex_payload_ref "$fex_payload_ref" \
  --arg windows_guest_ref "$windows_guest_ref" \
  --argjson release_children "$release_children" \
  --arg generation "$TILO_GENERATION" \
  --arg epoch "$SOURCE_DATE_EPOCH" \
  '{
    buildDefinition: {
      buildType: "https://tilo.sh/buildtypes/bootc-os/v1",
      externalParameters: ({generation: ($generation|tonumber)}
        + if $hardware_profile == "generic" then {}
          else {architecture: $release_architecture, hardwareProfile: $hardware_profile,
                firmwareManifestSha256: $firmware_manifest}
          end
        + if ($release_children | length) == 0 then {}
          else {children: $release_children}
          end
        + if ($kernels | length) == 1 then {}
          else {kernelInputs: $kernels}
          end
        + if $windows_input_class == "development-only" then
            {windowsProvider: {inputClass: "development-only",
              runtimeAcceptanceVerified: false, fexRef: $fex_payload_ref,
              guestRef: $windows_guest_ref}}
          else {} end),
      internalParameters: {
        sourceDateEpoch: ($epoch|tonumber),
        expectedReleaseDigest: $digest
      },
      resolvedDependencies: (
        ([{uri: $source}, {uri: $base}, {uri: $rpms}]
        + ($kernels | map({uri: .ref})))
        + if $app_collection == "" then []
          else [{uri: ("tilo-app-collection:sha256:" + $app_collection)}]
          end
        + if $desktop_payload == "" then []
          else [{uri: ("tilo-desktop-payload:sha256:" + $desktop_payload)}]
          end
        + if $firmware_candidate == "" then []
          else [{uri: $firmware_candidate}]
          end
        + if $windows_input_class == "development-only" then
            [{uri: $fex_payload_ref}, {uri: $windows_guest_ref}]
          else [] end
      )
    },
    runDetails: {
      builder: {id: "https://code.tilo.so/tilo/forgejo/actions/bootc-release-v1"},
      metadata: {invocationId: (env.GITHUB_RUN_ID // "local")},
      byproducts: ([{
        name: "independent-rebuild",
        uri: "urn:tilo:verification:manifest-digest-equal"
      }] + if $sbom_hash == "" then [] else [{
        name: "spdx-sbom",
        uri: ("sha256:" + $sbom_hash)
      }] end)
    }
  }' > "$out"
