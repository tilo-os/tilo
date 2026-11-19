#!/usr/bin/env bash
# The cached base of the server image (os/Containerfile.host-base): the mirrored Fedora bootc
# base with the runtime packages installed from the frozen RPM snapshot rpm-snapshot-ref.sh named
# in SNAPSHOT (the published one, or the build's own image-rpms-x86_64 in RPMS where it is
# not published yet), and the checksum-verified Firecracker release, into OUT (oci-build.sh), with
# snapshot it came from in OUT/rpms-source, which oci-sign.sh refuses unless it is the published
# one; and the tools that seal the server's UKI, from the same base and snapshot, in OUT/uki-tools.
#
# Nothing here compiles, and nothing here is a program of this tree: the package set and the
# Firecracker release change only when the snapshot or the base does, so t serves this to every
# commit that keeps them (source-date-host-base dates it by the files that decide it). The
# server's own programs and their configuration are os/Containerfile.host's layer on top
# (host-build.sh), which is then a copying build rather than a package install.
#
# usage: host-base.sh SNAPSHOT RPMS OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
usage='usage: host-base.sh SNAPSHOT RPMS OUT'
snapshot=${1:?$usage}
rpms=${2:?$usage}
out=${3:?$usage}
cd "$repo_root"
# The mirrored bootc base (os/base-snapshot.env) is what both layers stand on.
[[ -f os/base-snapshot.env ]] || die 'no os/base-snapshot.env'
. os/base-snapshot.env
[[ -n ${BASE_REF:-} ]] || die 'os/base-snapshot.env names no BASE_REF'
# buildah without a daemon on the step's own ext4 scratch disk: overlay works there and does not
# copy every layer of a multi-GB base whole as vfs does.
use_buildah buildah overlay
snapshot_for_build "$snapshot" "$rpms" x86_64
# The package manifest inside a published snapshot must name what this tree's Containerfiles
# install, not only its signed provenance: a stale snapshot fails here, before the build. The
# build's own image-rpms was built from these Containerfiles.
if [[ $rpms_source == "$rpms_ref" ]]; then
  python3 os/dev/verify-rpm-snapshot-buildkit.py "$rpms_ref"
fi
# The pinned Firecracker release, as a named build context. It comes from os-assets' outputs and
# is checksum-verified here; nothing downloads it.
assets=$(pinned_assets firecracker-v1.17.0-x86_64.tgz)
mkdir -p "$out"
# The context is the sealing tools' files and nothing else: the base reads the snapshot by digest
# and the Firecracker assets by the named context.
context=$scratch/host-base-context
rm -rf "$context"
mkdir -p "$context/os/dev"
cp os/dev/uki-tools.packages os/dev/uki.sh os/dev/uki-hardware-profile.py os/dev/hardware_profile.py \
  os/dev/make-boot-splash.py "$context/os/dev/"
# The base and, from the same file, the desktop's own `uki-tools` stage: the tools that seal the
# server's UKI (os/Containerfile.host), from the same base and snapshot, into OUT/uki-tools.
cat os/Containerfile.host-base os/image/helpers/uki-tools.Containerfile > "$context/Containerfile"
for stage in host-base uki-tools; do
  dest=$out
  [[ $stage == host-base ]] || dest=$out/$stage
  bash tools/ci/publish/oci-build.sh "$context" "$context/Containerfile" "$dest" \
    --platform linux/amd64 \
    --target "$stage" \
    --build-context "github-assets=$assets" \
    --build-arg "BASE_REF=$BASE_REF" \
    --build-arg "RPMS_REF=$rpms_ref"
done
printf '%s\n' "$rpms_source" > "$out/rpms-source"
