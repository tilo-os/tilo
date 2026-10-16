#!/usr/bin/env bash
# The build half of the x86 Windows guest (docs/windows-compatibility.md), as
# os/dev/run-development-windows-guest.py packages it, with podman instead of a Docker daemon:
# the Wine pair image-wine-paired made is signed with a key made for this run (private GnuPG
# state exists only in the signer's container, which has no network), the guest's RPMs are
# resolved against it, and the guest is assembled. Runs as root in a micro-VM step whose
# userspace is the wine-tools image, like wine.sh. It holds no secrets and pushes nothing: each
# image goes into an OCI archive with its digest beside it, named by the digest it is published
# with, so what the next build takes FROM and what the receipt records are the published images.
#
# usage: windows-guest.sh OUT
#
# podman stores images with the overlay driver here, not the vfs the image configures for Mock:
# the images sit on the Fedora bootc base's many layers, which vfs copies whole once per layer,
# more than the step's scratch disk holds.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
inputs=/target/windows-compat
tools_out=/target/publish/wine-tools
work=/var/tmp/tilo-windows-guest
registry=registry.tilo.so/tilo

[[ $(uname -m) == x86_64 ]] || die 'the guest is assembled natively on x86_64'
[[ $(id -u) == 0 ]] || die 'runs as root in a micro-VM step (isolation = "vm")'

# podman as root without systemd: cgroups and events by hand, images on the scratch disk.
mkdir -p /etc/containers/containers.conf.d
printf '[engine]\ncgroup_manager = "cgroupfs"\nevents_logger = "file"\n' \
  > /etc/containers/containers.conf.d/tilo-micro-vm.conf
printf '[storage]\ndriver = "overlay"\nrunroot = "/run/containers/storage"\ngraphroot = "/var/lib/containers/storage"\n' \
  > /etc/containers/storage.conf
p() { podman "$@"; }

# The wine-tools image this step runs in, by the digest it is published as.
runtime() {
  local digest
  digest=$(cat "$tools_out/digest")
  [[ $digest =~ ^sha256:[0-9a-f]{64}$ ]] || die "no wine-tools digest in $tools_out"
  printf '%s/wine-tools@%s' "$registry" "$digest"
}

# Write IMAGE to DIR/image.oci.tar with its digest and load that archive back under
# REPOSITORY, so builds take it FROM REPOSITORY@<digest>. Prints that reference.
archive() {
  local image=$1 dir=$2 repository=$3 digest
  mkdir -p "$dir"
  rm -f "$dir/image.oci.tar"
  p push --quiet --compression-format gzip --force-compression "$image" "oci-archive:$dir/image.oci.tar" >&2
  digest=$(archive_digest "$dir/image.oci.tar")
  [[ $digest =~ ^sha256:[0-9a-f]{64}$ ]] || die "no manifest digest in $dir/image.oci.tar"
  printf '%s\n' "$digest" > "$dir/digest"
  load "$dir" "$repository"
}

# Load DIR/image.oci.tar under REPOSITORY and print REPOSITORY@<digest>, checking that the
# reference resolves to it locally.
load() {
  local dir=$1 repository=$2 digest id
  digest=$(cat "$dir/digest")
  [[ $(archive_digest "$dir/image.oci.tar") == "$digest" ]] || die "$dir/image.oci.tar is not its build"
  id=$(p pull --quiet "oci-archive:$dir/image.oci.tar")
  p tag "$id" "$repository:local" >&2
  p image inspect "$repository@$digest" >/dev/null || die "$repository@$digest does not resolve to $dir"
  printf '%s@%s' "$repository" "$digest"
}

mkdir -p "$work"
out=${1:?usage: windows-guest.sh OUT}
rm -rf "$out"
mkdir -p "$out/evidence"
buildroot=$(runtime)
load "$tools_out" "$registry/wine-tools" >/dev/null
pair=$(load "$inputs/wine-paired" "$registry/wine-paired")
WINDOWS_GUEST_BUILDROOT_REF=$buildroot WINE_PAIR_REF=$pair \
  python3 "$repo_root/os/dev/run-development-windows-guest.py" --validate-only
