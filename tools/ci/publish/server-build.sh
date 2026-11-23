#!/usr/bin/env bash
# The build half of the server image (os/containers/tilo-server/Containerfile): the device's daemon,
# the command beside it and the filesystem replica t already built (server-binaries.sh, the check
# server-binaries) laid out on the mirrored base, with the runtime packages installed from the frozen
# RPM snapshot rpm-snapshot-ref.sh named in SNAPSHOT (the published one, or this run's
# image-rpms-x86_64 in RPMS where it is not published yet), into OUT (oci-build.sh), with the
# snapshot it came from in OUT/rpms-source, which oci-sign.sh refuses unless it is the published one.
# MODEL_RUNTIME is the model runtime the apps' model modules link against (the app-models check's
# `app-model-runtime/`), which the server applies installed apps' actions with.
# Nothing compiles here: the image is only the base, the packages, the binaries and their
# configuration, so it costs a layering run whatever changed in the tree.
#
# usage: server-build.sh SNAPSHOT RPMS BINARIES OUT MODEL_RUNTIME
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
usage='usage: server-build.sh SNAPSHOT RPMS BINARIES OUT MODEL_RUNTIME'
snapshot=${1:?$usage}
rpms=${2:?$usage}
binaries=${3:?$usage}
out=${4:?$usage}
app_model_runtime=${5:?$usage}
cd "$repo_root"
bash os/containers/tilo-server/test-contract.sh
for bin in tilo tilo-runtime; do
  [[ -x $binaries/$bin ]] || die "no $bin in $binaries"
done
[[ -f $app_model_runtime/runtime.json ]] || die "no model runtime in $app_model_runtime"
# oci-build.sh builds on this store, so an unpublished snapshot loaded here is the one FROM finds.
use_buildah buildah overlay
snapshot_for_build "$snapshot" "$rpms" x86_64
# The package manifest inside a published snapshot must name what this tree's Containerfiles
# install, not only its signed provenance: a stale snapshot fails here, before the build. The
# run's own image-rpms was built from these Containerfiles.
if [[ $rpms_source == "$rpms_ref" ]]; then
  python3 os/dev/verify-rpm-snapshot-buildkit.py "$rpms_ref"
fi
# The context is what the image copies and nothing else, so no checkout metadata reaches the
# build and a one-crate change moves one file in it. Source bytes and the pinned base and snapshot
# decide the image; a commit time is no input (oci-build.sh fixes the timestamps).
context=$scratch/server-context
rm -rf "$context"
mkdir -p "$context/bin" "$context/os/containers/tilo-server" "$context/os/usr/lib/systemd/system"
cp os/containers/tilo-server/Containerfile "$context/"
cp "$binaries/tilo" "$binaries/tilo-runtime" "$context/bin/"
cp os/containers/tilo-server/tilo-home-peer "$context/os/containers/tilo-server/"
cp -R "$app_model_runtime" "$context/app-model-runtime"
cp os/usr/lib/systemd/system/tilo-server.service "$context/os/usr/lib/systemd/system/"
bash tools/ci/publish/oci-build.sh "$context" "$context/Containerfile" "$out" \
  --platform linux/amd64 \
  --build-arg "RPMS_REF=$rpms_ref"
printf '%s\n' "$rpms_source" > "$out/rpms-source"
