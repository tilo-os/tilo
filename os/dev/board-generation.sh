#!/bin/bash
# Resolve the Lenovo series; bootstrap only from a reviewed immutable gen1.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export TILO_PUBLISH_REPO=code.tilo.so/tilo/tilo
export TILO_GENERATION_NAMESPACE=lenovo-83ed
key="${TILO_GENERATION_VERIFY_KEY:-keys/cosign.pub}"
current="$("$HERE/next-generation.sh" --current)"
if [[ $current != 0 ]]; then
  exec "$HERE/next-generation.sh"
fi

# This is an operator-selected historical image, not a tag-order heuristic.
seed="${TILO_BOARD_SERIES_SEED:-}"
[[ $seed =~ ^code\.tilo\.so/tilo/tilo@sha256:[0-9a-f]{64}$ ]] || {
  echo 'board generation: an explicit immutable Lenovo generation-1 seed is required' >&2
  exit 1
}
digest="${seed##*@}"
cosign verify --key "$key" "$seed" >/dev/null
provenance="$(cosign verify-attestation --key "$key" --type slsaprovenance1 "$seed")"
printf '%s' "$provenance" | jq -e --arg digest "$digest" '
  [(if type == "array" then .[] else . end) | .payload | @base64d | fromjson | .predicate]
  | any(.buildDefinition.buildType == "https://tilo.sh/buildtypes/bootc-os/v1"
        and .runDetails.builder.id == "https://code.tilo.so/tilo/forgejo/actions/bootc-release-v1"
        and any(.runDetails.byproducts[]?;
          .name == "independent-rebuild" and .uri == "urn:tilo:verification:manifest-digest-equal")
        and .buildDefinition.internalParameters.expectedReleaseDigest == $digest
        and .buildDefinition.externalParameters.generation == 1
        and .buildDefinition.externalParameters.hardwareProfile == "lenovo-83ed"
        and .buildDefinition.externalParameters.architecture == "aarch64"
        and (.buildDefinition.externalParameters.hardwareDefinitionSha256 | test("^[0-9a-f]{64}$"))
        and (.buildDefinition.externalParameters.firmwareManifestSha256 | test("^[0-9a-f]{64}$"))
        and any(.buildDefinition.resolvedDependencies[]?;
          .uri | test("^git\\+ssh://code.tilo.so/tilo/tilo@[0-9a-f]{40}$"))
        and any(.buildDefinition.resolvedDependencies[]?;
          .uri | test("^code.tilo.so/tilo/tilo-lenovo-83ed-firmware@sha256:[0-9a-f]{64}$")))
' >/dev/null

# The caller's board publication lock spans bootstrap, build and promotion.
# Retain the immutable signed predecessor as gen1 before allocating gen2.
for target in lenovo-83ed-latest lenovo-83ed-gen1; do
  existing=''
  error="$(mktemp)"
  manifest="$(mktemp)"
  if skopeo inspect --raw "docker://$TILO_PUBLISH_REPO:$target" >"$manifest" 2>"$error"; then
    existing="sha256:$(sha256sum "$manifest" | cut -d' ' -f1)"
  elif ! grep -Eq 'manifest unknown|MANIFEST_UNKNOWN|name unknown|NAME_UNKNOWN' "$error"; then
    cat "$error" >&2
    rm -f "$error" "$manifest"
    exit 1
  fi
  rm -f "$error" "$manifest"
  [[ -z $existing || $existing == "$digest" ]] || {
    echo "board generation: $target conflicts with the reviewed seed" >&2
    exit 1
  }
  if [[ -z $existing ]]; then
    skopeo copy --preserve-digests "docker://$seed" "docker://$TILO_PUBLISH_REPO:$target"
  fi
done
exec "$HERE/next-generation.sh"
