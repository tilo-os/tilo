#!/usr/bin/env bash
# The build half of the relay image (os/containers/tilo-relay/Containerfile): the relay and the
# filesystem replica compiled from this tree on the mirrored base, installed from the frozen RPM
# snapshot rpm-snapshot-ref.sh named in SNAPSHOT (the published one, or this run's
# image-rpms-x86_64 in RPMS where it is not published yet), into OUT (oci-build.sh), with the
# snapshot it came from in OUT/rpms-source, which oci-sign.sh refuses unless it is the published one.
#
# usage: relay-build.sh SNAPSHOT RPMS OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
snapshot=${1:?usage: relay-build.sh SNAPSHOT RPMS OUT}
rpms=${2:?}
out=${3:?}
cd "$repo_root"
bash os/containers/tilo-relay/test-contract.sh
. os/base-snapshot.env
[[ ${BASE_REF:-} =~ ^registry\.tilo\.so/tilo/fedora-bootc@sha256:[0-9a-f]{64}$ ]] \
  || die 'BASE_REF is not a digest-pinned fedora-bootc'
# oci-build.sh builds on this store, so an unpublished snapshot loaded here is the one FROM finds.
use_buildah buildah overlay
snapshot_for_build "$snapshot" "$rpms" x86_64
# The package manifest inside a published snapshot must name what this tree's Containerfiles
# install, not only its signed provenance: a stale snapshot fails here, before the compile. The
# run's own image-rpms was built from these Containerfiles.
if [[ $rpms_source == "$rpms_ref" ]]; then
  python3 os/dev/verify-rpm-snapshot-buildkit.py "$rpms_ref"
fi
assets=$(pinned_assets sccache-v0.17.0-x86_64-unknown-linux-musl.tar.gz)
# Source bytes and the pinned base and snapshot decide the image; a commit time is no input:
# the Containerfile's SOURCE_DATE_EPOCH defaults to 0, and oci-build.sh fixes the timestamps
# (buildah refuses a SOURCE_DATE_EPOCH argument beside them).
# The Containerfile binds its context to prove the ignore file kept checkout metadata out; buildah
# does not filter that bind, so the build gets a filtered copy.
context=$scratch/relay-context
stage_context os/containers/tilo-relay/Containerfile.dockerignore "$context"
bash tools/ci/publish/oci-build.sh "$context" os/containers/tilo-relay/Containerfile "$out" \
  --platform linux/amd64 \
  --ignorefile os/containers/tilo-relay/Containerfile.dockerignore \
  --build-context "github-assets=$assets" \
  --build-arg "BASE_REF=$BASE_REF" --build-arg "RPMS_REF=$rpms_ref"
printf '%s\n' "$rpms_source" > "$out/rpms-source"
