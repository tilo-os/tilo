#!/usr/bin/env bash
# Authenticate a native Tilo kernel repository before an OS build consumes it.
#
#   verify-kernel-provenance.sh <kernel-ref> <base-ref> <rpms-ref> <exact|signed-snapshot>
#
# Both modes verify the kernel's signature and its signed SLSA provenance, which
# must bind the qOS source commit, the native architecture, the kernel digest
# and the OS base. They differ only in the RPM snapshot the kernel was built
# against:
#
#   exact            the provenance names <rpms-ref> itself. Every publication
#                    uses this: a released image is built from one snapshot.
#   signed-snapshot  the provenance names <rpms-ref>, or exactly one other
#                    digest-pinned Tilo RPM snapshot that itself carries a
#                    valid signature. For unsigned integration images, whose
#                    RPM and kernel pins refresh independently.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
key="$repo/keys/cosign.pub"
die() { printf '%s\n' "$*" >&2; exit 1; }

[ "$#" -eq 4 ] || die "usage: verify-kernel-provenance.sh <kernel-ref> <base-ref> <rpms-ref> <exact|signed-snapshot>"
ref=$1 base=$2 rpms=$3 binding=$4
case "$binding" in exact|signed-snapshot) ;; *) die "RPM binding must be exact or signed-snapshot" ;; esac
case "$(uname -m)" in
  x86_64) expected_arch=amd64 ;;
  aarch64) expected_arch=arm64 ;;
  *) die "unsupported kernel architecture: $(uname -m)" ;;
esac
[[ "$ref" =~ ^code\.podesta\.ai/qos/tilo-kernel@sha256:[0-9a-f]{64}$ ]] \
  || die "no digest-pinned native Tilo kernel for $(uname -m)"

cosign verify --key "$key" "$ref" >/dev/null || die "Tilo kernel signature verification failed"
provenance="$(cosign verify-attestation --key "$key" --type slsaprovenance1 "$ref")" \
  || die "Tilo kernel provenance verification failed"

# The kernel's own identity: build type, source, architecture, digest and base.
# Collect every RPM snapshot that such a provenance statement binds.
bound="$(printf '%s' "$provenance" | jq -r \
  --arg digest "${ref##*@}" --arg architecture "$(uname -m)" --arg base "$base" '
    [(if type == "array" then .[] else . end) | .payload | @base64d | fromjson | .predicate
      | select(
          ((.buildDefinition.buildType == "https://tilo.sh/buildtypes/kernel-rpm/v1")
            or (.buildDefinition.buildType | endswith("/buildtypes/kernel-rpm/v1")))
          and .buildDefinition.externalParameters.architecture == $architecture
          and .buildDefinition.internalParameters.expectedReleaseDigest == $digest
          and any(.buildDefinition.resolvedDependencies[]?;
            .uri | test("^git\\+ssh://code\\.podesta\\.ai/qos/qos@[0-9a-f]{40}$"))
          and any(.buildDefinition.resolvedDependencies[]?; .uri == $base))
      | .buildDefinition.resolvedDependencies[]?.uri
      | select(test("^code\\.podesta\\.ai/qos/rpms@sha256:[0-9a-f]{64}$"))]
    | unique | .[]')"
[ -n "$bound" ] || die "Tilo kernel provenance does not bind its source, architecture, digest, base, and RPM inputs"

if ! grep -Fxq -- "$rpms" <<<"$bound"; then
  [ "$binding" = signed-snapshot ] \
    || die "Tilo kernel provenance binds RPM snapshot $(tr '\n' ' ' <<<"$bound")instead of $rpms"
  [ "$(wc -l <<<"$bound")" -eq 1 ] \
    || die "Tilo kernel provenance binds more than one RPM snapshot: $(tr '\n' ' ' <<<"$bound")"
  cosign verify --key "$key" "$bound" >/dev/null \
    || die "the RPM snapshot $bound bound by the Tilo kernel is not signed"
  printf 'Tilo kernel is bound to the earlier signed RPM snapshot %s\n' "$bound" >&2
fi

actual_arch="$(skopeo inspect "docker://$ref" | jq -er .Architecture)"
[ "$actual_arch" = "$expected_arch" ] \
  || die "signed Tilo kernel architecture is $actual_arch, expected $expected_arch"
