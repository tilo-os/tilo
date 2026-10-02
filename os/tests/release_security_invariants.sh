#!/usr/bin/env bash
# Static and executable checks for the OS release boundary. These are cheap
# enough for ordinary CI; the workflow still performs the expensive independent
# rebuild, VM boot and signature verification on a real release.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workflow="$repo/.forgejo/workflows/build-image.yml"
provenance="$repo/os/dev/release-provenance.sh"
freshness="$repo/os/dev/release-freshness.sh"
fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

# Generic publication triggers share the generation-series lock. A protected
# Lenovo dispatch uses its own source-specific group because it never allocates
# a generic genN; validation builds remain per-ref.
grep -Fq "group: \${{ github.event_name == 'workflow_call' && format('tilo-os-dev-{0}', github.ref == 'refs/heads/main' && github.sha || github.ref) || (github.ref == 'refs/heads/main' && github.event_name == 'workflow_dispatch' && github.event.inputs.release == 'true' && github.event.inputs.hardware_profile == 'lenovo-83ed') && format('tilo-os-image-lenovo-{0}', github.sha) || (startsWith(github.ref, 'refs/tags/v') || (github.ref == 'refs/heads/main' && (github.event_name == 'schedule' || (github.event_name == 'workflow_dispatch' && github.event.inputs.release == 'true')))) && 'tilo-os-image-series-2' || format('tilo-os-image-validate-{0}', github.ref) }}" "$workflow" \
  && ! grep -Eq 'CONTINUOUS_RELEASE: "1"' "$workflow" \
  && ok 'generic generation publication is serialized separately from Lenovo' \
  || bad 'release jobs can race and reuse one generation'

kernel_verify_line="$(grep -n './os/dev/verify-kernel-provenance.sh "$ref"' "$workflow" | tail -1 | cut -d: -f1)"
kernel_export_line="$(grep -n 'echo "KERNEL_RPMS_REF=$ref" >> "$GITHUB_ENV"' "$workflow" | head -1 | cut -d: -f1)"
kernel_build_arg_line="$(grep -n 'KERNEL_RPMS_REF=${{ env.KERNEL_RPMS_REF }}' "$workflow" | head -1 | cut -d: -f1)"
if [ -n "$kernel_verify_line" ] && [ -n "$kernel_export_line" ] && [ -n "$kernel_build_arg_line" ] \
   && [ "$kernel_verify_line" -lt "$kernel_export_line" ] \
   && [ "$kernel_export_line" -lt "$kernel_build_arg_line" ]; then
  ok 'kernel signature verification precedes export and every image build'
else
  bad 'kernel repository reaches an image build before signature verification'
fi

publication_guards="$(grep -Fc "if: steps.release-mode.outputs.publish == 'true'" "$workflow")"
if grep -Fq 'REQUESTED_RELEASE: ${{ github.event.inputs.release }}' "$workflow" \
   && grep -Fq '$GITHUB_EVENT_NAME == workflow_dispatch && $GITHUB_REF == refs/heads/main' "$workflow" \
   && grep -Fq '${REQUESTED_RELEASE:-false} == true' "$workflow" \
   && grep -Fq "if [[ '\${{ steps.release-mode.outputs.publish }}' == true ]]; then" "$workflow" \
   && [ "$publication_guards" -ge 6 ] \
   && ! grep -Eq "if:.*github.event_name == 'workflow_dispatch'[[:space:]]*}}" "$workflow"; then
  ok 'manual publication is explicit and restricted to main'
else
  bad 'a build-only or feature-branch dispatch can reach a publication step'
fi

grep -Fq 'Independently rebuild the release candidate' "$workflow" \
  && grep -Fq 'no-cache: true' "$workflow" \
  && grep -Fq 'independent OS builds differ' "$workflow" \
  && grep -Fq 'final sealed OS images differ' "$workflow" \
  && grep -Fq 'os/dev/reseal-image.sh' "$workflow" \
  && grep -Fq 'mapfile -t reproducible < <(podman_epoch_args "$SOURCE_DATE_EPOCH" "${podman_args[@]}")' "$repo/os/dev/reseal-image.sh" \
  && grep -Fq -- "printf '%s\\n' --source-date-epoch \"\$epoch\" --rewrite-timestamp" "$repo/os/dev/_common.sh" \
  && grep -Fq -- "printf '%s\\n' --timestamp \"\$epoch\"" "$repo/os/dev/_common.sh" \
  && ok 'release compares uncached base and final sealed OS rebuilds' \
  || bad 'reproducibility is claimed without a release-blocking comparison'

