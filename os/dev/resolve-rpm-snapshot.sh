#!/usr/bin/env bash
# Resolve this checkout's frozen RPM snapshot for an architecture by package-set fingerprint.
#
# Looks up registry.tilo.so/tilo/rpms:set-<fingerprint>-<arch>, runs the signature,
# provenance and architecture checks that published image builds require, and prints
# the digest-pinned RPMS_REF. Never reads a hand-kept pin file. Each architecture's set
# is published on its own: a machine resolves the set it builds for, and needs no
# build node of the other architecture to exist.
#
#   resolve-rpm-snapshot.sh [--arch ARCH]           print registry.../rpms@sha256:...
#   resolve-rpm-snapshot.sh --exists [--arch ARCH]  exit 0 if the tag resolves, 1 if
#                                                   it does not exist, 3 if the
#                                                   registry failed
#   resolve-rpm-snapshot.sh --tag [--arch ARCH]     print set-<fingerprint>-<arch>
#
# ARCH defaults to this machine's (`uname -m`): a build resolves the set of the
# architecture it builds for.
#
# Needs: python3, cosign, jq; skopeo if installed. Registry read access for the tag.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo"

mode=resolve
arch=$(uname -m)
while (($#)); do
  case $1 in
    --exists) mode=exists ;;
    --tag) mode=tag ;;
    --arch)
      arch=${2:?--arch needs an architecture}
      shift ;;
    --arch=*)
      arch=${1#--arch=} ;;
    -h | --help)
      sed -n '2,19p' "$0"
      exit 0
      ;;
    *)
      printf 'resolve-rpm-snapshot: unknown option %s\n' "$1" >&2
      exit 2
      ;;
  esac
  shift
done
case $arch in
  x86_64 | amd64) arch=x86_64; platform_arch=amd64 ;;
  aarch64 | arm64) arch=aarch64; platform_arch=arm64 ;;
  *)
    printf 'resolve-rpm-snapshot: unknown architecture %s\n' "$arch" >&2
    exit 2
    ;;
esac

fingerprint="$(python3 "$repo/os/dev/rpm-snapshot-fingerprint.py")"
[[ "$fingerprint" =~ ^[0-9a-f]{64}$ ]] \
  || { printf 'resolve-rpm-snapshot: invalid fingerprint\n' >&2; exit 1; }
tag="set-$fingerprint-$arch"
tagged="registry.tilo.so/tilo/rpms:$tag"
TMPDIR_RESOLVE="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_RESOLVE"' EXIT

if [ "$mode" = tag ]; then
  printf '%s\n' "$tag"
  exit 0
fi

# The manifest digest and the manifest of an image reference: skopeo where it is installed,
# else an anonymous reader (os/dev/registry-manifest.py) whose errors read the same.
manifest_digest() {
  if command -v skopeo >/dev/null; then
    skopeo inspect --format '{{.Digest}}' "docker://$1"
  else
    python3 "$repo/os/dev/registry-manifest.py" digest "$1"
  fi
}
manifest_raw() {
  if command -v skopeo >/dev/null; then
    skopeo inspect --raw "docker://$1"
  else
    python3 "$repo/os/dev/registry-manifest.py" raw "$1"
  fi
}
manifest_config() {
  if command -v skopeo >/dev/null; then
    skopeo inspect --config "docker://$1"
  else
    python3 "$repo/os/dev/registry-manifest.py" config "$1"
  fi
}

# Exit 1: the tag does not exist. Exit 3: the registry could not be asked, which
# says nothing about the tag; a caller must not treat it as "not published".
if ! digest="$(manifest_digest "$tagged" 2>"$TMPDIR_RESOLVE/err")"; then
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
trap 'rm -rf -- "$TMPDIR_RESOLVE" "$work"' EXIT
python3 "$repo/os/dev/derive-rpm-package-set.py" --arch "$arch" | LC_ALL=C sort -u > "$work/set.txt"
package_set_sha256="$(sha256sum "$work/set.txt" | cut -d' ' -f1)"

provenance="$(cosign verify-attestation --key "$repo/keys/cosign.pub" \
  --type slsaprovenance1 "$ref")" \
  || { printf 'resolve-rpm-snapshot: provenance verification failed for %s\n' "$ref" >&2; exit 1; }

printf '%s' "$provenance" | jq -e --arg digest "$digest" \
  --arg base "$BASE_REF" --arg arch "$platform_arch" \
  --arg package_set_sha256 "$package_set_sha256" '
    [(if type == "array" then .[] else . end) | .payload | @base64d | fromjson | .predicate]
    | any(
        ((.buildDefinition.buildType == "https://tilo.sh/buildtypes/frozen-rpm-snapshot/v1")
          or (.buildDefinition.buildType | endswith("/buildtypes/frozen-rpm-snapshot/v1")))
        and .buildDefinition.externalParameters.architecture == $arch
        and .buildDefinition.externalParameters.packageSetSha256 == $package_set_sha256
        and .buildDefinition.internalParameters.expectedReleaseDigest == $digest
        and any(.buildDefinition.resolvedDependencies[]?; .uri == $base)
      )
  ' >/dev/null \
  || {
    printf 'resolve-rpm-snapshot: frozen RPM provenance does not bind its architecture, package set, digest and base\n' >&2
    exit 1
  }

image_config="$(manifest_config "$ref")" \
  || { printf 'resolve-rpm-snapshot: could not read the image config of %s\n' "$ref" >&2; exit 1; }
image_platform="$(printf '%s' "$image_config" | jq -r '"\(.os)/\(.architecture)"')"
[ "$image_platform" = "linux/$platform_arch" ] \
  || {
    printf 'resolve-rpm-snapshot: %s is a %s image, not linux/%s\n' "$ref" "${image_platform:-unreadable}" "$platform_arch" >&2
    exit 1
  }

printf '%s\n' "$ref"
if [ -n "${GITHUB_OUTPUT:-}" ]; then
  {
    printf 'rpms_ref=%s\n' "$ref"
    printf 'fingerprint=%s\n' "$fingerprint"
    printf 'tag=%s\n' "$tag"
    printf 'arch=%s\n' "$arch"
  } >> "$GITHUB_OUTPUT"
fi
