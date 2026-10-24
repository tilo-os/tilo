#!/usr/bin/env bash
# Static and executable checks for the OS release boundary scripts. These are
# cheap enough for ordinary CI.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
provenance="$repo/os/dev/release-provenance.sh"
freshness="$repo/os/dev/release-freshness.sh"
fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

grep -Fq 'mapfile -t reproducible < <(podman_epoch_args "$SOURCE_DATE_EPOCH" "${podman_args[@]}")' "$repo/os/dev/reseal-image.sh" \
  && grep -Fq -- "printf '%s\\n' --source-date-epoch \"\$epoch\" --rewrite-timestamp" "$repo/os/dev/_common.sh" \
  && grep -Fq -- "printf '%s\\n' --timestamp \"\$epoch\"" "$repo/os/dev/_common.sh" \
  && ok 'resealing clamps timestamps to SOURCE_DATE_EPOCH' \
  || bad 'resealing keeps build-time timestamps'

grep -Fq '604800' "$freshness" \
  && ok 'release freshness statements are bounded to seven days' \
  || bad 'release freshness is unbounded'

grep -Fq 'COSIGN_KEY_REF must name a non-exportable KMS/HSM key' "$repo/os/dev/configure-cosign-kms.sh" \
  && ok 'release signing requires a non-exportable KMS/HSM key' \
  || bad 'release signing still accepts an exportable private key'

attestation_consumers=(
  "$repo/os/dev/resolve-rpm-snapshot.sh"
  "$repo/os/dev/build-image.sh"
)
attestation_shape_safe=1
for consumer in "${attestation_consumers[@]}"; do
  grep -Fq '[(if type == "array" then .[] else . end) | .payload' "$consumer" \
    || attestation_shape_safe=0
  grep -Fq '[.[].payload' "$consumer" && attestation_shape_safe=0
done
# The kernel's provenance (os/dev/verify-kernel-provenance.sh, which kernel.sh runs before
# publishing) is read in Python: one envelope or an array of them.
grep -Fq 'yield from value if isinstance(value, list) else (value,)' \
  "$repo/os/dev/kernel-build-environment.py" || attestation_shape_safe=0
encoded_predicate="$(printf '%s' '{"predicate":{"shapeSafe":true}}' | base64 -w0)"
for envelope in \
  "{\"payload\":\"$encoded_predicate\"}" \
  "[{\"payload\":\"$encoded_predicate\"}]"; do
  printf '%s' "$envelope" | jq -e '
    [(if type == "array" then .[] else . end) | .payload | @base64d | fromjson | .predicate]
    == [{"shapeSafe":true}]
  ' >/dev/null || attestation_shape_safe=0
done
if [ "$attestation_shape_safe" = 1 ]; then
  ok 'Cosign provenance accepts one verified envelope or an envelope array'
else
  bad 'Cosign provenance assumes only one verify-attestation JSON root shape'
fi

if grep -Fq 'TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY must be a non-placeholder 32-byte hex key' "$repo/os/Containerfile" \
   && grep -Fxq 'ARG TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY' "$repo/os/Containerfile"; then
  ok 'fresh images carry a validated pinned marketplace owner key'
else
  bad 'marketplace trust anchor is missing, placeholder, or not a build input'
fi