# BuildKit applies SOURCE_DATE_EPOCH to file mtimes only when the exporter
# rewrites them; without it COPY keeps checkout times and RUN the wall clock.
# Only a Build / OS dev image, never compared or released, skips it: BuildKit
# pushes it straight to tilo-ci.
release_rule="$(awk '/- name: Select build, validation, or publication mode/,/dev-channel=/' "$workflow")"
grep -Fq "outputs: type=oci,dest=/var/tmp/tilo-image-repro.tar,rewrite-timestamp=true" "$workflow" \
  && [ "$(grep -Fc 'outputs: ${{ env.TILO_IMAGE_OUTPUT }}' "$workflow")" -eq 2 ] \
  && grep -Fq "echo 'TILO_IMAGE_OUTPUT=type=oci,dest=/var/tmp/tilo-image.tar,rewrite-timestamp=true' >> \"\$GITHUB_ENV\"" <<<"$release_rule" \
  && grep -Fq "if [[ \$GITHUB_EVENT_NAME == workflow_call ]]; then" <<<"$release_rule" \
  && grep -Fq '[[ $publish != true ]]' <<<"$release_rule" \
  && grep -Fq 'echo "TILO_IMAGE_TAGS=registry.tilo.so/tilo/tilo-ci:$series-${{ matrix.arch }}"' <<<"$release_rule" \
  && ok 'every compared or released OS image export clamps file mtimes to SOURCE_DATE_EPOCH' \
  || bad 'an OS image export keeps build-time file mtimes'

# Integration images skip the second uncached build, but every publication
# (tag, nightly or protected manual main release) must still run it, with no
# compiler cache (tools/tests/test_publication_compiler_cache.py).
# The rebuild runs in its own, parallel reproduction lane, which exists only in
# publication matrices and refuses to run otherwise; the publication lane waits
# for its digest before installing or signing.
grep -A1 -F -- '- name: Independently rebuild the release candidate' "$workflow" \
    | grep -Fq "if: matrix.role == 'reproduce'" \
  && grep -Fq "echo '::error::a reproduction lane runs only for a publication'" "$workflow" \
  && grep -A2 -F -- '- name: Wait for the independent rebuild' "$workflow" \
    | grep -Fq "if: steps.release-mode.outputs.publish == 'true' && matrix.role != 'reproduce'" \
  && grep -A2 -F -- '- name: Compare with the independent rebuild' "$workflow" \
    | grep -Fq "if: steps.release-mode.outputs.publish == 'true' && matrix.role != 'reproduce'" \
  && grep -Fq 'REPRO_MANIFEST: ${{ steps.reproduction.outputs.manifest }}' "$workflow" \
  && grep -Fq 'PUBLISH: ${{ steps.release-mode.outputs.publish }}' "$workflow" \
  && test "$(grep -Fc 'if [[ $PUBLISH == true ]]; then' "$workflow")" -eq 2 \
  && ok 'every publication, and only a publication, pays for the uncached rebuild' \
  || bad 'the uncached reproducibility rebuild no longer gates exactly the publications'

grep -Fq -- '--type https://tilo.sh/security/release-freshness/v1' "$workflow" \
  && grep -Fq 'release freshness attestation is absent, expired, or does not bind this release' "$workflow" \
  && grep -Fq '604800' "$freshness" \
  && ok 'channel promotion requires a bounded signed freshness statement' \
  || bad 'release freshness is unsigned, unbounded, or not promotion-blocking'

