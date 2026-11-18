#!/usr/bin/env bash
# The build half of the server image (os/Containerfile.host): the programs t already built
# (server-binaries.sh, the check host-binaries) laid out on the same mirrored Fedora bootc base and
# the same install-time files the desktop image uses, with the runtime packages installed from the
# frozen RPM snapshot rpm-snapshot-ref.sh named in SNAPSHOT (the published one, or this run's
# image-rpms-x86_64 in RPMS where it is not published yet), into OUT (oci-build.sh), with the
# snapshot it came from in OUT/rpms-source, which oci-sign.sh refuses unless it is the published
# one. The Firecracker release it runs comes from the checksum-verified external assets, as the VM
# host image does. Nothing compiles here: the image is only the base, the packages, the programs
# and their configuration, so it costs a layering run whatever changed in the tree.
#
# usage: host-build.sh SNAPSHOT RPMS BINARIES OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
usage='usage: host-build.sh SNAPSHOT RPMS BINARIES OUT'
snapshot=${1:?$usage}
rpms=${2:?$usage}
binaries=${3:?$usage}
out=${4:?$usage}
cd "$repo_root"
for bin in tilo-server tilo-server-admin tilo-enroll fs tilo-legacy tilo-hosted-jail tilo-api \
  tilolang-process \
  tilo-esp-write tilo-image-policy-check tilo-image-trust tilo-board-successor \
  tilo-rollback tilo-auth tilo-boot-signin tilo-sibling-unlock; do
  [[ -x $binaries/$bin ]] || die "no $bin in $binaries"
done
# The mirrored bootc base and the install-time boot loader configuration the desktop image also
# builds on (os/base-snapshot.env, os/usr/lib/bootc/install/40-tilo.toml).
[[ -f os/base-snapshot.env ]] || die 'no os/base-snapshot.env'
. os/base-snapshot.env
[[ -n ${BASE_REF:-} ]] || die 'os/base-snapshot.env names no BASE_REF'
# oci-build.sh builds on this store, so an unpublished snapshot loaded here is the one FROM finds.
use_buildah buildah overlay
snapshot_for_build "$snapshot" "$rpms" x86_64
# The package manifest inside a published snapshot must name what this tree's Containerfiles
# install, not only its signed provenance: a stale snapshot fails here, before the build. The
# run's own image-rpms was built from these Containerfiles.
if [[ $rpms_source == "$rpms_ref" ]]; then
  python3 os/dev/verify-rpm-snapshot-buildkit.py "$rpms_ref"
fi
assets=$(pinned_assets firecracker-v1.17.0-x86_64.tgz)
# The context is what the image copies and nothing else, so no checkout metadata reaches the
# build and a one-crate change moves one file in it. Source bytes and the pinned base and snapshot
# decide the image; a commit time is no input (the Containerfile's SOURCE_DATE_EPOCH defaults to 0
# and oci-build.sh fixes the timestamps).
context=$scratch/host-context
rm -rf "$context"
mkdir -p "$context/bin" "$context/keys/release" \
  "$context/os/containers/tilo-vm-host" "$context/os/etc/containers/registries.d" \
  "$context/os/etc/modules-load.d" \
  "$context/os/usr/lib/bootc/install" "$context/os/usr/lib/sysusers.d" \
  "$context/os/usr/lib/tmpfiles.d" "$context/os/usr/lib/systemd/system" \
  "$context/os/usr/lib/systemd/system-generators" \
  "$context/os/usr/lib/dracut/modules.d" \
  "$context/os/usr/libexec" "$context/docs/app-model"
cp os/Containerfile.host "$context/Containerfile"
cp "$binaries"/* "$context/bin/"
cp os/containers/tilo-vm-host/hosted-jail.json "$context/os/containers/tilo-vm-host/"
cp os/etc/containers/policy.json "$context/os/etc/containers/"
cp os/etc/containers/registries.d/*.yaml "$context/os/etc/containers/registries.d/"
cp os/usr/lib/bootc/install/40-tilo.toml "$context/os/usr/lib/bootc/install/"
cp os/usr/lib/sysusers.d/*.conf "$context/os/usr/lib/sysusers.d/"
cp os/usr/lib/tmpfiles.d/*.conf "$context/os/usr/lib/tmpfiles.d/"
cp os/etc/modules-load.d/tilo-hosted.conf "$context/os/etc/modules-load.d/"
cp os/usr/libexec/* "$context/os/usr/libexec/"
cp -r os/usr/lib/systemd/system/. "$context/os/usr/lib/systemd/system/"
cp -r os/usr/lib/systemd/system-generators/. "$context/os/usr/lib/systemd/system-generators/"
cp -r os/usr/lib/dracut/modules.d/. "$context/os/usr/lib/dracut/modules.d/"
cp docs/recovery.md "$context/docs/"
cp docs/app-model/environments.md "$context/docs/app-model/"
cp keys/cosign.pub keys/tpm2-pcr-signing.public.pem "$context/keys/"
cp -r keys/release/. "$context/keys/release/"
bash tools/ci/publish/oci-build.sh "$context" "$context/Containerfile" "$out" \
  --platform linux/amd64 \
  --build-context "github-assets=$assets" \
  --build-arg "BASE_REF=$BASE_REF" \
  --build-arg "RPMS_REF=$rpms_ref"
printf '%s\n' "$rpms_source" > "$out/rpms-source"
