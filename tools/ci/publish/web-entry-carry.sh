#!/usr/bin/env bash
# The web entry image a release publishes: the build in BUILD (image-web-entry's, tested there)
# carrying the version of the image production serves now, so tabs open on that version survive
# the deploy (crates/browser/tilo-browser-runtime/container/carry.sh). Writes OUT/image.oci.tar
# and OUT/digest, which sign-web-entry signs and publish-web-entry publishes from stable, and
# OUT/receipt-extra.json, which adds to the receipt the image this one replaces and the version
# it carries.
#
# The outgoing image is the one production serves while this runs: tilo.coffee's version.json
# names its revision, whose image is registry.tilo.so/tilo/web-entry:sha-<revision>, pulled
# anonymously and pinned by digest. One that can't be observed fails the step: publishing
# without its version would strand every open tab at the next deploy. The carried image is
# tested against it before anything signs it.
#
# It holds no credentials and observes production, so it runs every time (cache = false), also
# for a working tree: `t test carry-web-entry`.
# usage: web-entry-carry.sh BUILD OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
build=${1:?usage: web-entry-carry.sh BUILD OUT}
out=${2:?}
repository=registry.tilo.so/tilo/web-entry
live=https://tilo.coffee/version.json
container=$repo_root/crates/browser/tilo-browser-runtime/container

revision=$(curl -fsS --retry 3 --max-time 30 -H 'Cache-Control: no-store' "$live" \
  | python3 -c 'import json, sys; print(json.load(sys.stdin).get("revision", ""))') \
  || die "could not read $live"
[[ $revision =~ ^[0-9a-f]{40}$ ]] || die "$live names no revision: $revision"
use_buildah web-entry overlay
# No credentials: the registry serves its images to anyone.
b pull --quiet "docker://$repository:sha-$revision" >/dev/null
digest=$(b images --digests --format '{{.Digest}}' "$repository:sha-$revision" | head -1)
[[ $digest =~ ^sha256:[0-9a-f]{64}$ ]] || die "$repository:sha-$revision has no digest: $digest"
outgoing=docker://$repository@$digest
printf 'replaces %s@%s (production serves %s)\n' "$repository" "$digest" "$revision"

carried=$(bash "$container/carry.sh" "$build/image.oci.tar" "$outgoing" "$out")
bash "$container/test-image.sh" "$out/image.oci.tar" "$outgoing"
version=null
[[ -z $carried ]] || version="\"$carried\""
printf '{"replaces":"%s@%s","carriedVersion":%s}\n' "$repository" "$digest" "$version" > "$out/receipt-extra.json"
printf 'carried %s into %s\n' "${carried:-nothing (the same version)}" "$(cat "$out/digest")"