sign_line="$(grep -n 'cosign sign ' "$workflow" | head -1 | cut -d: -f1)"
promote_line="$(grep -n 'Assemble, sign, attest, and promote the OS index' "$workflow" | head -1 | cut -d: -f1)"
[ -n "$sign_line" ] && [ -n "$promote_line" ] && [ "$sign_line" -lt "$promote_line" ] \
  && ok 'channel tags are promoted only after signing and attestation' \
  || bad 'release channels can expose an unsigned candidate'

grep -Fq '. os/dev/configure-cosign-kms.sh' "$workflow" \
  && grep -Fq 'COSIGN_KEY_REF must name a non-exportable KMS/HSM key' "$repo/os/dev/configure-cosign-kms.sh" \
  && ! grep -Fq 'COSIGN_PRIVATE_KEY' "$workflow" \
  && ok 'release signing requires a non-exportable KMS/HSM key' \
  || bad 'release signing still accepts an exportable private key secret'

if grep -Fqx '        run: os/dev/require-cosign-authority.sh' "$workflow" \
   && grep -Eq 'needs: \[plan, signing-preflight(, [A-Za-z0-9_-]+)*\]' "$workflow"; then
  ok 'OS construction starts only after release KMS identity preflight'
else
  bad 'OS release construction can start before its signing authority is identity-bound'
fi

if grep -Eq -- '--tlog-upload=false|--insecure-ignore-tlog' "$workflow"; then
  bad 'OS release disables transparency-log publication or verification'
else
  ok 'release signatures and attestations require transparency-log evidence'
fi
if grep -Fq 'cosign attach sbom --type spdx --input-format json' "$workflow" \
   && [ "$(grep -Fc './os/dev/verify-release-sbom.sh "$image"' "$workflow")" -eq 3 ] \
   && ! grep -Fq -- '--type spdxjson' "$workflow"; then
  ok 'release gates verify the complete registry SBOM through logged provenance'
else
  bad 'a release path bypasses the logged SBOM digest and complete registry attachment'
fi

attestation_consumers=(
  "$repo/.forgejo/workflows/build-image.yml"
  "$repo/.forgejo/workflows/build-kernel.yml"
  "$repo/.forgejo/workflows/build-rpms.yml"
  "$repo/os/dev/build-image.sh"
)
attestation_shape_safe=1
for consumer in "${attestation_consumers[@]}"; do
  grep -Fq '[(if type == "array" then .[] else . end) | .payload' "$consumer" \
    || attestation_shape_safe=0
  grep -Fq '[.[].payload' "$consumer" && attestation_shape_safe=0
done
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

if grep -Fq "key='\${{ vars.TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY }}'" "$workflow" \
   && grep -Fq 'TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY=${{ env.TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY }}' "$workflow" \
   && grep -Fq "if [[ '\${{ steps.release-mode.outputs.publish }}' == true ]]" "$workflow" \
   && grep -Fq 'publishing requires TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY' "$workflow" \
   && grep -Fq 'TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY must be a non-placeholder 32-byte hex key' "$repo/os/Containerfile" \
   && grep -Fxq 'ARG TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY' "$repo/os/Containerfile"; then
  ok 'fresh images carry a release-validated pinned marketplace owner key'
else
  bad 'marketplace trust anchor is missing, placeholder, or not a release build input'
fi

grep -Fq 'verify_signed_generation "$current"' "$repo/os/dev/next-generation.sh" \
  && grep -Fq 'latest does not point at signed gen$current' "$repo/os/dev/next-generation.sh" \
  && grep -Fq -- '--type slsaprovenance1' "$repo/os/dev/next-generation.sh" \
  && grep -Fq -- '--predicate provenance.json --type slsaprovenance1' "$workflow" \
  && ok 'generation allocation authenticates provenance and the latest channel' \
  || bad 'generation allocation still trusts registry tags alone'

