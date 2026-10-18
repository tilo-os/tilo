#!/usr/bin/env bash
# The OS release from stable: main's release image of the same commit (os-image-release-ARCH,
# found in the cache, not built again), sealed with the release keys, booted, signed, and
# published by t's server.
#
#   os-release.sh seal ARCH IN OUT   seal the release image in IN/oci for devices: install it
#                                    to a disk, close the composefs seal of its UKI and add the
#                                    PCR predictions the release key in KMS signs (the boot
#                                    layer, the one layer main can't make, since it carries
#                                    release signatures); then boot that disk under KVM with a
#                                    TPM, through the seal and the PCR checks, to a greeter
#                                    that accepts input. OUT/oci is the sealed image (IN's
#                                    layers and the boot layer's), OUT/digest its digest.
#   os-release.sh sign OUT LAYOUT... one index of the sealed images (one per architecture), and
#                                    the cosign signatures of it and of each image with the
#                                    release key in KMS, in OUT/oci as the manifests cosign
#                                    reads; publish-os pushes all of it from the server
#
# Every rootfs layer of what is published is main's: what stable adds is the boot layer and
# the signatures. Nothing here touches a registry: signing signs the digest the composition
# produced, and t's server publishes it (publish = { layout = ... } in t.toml).
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"
repository=registry.tilo.so/tilo/tilo

cmd_seal() {
  local arch=$1 in=$2 out=$3 id sealed
  [[ $(uname -m) == "$arch" ]] || die "the $arch image seals natively"
  [[ $(channel) == release ]] || die "only releases are sealed with the release keys; this run is for ${T_REF:-a working tree}"
  [[ -c /dev/kvm ]] || die 'no /dev/kvm: sealing boots the disk it makes'
  modprobe fuse 2>/dev/null || true
  modprobe loop 2>/dev/null || true
  use_cosign
  # The PCR prediction signer is a key in KMS, reached through the run's identity token.
  . os/dev/configure-cosign-kms.sh
  id=$(podman pull -q "oci:$in/oci")
  [[ $(podman image inspect --format '{{.Digest}}' "$id") == "$(cat "$in/digest")" ]] || die "$in/oci is not the image $in/digest names"
  podman tag "$id" localhost/tilo:release
  export TILO_IMAGE=localhost/tilo:release TILO_SKIP_BUILD=1 TILO_REQUIRE_PCR_PREDICTION=1 TILO_DISTROBOX= \
    TILO_VM_ARCH=$arch TILO_OUTPUT=$scratch/seal-output TILO_RUN=$scratch/seal-run TMPDIR=/var/tmp \
    TILO_MEM=6144 TILO_SMP=4 TILO_BOOT_WAIT_ATTEMPTS=150
  mkdir -p "$TILO_OUTPUT" "$TILO_RUN" "$out"
  # The seal loop of a release: install, close the composefs digest of the UKI, sign the PCR
  # predictions of exactly that UKI, install again with them.
  bash os/dev/build-image.sh
  export TILO_DISK=$TILO_OUTPUT/qcow2/disk.qcow2
  bash os/dev/vm.sh boot || die 'the sealed release disk does not start'
  local booted=0
  bash os/dev/vm.sh wait && booted=1
  bash os/dev/vm.sh stop >/dev/null 2>&1 || true
  tr -d '\000\r' < "$TILO_RUN/serial.log" | sed 's/\x1b\[[0-9;]*[A-Za-z]//g' > "$out/serial.log" || true
  if (( ! booted )) || grep -aq 'FAILED' "$out/serial.log"; then
    tail -n 80 "$out/serial.log" | sed 's/^/  serial: /' >&2
    die 'the sealed release does not boot through its seal to a greeter with no unit failed'
  fi
  echo 'ok    the sealed release boots through its seal and PCR checks to a greeter'
  sealed=$scratch/sealed
  rm -rf "$sealed"
  podman push -q --compression-format gzip --force-compression localhost/tilo:release "oci:$sealed:tilo"
  printf '{"inputs":{},"inputsDigest":""}' > "$scratch/seal-inputs.json"
  python3 -I tools/ci/os/oci_layers.py take "$in/oci" "$sealed" "$scratch/seal-fragment" seal "$scratch/seal-inputs.json"
  python3 -I tools/ci/os/oci_layers.py compose "$out" "$in/oci" "$scratch/seal-fragment"
}

cmd_sign() {
  local out=$1 digests d n=0 sigs
  shift
  [[ $(channel) == release ]] || die "only releases are signed; this run is for ${T_REF:-a working tree}"
  python3 -I tools/ci/os/oci_layers.py index "$out" "$@"
  use_cosign
  . os/dev/configure-cosign-kms.sh
  # The index and each architecture's image: consumers verify either.
  digests=$(python3 -I -c '
import json, sys
root = sys.argv[1]
top = json.load(open(root + "/index.json"))["manifests"][0]["digest"]
print(top)
for m in json.load(open(root + "/blobs/sha256/" + top[7:]))["manifests"]:
    print(m["digest"])' "$out/oci")
  for d in $digests; do
    n=$((n + 1))
    signing_payload "$repository" "$d" > "$scratch/payload-$n.json"
    cosign sign-blob --yes --key "$COSIGN_KEY_REF" --bundle "$scratch/bundle-$n.json" \
      --output-signature "$scratch/signature-$n" "$scratch/payload-$n.json" >/dev/null
    cosign verify-blob --key keys/cosign.pub --bundle "$scratch/bundle-$n.json" "$scratch/payload-$n.json" >/dev/null
    sigs=("$scratch/signature-$n,$scratch/bundle-$n.json")
    python3 -I tools/ci/os/oci_layers.py attach "$out/oci" "$d" "$scratch/payload-$n.json" "${sigs[@]}"
  done
  printf 'signed %s@%s and its images\n' "$repository" "$(cat "$out/digest")"
}

case ${1:-} in
  seal) cmd_seal "${2:?}" "${3:?}" "${4:?}" ;;
  sign) shift; cmd_sign "${1:?}" "${@:2}" ;;
  *) die 'usage: os-release.sh seal ARCH IN OUT | sign OUT LAYOUT...' ;;
esac
