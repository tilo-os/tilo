#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 2 ]]; then
  echo "usage: $0 BUILDER BASE_REF" >&2
  exit 2
fi

builder=$1
base_ref=$2

[[ -n "$builder" ]] || { echo 'BuildKit builder name is empty' >&2; exit 2; }
[[ "$base_ref" =~ ^code\.podesta\.ai/qos/fedora-bootc@sha256:[0-9a-f]{64}$ ]] || {
  echo 'BASE_REF must be the digest-pinned Fedora bootc mirror' >&2
  exit 2
}

probe=$(mktemp -d)
trap 'rm -rf "$probe"' EXIT
printf 'ARG BASE_REF\nFROM ${BASE_REF}\n' > "$probe/Containerfile"

# BuildKit does not retry a transient registry metadata response before failing
# the whole image build. Resolve the exact immutable base through the same
# builder first so a short Forgejo registry outage receives bounded backoff and
# the successful result is already in that builder's content store. Never fall
# back to a tag or a different registry: every attempt uses the pinned digest.
for attempt in 1 2 3; do
  if docker buildx build \
      --builder "$builder" \
      --progress plain \
      --file "$probe/Containerfile" \
      --build-arg "BASE_REF=$base_ref" \
      "$probe"; then
    exit 0
  fi

  if [[ $attempt == 3 ]]; then
    echo "::error::digest-pinned relay base remained unavailable after $attempt attempts" >&2
    exit 1
  fi
  delay=$((attempt * 2))
  echo "::notice::digest-pinned relay base lookup failed; retrying in ${delay}s ($attempt/2)" >&2
  sleep "$delay"
done