if grep -Fq 'runner":"imager-linux-x86_64"' "$workflow" \
   && grep -Fq 'runner":"imager-linux-aarch64"' "$workflow" \
   && grep -Fq 'if [[ $GITHUB_REF == refs/tags/v* ]]; then' "$workflow" \
   && grep -Fq 'runner":"runner-large"' "$workflow" \
   && grep -Fq 'runner":"runner-large-arm64"' "$workflow" \
   && grep -Fq "if: needs.plan.outputs.publish == 'true'" "$workflow" \
   && grep -Fq 'for candidate in /dev/dri/renderD*; do' "$workflow" \
   && grep -Fq 'installed-desktop gate will use the bounded QEMU TCG fallback' "$workflow" \
   && grep -Fq 'gl|2d) ./os/dev/vm.sh boot "--$TILO_VM_DISPLAY" ;;' "$workflow" \
   && grep -Fq "*) echo '::error::no installed-desktop display was selected'; exit 1 ;;" "$workflow" \
   && grep -Fq 'matrix: ${{ fromJSON(needs.plan.outputs.build-matrix) }}' "$workflow" \
   && grep -Fq 'candidate-${GITHUB_SHA}-${GITHUB_RUN_ID}-${{ github.run_attempt }}-${{ matrix.arch }}' "$workflow" \
   && grep -Fq 'expected exactly two native child descriptors' "$workflow" \
   && grep -Fq 'descriptor points to $actual_registry_arch content' "$workflow" \
   && grep -Fq 'podman manifest push --all' "$workflow" \
   && grep -Fq 'published OCI index does not contain exactly the verified native children' "$workflow" \
   && grep -Fq 'externalParameters.children == $children' "$workflow" \
   && grep -Fq 'type=gha,scope=tilo-os-${{ needs.plan.outputs.hardware-profile }}-rootfs-${{ matrix.arch }}' "$workflow" \
   && grep -Fq 'promote_immutable "$tag"' "$workflow" \
   && grep -Fq 'latest="registry.tilo.so/tilo/tilo:latest"' "$workflow" \
   && grep -Fq 'registry did not commit latest as $index_digest' "$workflow"; then
  ok 'OS publication promotes one signed two-architecture index after protected native gates'
else
  bad 'OS publication can omit a native architecture or promote child manifests independently'
fi

# The display falls back to 2D on hosts without a render node, but the
# compositor, OOBE and AT-SPI gates must stay unconditional: the display mode
# may choose how the VM boots, never which gates run.
if [ "$(grep -Fc 'TILO_VM_DISPLAY' "$workflow")" -eq 3 ] \
   && grep -Fq 'echo "TILO_VM_DISPLAY=$display" >> "$GITHUB_ENV"' "$workflow" \
   && ! grep -Fq 'OS publication requires a readable/writable DRI render node' "$workflow" \
   && grep -Fq './os/dev/vm.sh ssh /usr/libexec/tilo/native-atspi-smoke.sh oobe-actions' "$workflow" \
   && grep -Fq './os/dev/vm.sh ssh /usr/libexec/tilo/installed-atspi-apps.sh' "$workflow" \
   && grep -Fq './os/dev/vm.sh ssh /usr/libexec/tilo/native-atspi-smoke.sh desktop' "$workflow" \
   && grep -Fq './os/dev/vm.sh ssh /usr/libexec/tilo/native-atspi-smoke.sh lock' "$workflow" \
   && grep -Fq 'busctl --user status org.freedesktop.impl.portal.desktop.tilo' "$workflow"; then
  ok 'installed compositor, OOBE and AT-SPI gates run in both the virgl and 2D display modes'
else
  bad 'the display mode can bypass an installed compositor, OOBE or AT-SPI gate'
fi

durable_promotion_line="$(grep -n 'for tag in "${durable_tags\[@\]}"' "$workflow" | head -1 | cut -d: -f1)"
latest_promotion_line="$(grep -n 'latest="registry.tilo.so/tilo/tilo:latest"' "$workflow" | head -1 | cut -d: -f1)"
if [ -n "$durable_promotion_line" ] && [ -n "$latest_promotion_line" ] \
   && [ "$durable_promotion_line" -lt "$latest_promotion_line" ] \
   && grep -Fq 'bad semver/source name cannot strand a newly written genN' "$workflow" \
   && grep -Fq 'immutable release tag $tag already binds $existing' "$workflow" \
   && grep -Fq 'registry did not persist $tag as $index_digest' "$workflow"; then
  ok 'immutable release tags are idempotent and verified before latest moves'
