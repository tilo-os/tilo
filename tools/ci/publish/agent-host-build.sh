#!/usr/bin/env bash
# The build half of the agent host image (os/containers/tilo-agent-host/Containerfile): the member
# and its enroll program that t already built (server-binaries.sh, the check relay-binaries) laid
# out on the mirrored base, with the runtime packages installed from the frozen RPM snapshot
# rpm-snapshot-ref.sh named in SNAPSHOT (the published one, or this run's image-rpms-x86_64 in
# RPMS where it is not published yet), into OUT (oci-build.sh), with the snapshot it came from in
# OUT/rpms-source, which oci-sign.sh refuses unless it is the published one. Nothing compiles
# here: the image is only the base, the packages and the programs.
#
# usage: agent-host-build.sh SNAPSHOT RPMS BINARIES OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
usage='usage: agent-host-build.sh SNAPSHOT RPMS BINARIES OUT'
snapshot=${1:?$usage}
rpms=${2:?$usage}
binaries=${3:?$usage}
out=${4:?$usage}
cd "$repo_root"
bash os/containers/tilo-agent-host/test-contract.sh
for bin in tilo-runtime tilo-enroll; do
  [[ -x $binaries/$bin ]] || die "no $bin in $binaries"
done
# oci-build.sh builds on this store, so an unpublished snapshot loaded here is the one FROM finds.
use_buildah buildah overlay
snapshot_for_build "$snapshot" "$rpms" x86_64
if [[ $rpms_source == "$rpms_ref" ]]; then
  python3 os/dev/verify-rpm-snapshot-buildkit.py "$rpms_ref"
fi
# The context is what the image copies and nothing else, so no checkout metadata reaches the
# build. Source bytes and the pinned base and snapshot decide the image; a commit time is no input
# (the Containerfile's SOURCE_DATE_EPOCH defaults to 0 and oci-build.sh fixes the timestamps).
context=$scratch/agent-host-context
rm -rf "$context"
mkdir -p "$context/bin" "$context/os/containers/tilo-agent-host"
cp os/containers/tilo-agent-host/Containerfile "$context/"
cp "$binaries/tilo-runtime" "$binaries/tilo-enroll" "$context/bin/"
cp os/containers/tilo-agent-host/tilo-agent-mcp "$context/os/containers/tilo-agent-host/"
bash tools/ci/publish/oci-build.sh "$context" "$context/Containerfile" "$out" \
  --platform linux/amd64 \
  --build-arg "RPMS_REF=$rpms_ref"
printf '%s\n' "$rpms_source" > "$out/rpms-source"
