#!/usr/bin/env bash
# The server image (os/Containerfile.host): the programs t already built (server-binaries.sh, the
# check host-binaries) and the install-time files they read, laid out on BASE, the image
# os/Containerfile.host-base built (host-base.sh): the same mirrored Fedora bootc base and package
# set the desktop image uses, with Firecracker. The layer is a copying build, so a change to one
# program or one unit file costs the compile and this build, not a package install.
#
# BOOTC is the OS tools image the OS image's desktop layer takes its bootc from (os-tools-ARCH,
# os/image/helpers/bootc-builder.Containerfile): the base's own release with the patches in
# os/external-assets/bootc-runtime/, loaded here and copied onto the base in place of its stock
# binary. One build serves both images.
#
# With --dev it builds the development server image instead (t's image-host-dev), on the same base
# and with the same stages, then the additions of os/Containerfile.host-dev on their last one
# (t's image-host-dev programs, server-binaries.sh `host host-dev`, are built without LTO): the
# server image's own programs, units, flavour and update policy with the runtime that runs the
# device's steps and the dev-VM CLI, the device's boot units, a base.json naming how the programs were built, and systemd-sysext
# merging layers at boot. It is never a release; seal-host-dev seals it as seal-host seals the
# server image. A commit that changes a server program therefore costs its no-LTO compile and this
# build, not the release server image's.
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
# (bootc#2334), which seal-host (seal-host-dev for the development image) does before anything is
# signed.
#
# usage: host-build.sh [--dev] BINARIES BASE BOOTC GENERATION OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
usage='usage: host-build.sh [--dev] BINARIES BASE BOOTC GENERATION OUT'
dev=0
case ${1:-} in --dev) dev=1; shift ;; esac
binaries=${1:?$usage}
base=${2:?$usage}
bootc=${3:?$usage}
generation_file=${4:?$usage}
out=${5:?$usage}
read -r generation < "$generation_file"
[[ $generation =~ ^[1-9][0-9]*$ ]] || die "$generation_file holds no generation: '$generation'"
cd "$repo_root"
bins=(tilo-runtime tilo fs tilo-linux-vm tilo-hosted-jail tilo-api
      tilolang-process
      tilo-esp-write tilo-image-policy-check tilo-image-trust tilo-board-successor
      tilo-rollback tilo-auth tilo-boot-signin tilo-crypto tilo-default-start)
# The device's units a development server enables: the record of the booted image (before
# systemd-sysext merges anything), the accounts brought back after boot, and the layers an
# installation left for the first boot, applied before the server starts.
vm_units=(tilo-vm-reconcile-image.service tilo-vm-restore.service tilo-vm-incoming.service)
if (( dev )); then
  mapfile -t dev_bins < <(python3 os/scripts/install-components.py --dry-run --format bins native-runtime-host device-steps vm-cli)
  bins+=("${dev_bins[@]}")
  for unit in "${vm_units[@]}"; do
    [[ -f crates/os/tilo-device-steps/data/$unit ]] || die "no crates/os/tilo-device-steps/data/$unit"
  done
fi
for bin in "${bins[@]}"; do
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
# The OS tools image, as this store's image: the layer copies its bootc onto the base. It comes
# as the layout its check left, so nothing pulls it from a registry.
[[ -f $bootc/image-tool-stages/oci/index.json ]] \
  || die "$bootc holds no image-tool-stages layout: it is not an os-tools build"
b tag "$(b pull -q "oci:$bootc/image-tool-stages/oci:tilo")" localhost/tilo-host-bootc-tools:built
# The context is what the image copies and nothing else, so no checkout metadata reaches the
# build and a one-crate change moves one file in it. Source bytes and the cached base decide the
# image; a commit time is no input (the date is source-date-host's, or source-date-host-dev's,
# over exactly these files).
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
if (( dev )); then
  cat os/Containerfile.host-dev >> "$context/Containerfile"
