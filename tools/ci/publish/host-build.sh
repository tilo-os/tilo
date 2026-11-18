#!/usr/bin/env bash
# The upper half of the server image (os/Containerfile.host): the programs t already built
# (server-binaries.sh, the check host-binaries) and the install-time files they read, laid out on
# BASE, the image os/Containerfile.host-base built (host-base.sh): the same mirrored Fedora bootc
# base and package set the desktop image uses, with Firecracker. The layer is a copying build, so a
# change to one program or one unit file costs the compile and this build, not a package install.
#
# The snapshot the image came from is the base's, recorded beside it: this layer installs no
# package, so it names what the base applied. OUT/rpms-source carries it for oci-sign.sh, which
# refuses an image built from a set no release published.
#
# The image is sealed with the desktop's own stages (os/image/boot.Containerfile, appended to
# os/Containerfile.host), built with the sealing tools host-base built (BASE/uki-tools): one UKI
# carrying the kernel, the initramfs and a command line with GENERATION, the number in the file
# GENERATION names (generation-host). Its composefs digest is a placeholder of zeros: the install
# computes the real one and os/dev/build-image.sh rewrites only the UKI under /boot to name it
# (bootc#2334), which seal-host does before anything is signed.
#
# usage: host-build.sh BINARIES BASE GENERATION OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
usage='usage: host-build.sh BINARIES BASE GENERATION OUT'
binaries=${1:?$usage}
base=${2:?$usage}
generation_file=${3:?$usage}
out=${4:?$usage}
read -r generation < "$generation_file"
[[ $generation =~ ^[1-9][0-9]*$ ]] || die "$generation_file holds no generation: '$generation'"
cd "$repo_root"
for bin in tilo-server tilo-server-admin tilo-enroll fs tilo-linux-vm tilo-hosted-jail tilo-api \
  tilolang-process \
  tilo-esp-write tilo-image-policy-check tilo-image-trust tilo-board-successor \
  tilo-rollback tilo-auth tilo-boot-signin tilo-crypto; do
  [[ -x $binaries/$bin ]] || die "no $bin in $binaries"
done
use_buildah buildah overlay
[[ -s $base/rpms-source ]] || die "$base names no RPM snapshot: it is not a host-base build"
rpms_source=$(cat "$base/rpms-source")
# The base as this store's image, by the digest the build recorded.
base_digest=$(cat "$base/digest")
[[ $(archive_digest "$base/image.oci.tar") == "$base_digest" ]] \
  || die "$base/image.oci.tar is not the base $base_digest names"
base_image=$(b pull -q "oci-archive:$base/image.oci.tar")
b tag "$base_image" localhost/tilo-host-base:built
[[ $(archive_digest "$base/uki-tools/image.oci.tar") == "$(cat "$base/uki-tools/digest")" ]] \
  || die "$base/uki-tools/image.oci.tar is not the image $base/uki-tools/digest names"
b tag "$(b pull -q "oci-archive:$base/uki-tools/image.oci.tar")" localhost/tilo-host-uki-tools:built
# The context is what the image copies and nothing else, so no checkout metadata reaches the
# build and a one-crate change moves one file in it. Source bytes and the cached base decide the
# image; a commit time is no input (the date is source-date-host's, over exactly these files).
context=$scratch/host-context
rm -rf "$context"
mkdir -p "$context/bin" "$context/keys/release" \
  "$context/os/containers/tilo-vm-host" "$context/os/etc/containers/registries.d" \
  "$context/os/etc/modules-load.d" \
  "$context/os/usr/lib/bootc/install" "$context/os/usr/lib/bootc/kargs.d" \
  "$context/os/usr/lib/sysusers.d" \
  "$context/os/usr/lib/tmpfiles.d" "$context/os/usr/lib/systemd/system" \
  "$context/os/usr/lib/systemd/system-generators" \
  "$context/os/usr/lib/dracut/modules.d" \
  "$context/os/usr/libexec" "$context/os/scripts" "$context/docs/app-model"
cat os/Containerfile.host os/image/boot.Containerfile > "$context/Containerfile"
cp "$binaries"/* "$context/bin/"
cp os/containers/tilo-vm-host/hosted-jail.json "$context/os/containers/tilo-vm-host/"
cp os/etc/containers/policy.json "$context/os/etc/containers/"
cp os/etc/containers/registries.d/*.yaml "$context/os/etc/containers/registries.d/"
cp os/usr/lib/bootc/install/40-tilo.toml "$context/os/usr/lib/bootc/install/"
cp os/usr/lib/bootc/kargs.d/00-root.toml os/usr/lib/bootc/kargs.d/01-hardening.toml \
  os/usr/lib/bootc/kargs.d/03-boot.toml os/usr/lib/bootc/kargs.d/04-mode.toml \
  os/usr/lib/bootc/kargs.d/05-credentials.toml "$context/os/usr/lib/bootc/kargs.d/"
cp os/usr/lib/sysusers.d/*.conf "$context/os/usr/lib/sysusers.d/"
cp os/usr/lib/tmpfiles.d/*.conf "$context/os/usr/lib/tmpfiles.d/"
cp os/etc/modules-load.d/tilo-hosted.conf "$context/os/etc/modules-load.d/"
cp os/usr/libexec/* "$context/os/usr/libexec/"
cp -r os/usr/lib/systemd/system/. "$context/os/usr/lib/systemd/system/"
cp -r os/usr/lib/systemd/system-generators/. "$context/os/usr/lib/systemd/system-generators/"
cp -r os/usr/lib/dracut/modules.d/. "$context/os/usr/lib/dracut/modules.d/"
cp os/scripts/build-initramfs.sh os/scripts/verify-board-initramfs.py "$context/os/scripts/"
cp docs/recovery.md "$context/docs/"
cp docs/app-model/environments.md "$context/docs/app-model/"
cp keys/cosign.pub keys/tpm2-pcr-signing.public.pem "$context/keys/"
cp -r keys/release/. "$context/keys/release/"
bash tools/ci/publish/oci-build.sh "$context" "$context/Containerfile" "$out" \
  --platform linux/amd64 \
  --build-arg "HOST_BASE=localhost/tilo-host-base:built" \
  --build-arg "UKI_TOOLS=localhost/tilo-host-uki-tools:built" \
  --build-arg "TILO_GENERATION=$generation" \
  --build-arg "TILO_CFS_DIGEST=$(printf '0%.0s' {1..128})"
printf '%s\n' "$rpms_source" > "$out/rpms-source"
printf '%s\n' "$generation" > "$out/generation"
