#!/usr/bin/env bash
# Ensure the digest-pinned Fedora bootc base is present in registry.tilo.so.
#
# os/base-snapshot.env stays a digest pin. When that digest is missing from the
# mirror, copy BASE_SOURCE across with digests preserved. A BASE_SOURCE that no
# longer carries the pinned digest fails closed — bump BASE_REF (one line) and
# the RPM fingerprint follows.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo"
# shellcheck disable=SC1091
. "$repo/os/base-snapshot.env"

[[ "${BASE_REF:-}" =~ ^registry\.tilo\.so/tilo/fedora-bootc@sha256:[0-9a-f]{64}$ ]] \
  || { printf 'ensure-base-mirror: BASE_REF is not a digest-pinned fedora-bootc\n' >&2; exit 1; }
[[ "${BASE_SOURCE:-}" =~ ^quay\.io/fedora/fedora-bootc:[A-Za-z0-9._-]+$ ]] \
  || { printf 'ensure-base-mirror: BASE_SOURCE is not a quay.io fedora-bootc tag\n' >&2; exit 1; }

want="${BASE_REF##*@}"
if skopeo inspect --raw "docker://$BASE_REF" >/dev/null 2>&1; then
  printf 'ensure-base-mirror: %s already present\n' "$BASE_REF"
  exit 0
fi

command -v skopeo >/dev/null \
  || { printf 'ensure-base-mirror: skopeo is required\n' >&2; exit 1; }

dst="registry.tilo.so/tilo/fedora-bootc:${BASE_SOURCE##*:}-recover-${want#sha256:}"
dst="${dst:0:128}"
printf 'ensure-base-mirror: mirroring %s -> %s\n' "$BASE_SOURCE" "$dst" >&2
skopeo copy --all --preserve-digests --retry-times 5 \
  "docker://$BASE_SOURCE" "docker://$dst"

got="$(skopeo inspect --raw "docker://$dst" | skopeo manifest-digest /dev/stdin 2>/dev/null \
  || skopeo inspect --format '{{.Digest}}' "docker://$dst")"
# skopeo manifest-digest may need a file; fall back to inspect format.
if [[ ! "$got" =~ ^sha256:[0-9a-f]{64}$ ]]; then
  got="$(skopeo inspect --format '{{.Digest}}' "docker://$dst")"
fi
[ "$got" = "$want" ] \
  || {
    printf 'ensure-base-mirror: BASE_SOURCE resolved to %s, not pinned %s; bump BASE_REF\n' \
      "$got" "$want" >&2
    exit 1
  }

skopeo inspect --raw "docker://$BASE_REF" >/dev/null \
  || { printf 'ensure-base-mirror: pin %s still missing after mirror\n' "$BASE_REF" >&2; exit 1; }
printf 'ensure-base-mirror: recovered %s\n' "$BASE_REF"