else
  bad 'latest can move before durable release tags are safely committed'
fi

if grep -E 'uses: [^#[:space:]]+@(v[0-9]|main|master)([[:space:]#]|$)' "$workflow"; then
  bad 'OS release workflow executes a mutable third-party action reference'
else
  ok 'OS release workflow pins every third-party action by commit'
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
hex="$(printf 'a%.0s' {1..64})"
if RELEASE_DIGEST="sha256:$hex" TILO_GENERATION=42 SOURCE_DATE_EPOCH=1 \
   BASE_REF="base@sha256:$hex" RPMS_REF="rpms@sha256:$hex" \
   KERNEL_RPMS_REF="kernel@sha256:$hex" TILO_APP_COLLECTION_SHA256="$hex" \
   TILO_DESKTOP_PAYLOAD_SHA256="$hex" TILO_SBOM_SHA256="$hex" \
   GITHUB_SHA="$hex" GITHUB_RUN_ID=test \
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
   GITHUB_SHA="$hex" GITHUB_RUN_ID=test \
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
   KERNEL_RPMS_REF="kernel@sha256:$hex" GITHUB_SHA="$hex" \
   "$provenance" "$tmp/bad.json" >/dev/null 2>&1; then
  bad 'provenance accepted an unpinned build dependency'
else
  ok 'provenance refuses unpinned build dependencies'
fi

firmware="code.tilo.so/tilo/lenovo-firmware@sha256:$hex"
if RELEASE_DIGEST="sha256:$hex" TILO_GENERATION=1 SOURCE_DATE_EPOCH=1 \
   BASE_REF="base@sha256:$hex" RPMS_REF="rpms@sha256:$hex" \
   KERNEL_RPMS_REF="kernel@sha256:$hex" GITHUB_SHA="$hex" \
   TILO_HARDWARE_PROFILE=lenovo-83ed TILO_RELEASE_ARCHITECTURE=aarch64 \
   TILO_FIRMWARE_CANDIDATE="$firmware" \
   TILO_FIRMWARE_MANIFEST_SHA256="$hex" \
   "$provenance" "$tmp/lenovo-provenance.json" \
   && jq -e --arg firmware "$firmware" --arg manifest "$hex" '
     .buildDefinition.externalParameters.hardwareProfile == "lenovo-83ed"
     and .buildDefinition.externalParameters.architecture == "aarch64"
     and .buildDefinition.externalParameters.firmwareManifestSha256 == $manifest
     and any(.buildDefinition.resolvedDependencies[]; .uri == $firmware)
   ' "$tmp/lenovo-provenance.json" >/dev/null \
   && ! (RELEASE_DIGEST="sha256:$hex" TILO_GENERATION=1 SOURCE_DATE_EPOCH=1 \
         BASE_REF="base@sha256:$hex" RPMS_REF="rpms@sha256:$hex" \
         KERNEL_RPMS_REF="kernel@sha256:$hex" GITHUB_SHA="$hex" \
         TILO_HARDWARE_PROFILE=generic TILO_FIRMWARE_CANDIDATE="$firmware" \
         "$provenance" "$tmp/bad-generic-firmware.json" >/dev/null 2>&1); then
  ok 'Lenovo release provenance binds reviewed firmware and generic releases reject it'
else
  bad 'Lenovo firmware input is absent from release provenance or can enter a generic release'
fi

fex="registry.tilo.so/tilo/tilo-fex-payload-candidate@sha256:$hex"
guest="registry.tilo.so/tilo/tilo-development-windows-guest@sha256:$(printf 'b%.0s' {1..64})"
if RELEASE_DIGEST="sha256:$hex" TILO_GENERATION=1 SOURCE_DATE_EPOCH=1 \
   BASE_REF="base@sha256:$hex" RPMS_REF="rpms@sha256:$hex" \
   KERNEL_RPMS_REF="kernel@sha256:$hex" GITHUB_SHA="$hex" \
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
         KERNEL_RPMS_REF="kernel@sha256:$hex" GITHUB_SHA="$hex" \
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