fi
cp "$binaries"/* "$context/bin/"
# The server image's components, placed as os/components.toml says (`images = ["host"]`).
python3 os/scripts/install-components.py --all --image host --from "$binaries" \
  --dest "$context/host-components" --quiet
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
args=(--platform linux/amd64
      --build-arg "HOST_BASE=localhost/tilo-host-base:built"
      --build-arg "UKI_TOOLS=localhost/tilo-host-uki-tools:built"
      --build-arg "BOOTC_TOOLS=localhost/tilo-host-bootc-tools:built"
      --build-arg "TILO_GENERATION=$generation"
      --build-arg "TILO_CFS_DIGEST=$(printf '0%.0s' {1..128})")
if (( dev )); then
  # The additions' inputs, as os/Containerfile.host-dev stages them, and the base.json they read:
  # the programs came from t's host-binaries-dev check, whose image must be t.toml's default one,
  # since that is the builder a layer says it was built by.
  mkdir -p "$context/vm-units"
  for unit in "${vm_units[@]}"; do
    cp "crates/os/tilo-device-steps/data/$unit" "$context/vm-units/"
  done
  # The device's step units and the rule and directories its daemon starts them with, as every
  # image that carries tilo-runtime has them.
  cp os/usr/lib/systemd/system/tilo-device-*@.service "$context/vm-units/"
  mkdir -p "$context/device-steps"
  cp os/etc/polkit-1/rules.d/49-tilo-device-steps.rules os/usr/lib/tmpfiles.d/tilo-device-control.conf \
    "$context/device-steps/"
  cp os/components.toml "$context/os/"
  mkdir -p "$context/os/usr/lib/tilo"
  cp os/usr/lib/tilo/node-server.toml "$context/os/usr/lib/tilo/"
  cp os/scripts/install-components.py os/scripts/finalize-dev-image.py "$context/os/scripts/"
  cp Cargo.lock "$context/"
  cp keys/cosign-dev.pub "$context/keys/"
  builder=$(awk '/^\[/ { exit } /^image = "/ { gsub(/^image = "|"$/, ""); print; exit }' t.toml)
  [[ $builder =~ ^registry\.tilo\.so/tilo/tilo-builder@sha256:[0-9a-f]{64}$ ]] \
    || die "t.toml names no default check image pinned by digest: '$builder'"
  awk -v name='name = "host-binaries-dev"' '
    /^\[\[check\]\]/ { inside = 0 } $0 == name { inside = 1 }
    inside && /^image = / { found = 1 } END { exit !found }' t.toml \
    && die "host-binaries-dev names an image of its own; base.json would name the wrong builder"
  base_created=$(b inspect --type image --format '{{.OCIv1.Created.UTC.Format "2006-01-02T15:04:05Z"}}' \
    localhost/tilo-host-base:built)
  args+=(--build-arg "TILO_BASE_IMAGE=localhost/tilo-host-base:built"
         --build-arg "TILO_BASE_DIGEST=$base_digest"
         --build-arg "TILO_BASE_CREATED=$base_created"
         --build-arg "TILO_BUILDER_IMAGE=$builder"
         --build-arg "TILO_VM_UNITS=${vm_units[*]}"
         # The programs it carries were linked without LTO (host-binaries-dev), which the
         # sealing stage's label says; a publication is refused such an image.
         --build-arg "TILO_RUST_LTO=off")
fi
bash tools/ci/publish/oci-build.sh "$context" "$context/Containerfile" "$out" "${args[@]}"
printf '%s\n' "$rpms_source" > "$out/rpms-source"
printf '%s\n' "$generation" > "$out/generation"
if (( dev )); then
  # What the image's layer check reads, beside it, for whoever builds a layer for it.
  b tag "$(b pull -q "oci-archive:$out/image.oci.tar")" localhost/tilo-host-dev:built
  container=$(b from -q localhost/tilo-host-dev:built)
  cp "$(b mount "$container")/usr/share/tilo-vm/base.json" "$out/base.json"
  b rm "$container" >/dev/null
  printf 'development server %s on %s\n' "$(cat "$out/digest")" "$base_digest"
  cat "$out/base.json"
fi