grep -Fq 'verify_signed_generation "$current"' "$repo/os/dev/next-generation.sh" \
  && grep -Fq 'latest does not point at signed gen$current' "$repo/os/dev/next-generation.sh" \
  && grep -Fq -- '--type slsaprovenance1' "$repo/os/dev/next-generation.sh" \
  && ok 'generation allocation authenticates provenance and the latest channel' \
  || bad 'generation allocation still trusts registry tags alone'

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
hex="$(printf 'a%.0s' {1..64})"
if RELEASE_DIGEST="sha256:$hex" TILO_GENERATION=42 SOURCE_DATE_EPOCH=1 \
   BASE_REF="base@sha256:$hex" RPMS_REF="rpms@sha256:$hex" \
   KERNEL_RPMS_REF="kernel@sha256:$hex" TILO_APP_COLLECTION_SHA256="$hex" \
   TILO_DESKTOP_PAYLOAD_SHA256="$hex" TILO_SBOM_SHA256="$hex" \
   SOURCE_REVISION="$hex" \
   "$provenance" "$tmp/provenance.json" \
   && jq -e '.buildDefinition.externalParameters.generation == 42' "$tmp/provenance.json" >/dev/null \
   && jq -e '.buildDefinition.resolvedDependencies[] | select(.uri == "tilo-app-collection:sha256:'"$hex"'")' "$tmp/provenance.json" >/dev/null \
   && jq -e '.buildDefinition.resolvedDependencies[] | select(.uri == "tilo-desktop-payload:sha256:'"$hex"'")' "$tmp/provenance.json" >/dev/null \
   && jq -e '.buildDefinition.internalParameters.expectedReleaseDigest == "sha256:'"$hex"'"' "$tmp/provenance.json" >/dev/null \
   && jq -e '.runDetails.byproducts[0].uri == "urn:tilo:verification:manifest-digest-equal"' "$tmp/provenance.json" >/dev/null \
   && jq -e '.runDetails.byproducts[] | select(.name == "spdx-sbom" and .uri == "sha256:'"$hex"'")' "$tmp/provenance.json" >/dev/null; then
  ok 'SLSA provenance binds generation, source inputs, app collection, image and SBOM digests'
else
  bad 'release provenance generator did not produce a bound predicate'
fi

children='[{"architecture":"x86_64","digest":"sha256:'"$hex"'"},{"architecture":"aarch64","digest":"sha256:'"$(printf 'b%.0s' {1..64})"'"}]'
kernel_refs='[{"architecture":"x86_64","ref":"registry.tilo.so/tilo/tilo-kernel@sha256:'"$hex"'"},{"architecture":"aarch64","ref":"registry.tilo.so/tilo/tilo-kernel@sha256:'"$(printf 'b%.0s' {1..64})"'"}]'
if RELEASE_DIGEST="sha256:$hex" TILO_GENERATION=42 SOURCE_DATE_EPOCH=1 \
   BASE_REF="base@sha256:$hex" RPMS_REF="rpms@sha256:$hex" \
   TILO_KERNEL_REFS_JSON="$kernel_refs" TILO_RELEASE_CHILDREN_JSON="$children" \
   SOURCE_REVISION="$hex" \
   "$provenance" "$tmp/index-provenance.json" \
   && jq -e --argjson children "$children" \
     '.buildDefinition.externalParameters.children == $children' \
     "$tmp/index-provenance.json" >/dev/null \
   && jq -e --argjson kernels "$kernel_refs" \
     '.buildDefinition.externalParameters.kernelInputs == $kernels' \
     "$tmp/index-provenance.json" >/dev/null; then
  ok 'index provenance binds exact architecture child and kernel digests'
else
  bad 'index provenance omits or rewrites native child or kernel inputs'
fi

if RELEASE_DIGEST="sha256:$hex" TILO_GENERATION=42 SOURCE_DATE_EPOCH=1 \
   BASE_REF='base:latest' RPMS_REF="rpms@sha256:$hex" \
   KERNEL_RPMS_REF="kernel@sha256:$hex" SOURCE_REVISION="$hex" \
   "$provenance" "$tmp/bad.json" >/dev/null 2>&1; then
  bad 'provenance accepted an unpinned build dependency'
else
  ok 'provenance refuses unpinned build dependencies'
fi

