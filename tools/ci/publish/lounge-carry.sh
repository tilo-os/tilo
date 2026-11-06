#!/usr/bin/env bash
# The Lounge image a release publishes: the image `publish-site` built from this commit and
# passed through its gate (two headless browsers joining, chatting, sharing a desk)
# — registry.tilo.so/tilo/lounge:dev-<commit> — pulled with no credentials and pinned by digest.
# A release signs and publishes that image instead of building a second one beside it, so no
# Lounge ships that the gate did not use. It says which image it carried, so a release that
# needed a publish-site run for its commit first says so when it fails.
#
# The image is taken by the tag this commit's name, and its revision label must say the same
# commit: a mis-tagged image is refused. Writes OUT/image.oci.tar and OUT/digest, which
# sign-lounge signs and publish-lounge publishes as sha-<commit>.
#
# It reads the registry and holds no credentials, so it runs every time (cache = false).
# usage: lounge-carry.sh OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
out=${1:?usage: lounge-carry.sh OUT}
repository=registry.tilo.so/tilo/lounge
require_commit
tag=dev-$T_COMMIT
anonymous=$scratch/anonymous-auth.json
printf '{}' > "$anonymous"
inspect_image() { skopeo inspect --authfile "$anonymous" --format "$1" "docker://$repository:$tag"; }
digest=$(inspect_image '{{.Digest}}') \
  || die "the registry has no $repository:$tag: no publish-site run has published $T_COMMIT"
[[ $digest =~ ^sha256:[0-9a-f]{64}$ ]] || die "$repository:$tag names no digest: $digest"
revision=$(inspect_image '{{index .Labels "org.opencontainers.image.revision"}}')
[[ $revision == "$T_COMMIT" ]] || die "$repository:$tag was built from $revision, not $T_COMMIT"
printf 'carrying %s (%s) at %s\n' "$repository:$tag" "$revision" "$digest"
rm -rf "$out"
mkdir -p "$out"
skopeo copy --preserve-digests --retry-times 3 --authfile "$anonymous" \
  "docker://$repository@$digest" "oci-archive:$out/image.oci.tar"
carried=$(archive_digest "$out/image.oci.tar")
[[ $carried == "$digest" ]] || die "$out/image.oci.tar is $carried, not the $digest the registry serves"
printf '%s\n' "$digest" > "$out/digest"
printf 'carried %s\n' "$digest"
