#!/usr/bin/env bash
# Emit the reproducible OS release's SLSA v1 provenance predicate. The caller
# signs it as an in-toto attestation bound to RELEASE_DIGEST.
set -euo pipefail

out="${1:-provenance.json}"
: "${RELEASE_DIGEST:?RELEASE_DIGEST is required}"
: "${QOS_GENERATION:?QOS_GENERATION is required}"
: "${SOURCE_DATE_EPOCH:?SOURCE_DATE_EPOCH is required}"
: "${BASE_REF:?BASE_REF is required}"
: "${RPMS_REF:?RPMS_REF is required}"
: "${KERNEL_RPMS_REF:?KERNEL_RPMS_REF is required}"
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
require_digest_ref KERNEL_RPMS_REF "$KERNEL_RPMS_REF"
release_hash="${RELEASE_DIGEST#sha256:}"
[ "$release_hash" != "$RELEASE_DIGEST" ] && [ "${#release_hash}" -eq 64 ] \
  && case "$release_hash" in *[!0-9a-f]*) false ;; *) true ;; esac \
  || { printf 'release provenance: invalid RELEASE_DIGEST\n' >&2; exit 1; }
app_collection="${QOS_APP_COLLECTION_SHA256:-}"
if [ -n "$app_collection" ]; then
  [ "${#app_collection}" -eq 64 ] \
    && case "$app_collection" in *[!0-9a-f]*) false ;; *) true ;; esac \
    || { printf 'release provenance: invalid QOS_APP_COLLECTION_SHA256\n' >&2; exit 1; }
fi
desktop_payload="${QOS_DESKTOP_PAYLOAD_SHA256:-}"
if [ -n "$desktop_payload" ]; then
  [ "${#desktop_payload}" -eq 64 ] \
    && case "$desktop_payload" in *[!0-9a-f]*) false ;; *) true ;; esac \
    || { printf 'release provenance: invalid QOS_DESKTOP_PAYLOAD_SHA256\n' >&2; exit 1; }
fi

jq -n \
  --arg source "git+ssh://code.podesta.ai/qos/qos@$GITHUB_SHA" \
  --arg digest "$RELEASE_DIGEST" \
  --arg base "$BASE_REF" \
  --arg rpms "$RPMS_REF" \
  --arg kernel "$KERNEL_RPMS_REF" \
  --arg app_collection "$app_collection" \
  --arg desktop_payload "$desktop_payload" \
  --arg generation "$QOS_GENERATION" \
  --arg epoch "$SOURCE_DATE_EPOCH" \
  '{
    buildDefinition: {
      buildType: "https://qos.dev/buildtypes/bootc-os/v1",
      externalParameters: {generation: ($generation|tonumber)},
      internalParameters: {
        sourceDateEpoch: ($epoch|tonumber),
        expectedReleaseDigest: $digest
      },
      resolvedDependencies: (
        [{uri: $source}, {uri: $base}, {uri: $rpms}, {uri: $kernel}]
        + if $app_collection == "" then []
          else [{uri: ("qos-app-collection:sha256:" + $app_collection)}]
          end
        + if $desktop_payload == "" then []
          else [{uri: ("qos-desktop-payload:sha256:" + $desktop_payload)}]
          end
      )
    },
    runDetails: {
      builder: {id: "https://code.podesta.ai/qos/forgejo/actions/bootc-release-v1"},
      metadata: {invocationId: (env.GITHUB_RUN_ID // "local")},
      byproducts: [{
        name: "independent-rebuild",
        uri: "urn:qos:verification:manifest-digest-equal"
      }]
    }
  }' > "$out"
