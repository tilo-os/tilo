#!/bin/bash
# Verify the snapshot pins a pull request changes, against this checkout.
#
#   os/dev/verify-snapshot-pins.sh os/rpms-snapshot.env [os/kernel-snapshot.env]
#
# Each argument names a pin file the pull request changed; only those pins
# are verified, because a pin the change does not touch is main's business.
#
# The pin-only fast path of check.yml runs this instead of the Rust and
# security suites: a pull request that changes only os/rpms-snapshot.env
# and/or os/kernel-snapshot.env changes no code, so what needs proving is that
# each pinned reference is the one CI published and signed for this source.
#
#   os/rpms-snapshot.env    RPMS_REF is signed with keys/cosign.pub, its signed
#                           SLSA provenance binds this checkout's package set
#                           (both architectures and each one, derived from the
#                           Containerfiles), its digest and os/base-snapshot.env's
#                           base, and the index holds exactly amd64 and arm64.
#   os/kernel-snapshot.env  each architecture's kernel passes
#                           os/dev/verify-kernel-provenance.sh.
#
# Needs cosign, skopeo, jq and registry read access. Exit 0 when every pin
# verifies, 1 otherwise.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo"
fail() { printf 'verify-snapshot-pins: %s\n' "$*" >&2; exit 1; }
[ "$#" -gt 0 ] || fail "name the changed pin files"
rpms=0 kernel=0
for pin in "$@"; do
  case "$pin" in
    os/rpms-snapshot.env) rpms=1 ;;
    os/kernel-snapshot.env) kernel=1 ;;
    *) fail "not a pin this check verifies: $pin" ;;
  esac
done
for tool in cosign skopeo jq; do
  command -v "$tool" >/dev/null || fail "$tool is required"
done

if [ "$rpms" = 1 ]; then
  # shellcheck disable=SC1091
  . os/base-snapshot.env
  # shellcheck disable=SC1091
  . os/rpms-snapshot.env
  [[ "${RPMS_REF:-}" =~ ^code\.podesta\.ai/qos/rpms@sha256:[0-9a-f]{64}$ ]] \
    || fail "RPMS_REF is not the digest-pinned Tilo RPM repository"
  cosign verify --key keys/cosign.pub "$RPMS_REF" >/dev/null || fail "RPMS_REF signature does not verify"
  work="$(mktemp -d)"
  trap 'rm -rf -- "$work"' EXIT
  ./os/dev/derive-rpm-package-set.py --arch x86_64 | LC_ALL=C sort -u > "$work/amd64.txt"
  ./os/dev/derive-rpm-package-set.py --arch aarch64 | LC_ALL=C sort -u > "$work/arm64.txt"
  package_set_sha256="$(cat "$work/amd64.txt" "$work/arm64.txt" | LC_ALL=C sort -u | sha256sum | cut -d' ' -f1)"
  package_amd64_sha256="$(sha256sum "$work/amd64.txt" | cut -d' ' -f1)"
  package_arm64_sha256="$(sha256sum "$work/arm64.txt" | cut -d' ' -f1)"
  cosign verify-attestation --key keys/cosign.pub --type slsaprovenance1 "$RPMS_REF" \
    | jq -e --arg digest "${RPMS_REF##*@}" --arg base "$BASE_REF" \
        --arg package_set_sha256 "$package_set_sha256" \
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
        )' >/dev/null \
    || fail "RPMS_REF provenance does not bind this checkout's package set, digest, base and architectures"
  platforms="$(skopeo inspect --raw "docker://$RPMS_REF" | jq -c \
    '[.manifests[].platform | {os,architecture}] | sort_by(.architecture)')"
  [ "$platforms" = '[{"os":"linux","architecture":"amd64"},{"os":"linux","architecture":"arm64"}]' ] \
    || fail "RPMS_REF index has unexpected platforms: $platforms"
  echo "verify-snapshot-pins: RPMS_REF $RPMS_REF is signed and binds this checkout's package set"
fi

[ "$kernel" = 1 ] || exit 0
for key in KERNEL_RPMS_X86_64_REF KERNEL_RPMS_AARCH64_REF; do
  ref="$(sed -n "s/^$key=//p" os/kernel-snapshot.env)"
  [ -n "$ref" ] || continue
  ./os/dev/verify-kernel-provenance.sh "$ref" >/dev/null || fail "$key=$ref does not verify"
  echo "verify-snapshot-pins: $key $ref verifies"
done