build() {
  local tag=$1 recipe=$2 context=$3
  shift 3
  p build --pull=never --network=host --format oci --timestamp 0 --platform linux/amd64 \
    -t "$tag" -f "$recipe" "$@" "$context" >&2
}
# The signer: private GnuPG state exists only in this container, which has no network.
build localhost/windows-guest-signing:build "$repo_root/os/Containerfile.windows-guest-tools" "$repo_root" \
  --build-arg "WINDOWS_GUEST_BUILDROOT_REF=$buildroot"
container=$(p create "$pair" /unused)
mkdir -p "$work/pair" "$work/export"
p cp "$container:/wine-paired/." "$work/pair/"
p rm "$container" >/dev/null
receipt=$(p run --rm --network=none -v "$repo_root/os/dev:/tilo-dev:ro" -v "$work/pair:/wine-paired:ro" \
  -v "$work/export:/export" --entrypoint sh localhost/windows-guest-signing:build -euc '
    cp -a /tilo-dev/. /tilo/
    python3 /tilo/development-wine-repository.py sign --pair /wine-paired --pair-reference "$1" \
      --pin /tilo/wine-source.json --output /export/signed-wine
  ' signer "$pair")
[[ -n $receipt ]] || die 'the development Wine signer failed'
key=$(printf '%s' "$receipt" | python3 -c 'import json, sys; print(json.load(sys.stdin)["public_key_sha256"])')
[[ $key =~ ^[0-9a-f]{64}$ ]] || die 'the signer returned no public key digest'
printf '%s\n' "$receipt" > "$out/evidence/signing-receipt.json"
printf 'FROM scratch\nCOPY signed-wine /signed-wine\n' > "$work/Containerfile.signed-wine"
build localhost/development-wine:build "$work/Containerfile.signed-wine" "$work/export"
wine=$(archive localhost/development-wine:build "$out/development-wine" "$registry/tilo-development-wine")
args=(--build-arg "WINDOWS_GUEST_BUILDROOT_REF=$buildroot" --build-arg "DEVELOPMENT_WINE_REF=$wine"
      --build-arg "DEVELOPMENT_WINE_KEY_SHA256=$key")
build localhost/windows-guest-rpms:build "$repo_root/os/Containerfile.windows-guest-rpms" "$repo_root" "${args[@]}"
rpms=$(archive localhost/windows-guest-rpms:build "$out/rpms" "$registry/tilo-development-windows-guest-rpms")
python3 "$repo_root/os/dev/fetch-windows-guest-assets.py" "$work/assets"
build localhost/windows-guest:build "$repo_root/os/Containerfile.windows-guest" "$repo_root" "${args[@]}" \
  --build-arg "WINDOWS_GUEST_RPMS_REF=$rpms" --build-context "github-assets=$work/assets"
guest=$(archive localhost/windows-guest:build "$out/guest" "$registry/tilo-development-windows-guest")
printf '%s\n' "$guest" > "$out/evidence/guest-reference.txt"
python3 - "$out/evidence" "$buildroot" "$pair" "$wine" "$key" "$rpms" "$guest" <<'EOF'
import json, sys
from pathlib import Path
evidence, buildroot, pair, wine, key, rpms, guest = sys.argv[1:]
inputs = {"WINDOWS_GUEST_BUILDROOT_REF": buildroot, "WINE_PAIR_REF": pair, "DEVELOPMENT_WINE_REF": wine,
      "DEVELOPMENT_WINE_KEY_SHA256": key, "WINDOWS_GUEST_RPMS_REF": rpms}
# This receipt proves assembly only, never Windows application acceptance.
Path(evidence, "result.json").write_text(json.dumps({
"schema": 1, "development_only": True, "reference": guest,
"runtime_acceptance_verified": False, "inputs": inputs}, indent=2) + "\n")
EOF
