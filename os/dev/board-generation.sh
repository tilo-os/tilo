#!/bin/bash
# The Lenovo 83ED's dense generation series in code.tilo.so/tilo/tilo (lenovo-83ed-gen<N>,
# lenovo-83ed-latest; next-generation.sh verifies it).
#
#   board-generation.sh           the generation the board's next release carries; reads the
#                                 registry only
#   board-generation.sh --retain  before that release is published, with registry credentials: an
#                                 empty series gets its seed as lenovo-83ed-gen1 and -latest
#
# The Lenovo images published before the series existed all carry generation 1, and a device
# refuses a successor of the generation it runs. So an empty series starts from a seed, one of
# those images by digest (TILO_LENOVO_SERIES_SEED in os/publication.env, or
# TILO_BOARD_SERIES_SEED), verified as this board's signed generation 1, and the first release is
# generation 2. Never a tag order, a commit count or a timestamp.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export TILO_PUBLISH_REPO=code.tilo.so/tilo/tilo
export TILO_GENERATION_NAMESPACE=lenovo-83ed
key="${TILO_GENERATION_VERIFY_KEY:-keys/cosign.pub}"
retain=0
case "${1:-}" in
  '') ;;
  --retain) retain=1 ;;
  *) echo 'usage: board-generation.sh [--retain]' >&2; exit 2 ;;
esac
current="$("$HERE/next-generation.sh" --current)"
if [[ $current != 0 ]]; then
  (( retain )) && exit 0
  exec "$HERE/next-generation.sh"
fi

seed="${TILO_BOARD_SERIES_SEED-$(sed -n 's/^TILO_LENOVO_SERIES_SEED=//p' "$HERE/../publication.env")}"
[[ $seed =~ ^code\.tilo\.so/tilo/tilo@sha256:[0-9a-f]{64}$ ]] || {
  echo 'board generation: an empty series needs an immutable Lenovo generation-1 seed' >&2
  exit 1
}
digest="${seed##*@}"
cosign verify --key "$key" "$seed" >/dev/null
provenance="$(cosign verify-attestation --key "$key" --type slsaprovenance1 "$seed")"
# The seed was released before t built the OS: by the forge's release workflow, and before
# provenance named the hardware definition.
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
        and (.buildDefinition.externalParameters.firmwareManifestSha256 | test("^[0-9a-f]{64}$"))
        and any(.buildDefinition.resolvedDependencies[]?;
          .uri | test("^git\\+ssh://code.tilo.so/tilo/tilo@[0-9a-f]{40}$"))
        and any(.buildDefinition.resolvedDependencies[]?;
          .uri | test("^code.tilo.so/tilo/tilo-lenovo-83ed-firmware@sha256:[0-9a-f]{64}$")))
' >/dev/null

if (( ! retain )); then
  echo 2
  exit 0
fi
# The caller publishes the release right after; the seed's tags never move once set.
for target in lenovo-83ed-gen1 lenovo-83ed-latest; do
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
    echo "board generation: $target conflicts with the seed" >&2
    exit 1
  }
  if [[ -z $existing ]]; then
    skopeo copy --preserve-digests "docker://$seed" "docker://$TILO_PUBLISH_REPO:$target"
  fi
done
