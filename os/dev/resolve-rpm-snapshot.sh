#!/usr/bin/env bash
# Resolve this checkout's frozen RPM snapshot by package-set fingerprint.
#
# Looks up registry.tilo.so/tilo/rpms:set-<fingerprint>, runs the signature,
# provenance and both-architecture checks that published image builds require,
# and prints the digest-pinned RPMS_REF. Never reads a hand-kept pin file.
#
#   resolve-rpm-snapshot.sh           print registry.../rpms@sha256:...
#   resolve-rpm-snapshot.sh --exists  exit 0 if the fingerprint tag resolves, 1 if
#                                     it does not exist, 3 if the registry failed
#   resolve-rpm-snapshot.sh --tag     print set-<fingerprint> only
#
# Needs: python3, cosign, skopeo, jq. Registry read access for the tag.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo"

mode=resolve
case "${1:-}" in
  "") ;;
  --exists) mode=exists ;;
  --tag) mode=tag ;;
  -h|--help)
    sed -n '2,12p' "$0"
    exit 0
    ;;
  *)
    printf 'resolve-rpm-snapshot: unknown option %s\n' "$1" >&2
    exit 2
    ;;
esac

fingerprint="$(python3 "$repo/os/dev/rpm-snapshot-fingerprint.py")"
[[ "$fingerprint" =~ ^[0-9a-f]{64}$ ]] \
  || { printf 'resolve-rpm-snapshot: invalid fingerprint\n' >&2; exit 1; }
tag="set-$fingerprint"
tagged="registry.tilo.so/tilo/rpms:$tag"
TMPDIR_RESOLVE="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_RESOLVE"' EXIT

if [ "$mode" = tag ]; then
  printf '%s\n' "$tag"
  exit 0
fi

# Without skopeo the lookup fails with "not found", which must not read as
# "not published".
command -v skopeo >/dev/null \
  || { printf 'resolve-rpm-snapshot: skopeo is required\n' >&2; exit 3; }

# Exit 1: the tag does not exist. Exit 3: the registry could not be asked, which
# says nothing about the tag; a caller must not treat it as "not published".
if ! digest="$(skopeo inspect --format '{{.Digest}}' "docker://$tagged" 2>"$TMPDIR_RESOLVE/err")"; then
  if grep -qE 'manifest unknown|name unknown|not found' "$TMPDIR_RESOLVE/err"; then
    printf 'resolve-rpm-snapshot: no published snapshot for %s\n' "$tagged" >&2
    exit 1
  fi
  printf 'resolve-rpm-snapshot: could not look up %s: %s\n' "$tagged" "$(tail -n 1 "$TMPDIR_RESOLVE/err")" >&2
  exit 3
fi
[[ "$digest" =~ ^sha256:[0-9a-f]{64}$ ]] \
  || { printf 'resolve-rpm-snapshot: tag %s has no digest\n' "$tagged" >&2; exit 1; }
ref="registry.tilo.so/tilo/rpms@$digest"

if [ "$mode" = exists ]; then
  exit 0
fi

# shellcheck disable=SC1091
. "$repo/os/base-snapshot.env"
[[ "${BASE_REF:-}" =~ ^registry\.tilo\.so/tilo/fedora-bootc@sha256:[0-9a-f]{64}$ ]] \
  || { printf 'resolve-rpm-snapshot: BASE_REF is not a digest-pinned fedora-bootc\n' >&2; exit 1; }

for tool in cosign jq; do
  command -v "$tool" >/dev/null \
    || { printf 'resolve-rpm-snapshot: %s is required\n' "$tool" >&2; exit 1; }
done

cosign verify --key "$repo/keys/cosign.pub" "$ref" >/dev/null \
  || { printf 'resolve-rpm-snapshot: signature does not verify for %s\n' "$ref" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf -- "$work"' EXIT
python3 "$repo/os/dev/derive-rpm-package-set.py" --arch x86_64 | LC_ALL=C sort -u > "$work/amd64.txt"
python3 "$repo/os/dev/derive-rpm-package-set.py" --arch aarch64 | LC_ALL=C sort -u > "$work/arm64.txt"
package_set_sha256="$(cat "$work/amd64.txt" "$work/arm64.txt" | LC_ALL=C sort -u | sha256sum | cut -d' ' -f1)"
package_amd64_sha256="$(sha256sum "$work/amd64.txt" | cut -d' ' -f1)"
package_arm64_sha256="$(sha256sum "$work/arm64.txt" | cut -d' ' -f1)"

provenance="$(cosign verify-attestation --key "$repo/keys/cosign.pub" \
  --type slsaprovenance1 "$ref")" \
  || { printf 'resolve-rpm-snapshot: provenance verification failed for %s\n' "$ref" >&2; exit 1; }

printf '%s' "$provenance" | jq -e --arg digest "$digest" \
  --arg base "$BASE_REF" --arg package_set_sha256 "$package_set_sha256" \
  --arg package_amd64_sha256 "$package_amd64_sha256" \
  --arg package_arm64_sha256 "$package_arm64_sha256" '
    [(if type == "array" then .[] else . end) | .payload | @base64d | fromjson | .predicate]
    | any(
        ((.buildDefinition.buildType == "https://tilo.sh/buildtypes/frozen-rpm-index/v1")
          or (.buildDefinition.buildType | endswith("/buildtypes/frozen-rpm-index/v1")))
        and .buildDefinition.externalParameters.architectures == ["amd64", "arm64"]
        and .buildDefinition.externalParameters.packageSetSha256 == $package_set_sha256
        and .buildDefinition.externalParameters.packageSetSha256ByArchitecture
          == {amd64: $package_amd64_sha256, arm64: $package_arm64_sha256}
        and .buildDefinition.internalParameters.expectedReleaseDigest == $digest
        and any(.buildDefinition.resolvedDependencies[]?; .uri == $base)
      )
  ' >/dev/null \
  || {
    printf 'resolve-rpm-snapshot: frozen RPM provenance does not bind its package set, digest, base, and architectures\n' >&2
    exit 1
  }

platforms="$(skopeo inspect --raw "docker://$ref" | jq -c \
  '[.manifests[].platform | {os,architecture}] | sort_by(.architecture)')"
[ "$platforms" = '[{"os":"linux","architecture":"amd64"},{"os":"linux","architecture":"arm64"}]' ] \
  || {
    printf 'resolve-rpm-snapshot: frozen RPM index has unexpected platforms: %s\n' "$platforms" >&2
    exit 1
  }

printf '%s\n' "$ref"
if [ -n "${GITHUB_OUTPUT:-}" ]; then
  {
    printf 'rpms_ref=%s\n' "$ref"
    printf 'fingerprint=%s\n' "$fingerprint"
    printf 'tag=%s\n' "$tag"
  } >> "$GITHUB_OUTPUT"
fi