firmware="code.tilo.so/tilo/lenovo-firmware@sha256:$hex"
if RELEASE_DIGEST="sha256:$hex" TILO_GENERATION=1 SOURCE_DATE_EPOCH=1 \
   BASE_REF="base@sha256:$hex" RPMS_REF="rpms@sha256:$hex" \
   KERNEL_RPMS_REF="kernel@sha256:$hex" SOURCE_REVISION="$hex" \
   TILO_HARDWARE_PROFILE=lenovo-83ed TILO_RELEASE_ARCHITECTURE=aarch64 \
   TILO_FIRMWARE_CANDIDATE="$firmware" \
   TILO_FIRMWARE_MANIFEST_SHA256="$hex" \
   "$provenance" "$tmp/lenovo-provenance.json" \
   && jq -e --arg firmware "$firmware" --arg manifest "$hex" \
     --arg definition "$(sha256sum "$repo/os/hardware/profiles/lenovo-83ed.json" | cut -d' ' -f1)" '
     .buildDefinition.externalParameters.hardwareProfile == "lenovo-83ed"
     and .buildDefinition.externalParameters.architecture == "aarch64"
     and .buildDefinition.externalParameters.firmwareManifestSha256 == $manifest
     and .buildDefinition.externalParameters.hardwareDefinitionSha256 == $definition
     and any(.buildDefinition.resolvedDependencies[]; .uri == $firmware)
   ' "$tmp/lenovo-provenance.json" >/dev/null \
   && ! (RELEASE_DIGEST="sha256:$hex" TILO_GENERATION=1 SOURCE_DATE_EPOCH=1 \
         BASE_REF="base@sha256:$hex" RPMS_REF="rpms@sha256:$hex" \
         KERNEL_RPMS_REF="kernel@sha256:$hex" SOURCE_REVISION="$hex" \
         TILO_HARDWARE_PROFILE=generic TILO_FIRMWARE_CANDIDATE="$firmware" \
         "$provenance" "$tmp/bad-generic-firmware.json" >/dev/null 2>&1); then
  ok 'Lenovo release provenance binds reviewed firmware and its hardware definition, and generic releases reject firmware'
else
  bad 'Lenovo firmware input is absent from release provenance or can enter a generic release'
fi

fex="registry.tilo.so/tilo/tilo-fex-payload-candidate@sha256:$hex"
guest="registry.tilo.so/tilo/tilo-development-windows-guest@sha256:$(printf 'b%.0s' {1..64})"
if RELEASE_DIGEST="sha256:$hex" TILO_GENERATION=1 SOURCE_DATE_EPOCH=1 \
   BASE_REF="base@sha256:$hex" RPMS_REF="rpms@sha256:$hex" \
   KERNEL_RPMS_REF="kernel@sha256:$hex" SOURCE_REVISION="$hex" \
   TILO_HARDWARE_PROFILE=lenovo-83ed TILO_RELEASE_ARCHITECTURE=aarch64 \
   TILO_FIRMWARE_CANDIDATE="$firmware" TILO_FIRMWARE_MANIFEST_SHA256="$hex" \
   TILO_WINDOWS_PROVIDER_INPUT_CLASS=development-only \
   FEX_PAYLOAD_REF="$fex" WINDOWS_GUEST_REF="$guest" \
   "$provenance" "$tmp/lenovo-windows-provenance.json" \
   && jq -e --arg fex "$fex" --arg guest "$guest" '
     .buildDefinition.externalParameters.windowsProvider == {
       inputClass: "development-only", runtimeAcceptanceVerified: false,
       fexRef: $fex, guestRef: $guest
     }
     and any(.buildDefinition.resolvedDependencies[]; .uri == $fex)
     and any(.buildDefinition.resolvedDependencies[]; .uri == $guest)
   ' "$tmp/lenovo-windows-provenance.json" >/dev/null \
   && ! (RELEASE_DIGEST="sha256:$hex" TILO_GENERATION=1 SOURCE_DATE_EPOCH=1 \
         BASE_REF="base@sha256:$hex" RPMS_REF="rpms@sha256:$hex" \
         KERNEL_RPMS_REF="kernel@sha256:$hex" SOURCE_REVISION="$hex" \
         TILO_HARDWARE_PROFILE=lenovo-83ed TILO_RELEASE_ARCHITECTURE=aarch64 \
         TILO_FIRMWARE_CANDIDATE="$firmware" TILO_FIRMWARE_MANIFEST_SHA256="$hex" \
         TILO_WINDOWS_PROVIDER_INPUT_CLASS=development-only \
         FEX_PAYLOAD_REF="$fex" WINDOWS_GUEST_REF= \
         "$provenance" "$tmp/bad-windows-provider.json" >/dev/null 2>&1); then
  ok 'one-off Lenovo Windows provider provenance binds both digests and development status'
else
  bad 'development Windows provider provenance omitted a digest or accepted an incomplete pair'
fi

exit "$fail"
