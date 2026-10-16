#!/usr/bin/env bash
# The build half of the relay image (os/containers/tilo-relay/Containerfile): the relay and the
# filesystem replica compiled from this tree on the mirrored base, installed from the verified
# frozen RPM snapshot in SNAPSHOT/rpms-ref (rpm-snapshot-ref.sh), into OUT (oci-build.sh).
#
# usage: relay-build.sh SNAPSHOT OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
snapshot=${1:?usage: relay-build.sh SNAPSHOT OUT}
out=${2:?}
cd "$repo_root"
bash os/containers/tilo-relay/test-contract.sh
. os/base-snapshot.env
[[ ${BASE_REF:-} =~ ^registry\.tilo\.so/tilo/fedora-bootc@sha256:[0-9a-f]{64}$ ]] \
  || die 'BASE_REF is not a digest-pinned fedora-bootc'
rpms_ref=$(cat "$snapshot/rpms-ref")
[[ $rpms_ref =~ ^registry\.tilo\.so/tilo/rpms@sha256:[0-9a-f]{64}$ ]] || die "unexpected snapshot $rpms_ref"
assets=$scratch/github-assets
TILO_GITHUB_ASSET_CACHE=$assets TILO_ASSET_STORE= bash os/dev/cache-github-assets.sh \
  sccache-v0.17.0-x86_64-unknown-linux-musl.tar.gz
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
