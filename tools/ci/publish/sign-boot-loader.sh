#!/usr/bin/env bash
# Sign the boot loader a release image installs on the ESP, before the image is
# sealed: systemd-boot sits at /usr/lib/systemd/boot/efi/systemd-boot*.efi in the
# rootfs, `bootctl install` (which `bootc install` runs) copies its `.efi.signed`
# companion to EFI/systemd and EFI/BOOT when there is one, and the firmware
# verifies what it loads. So the signature has to be in the image the seal
# describes, not added to the installed disk afterwards.
#
#   sign-boot-loader.sh IMAGE OUT
#
# IMAGE is an image a build left as an OCI archive (IMAGE/image.oci.tar) or an OCI
# layout (IMAGE/oci), with IMAGE/digest. OUT is the same shape with the companion
# added, its digest, and the generation and rpms-source when IMAGE has them.
#
# The keys are the release EFI key, in KMS through tools/signing/boot-signer.sh
# (the sign-boot step's own signer). With TILO_BOOT_SIGNER_DRY_RUN set, the
# throwaway keys made there sign instead, and nothing is pinned: what a rehearsal
# and the boot check use. A release proves the certificate against
# TILO_EFI_SIGNING_CERT_SHA256 before it signs anything.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"

usage='usage: sign-boot-loader.sh IMAGE OUT'
in=${1:?$usage}
out=${2:?$usage}
dry=${TILO_BOOT_SIGNER_DRY_RUN:-}

case $(uname -m) in
  x86_64) loader_name=systemd-bootx64.efi machine=0x8664 ;;
  aarch64) loader_name=systemd-bootaa64.efi machine=0xaa64 ;;
  *) die "no systemd-boot name for $(uname -m)" ;;
esac

tools() {
  bash tools/ci/install-packages.sh tools/ci/packages/boot-assessment.txt
  rpm -q binutils >/dev/null 2>&1 \
    || dnf install -y --setopt=install_weak_deps=False binutils >/dev/null
  command -v python3 >/dev/null || dnf install -y python3 >/dev/null
  use_signing_tools
}

# The image in $1, as podman's $2, from whichever shape the build left it in.
load_image() {
  local dir=$1 name=$2 layout digest
  digest=$(cat "$dir/digest")
  if [[ -f $dir/image.oci.tar ]]; then
    [[ $(archive_digest "$dir/image.oci.tar") == "$digest" ]] \
      || die "$dir/image.oci.tar is not the image $dir/digest names"
    layout=$scratch/loader-input
    rm -rf "$layout"
    mkdir -p "$layout"
    tar -xf "$dir/image.oci.tar" -C "$layout"
    podman tag "$(podman pull -q "oci:$layout")" "$name"
  else
    [[ -f $dir/oci/index.json ]] || die "$dir holds no image: neither image.oci.tar nor oci/"
    [[ $(index_digest "$dir/oci/index.json" < "$dir/oci/index.json") == "$digest" ]] \
      || die "$dir/oci is not the image $dir/digest names"
    podman tag "$(podman pull -q "oci:$dir/oci")" "$name"
  fi
}

# The image's own tilo-crypto, over a file in the workspace mounted read-only:
# the verification a native runner needs no separate copy of.
image_crypto() {
  local command=$1 argument
  shift
  local -a arguments=()
  for argument in "$@"; do
    case $argument in
      "$work"/*) arguments+=("/work/${argument#"$work/"}") ;;
      *) die "tilo-crypto input is outside the signing workspace: $argument" ;;
    esac
  done
  podman run --rm --pull=never -v "$work:/work:ro,Z" \
    --entrypoint /usr/libexec/tilo/tilo-crypto "$name" "$command" "${arguments[@]}"
}

tools
[[ ! -e $out || ! -L $out ]] || die "$out is a symlink"
rm -rf "$out"
mkdir -p "$out"
work=$(mktemp -d "${TMPDIR:-/var/tmp}/tilo-boot-loader.XXXXXX")
name=localhost/tilo-boot-loader:$(basename "$work" | tr 'A-Z' 'a-z')
trap 'podman rmi -f "$name" >/dev/null 2>&1 || true; rm -rf "$work"' EXIT
work=$(cd "$work" && pwd)

load_image "$in" "$name"
# A container that is created and never started: reading one file runs nothing of
# the image, which keeps the step with the signing keys from running what it signs.
container=$(podman create --pull=never --entrypoint /bin/true "$name")
podman cp "$container:/usr/lib/systemd/boot/efi/$loader_name" "$work/loader.efi"
if podman cp "$container:/usr/lib/systemd/boot/efi/$loader_name.signed" "$work/present.signed" 2>/dev/null; then
  die "$in already carries a signed boot loader"
fi
podman rm "$container" >/dev/null
[[ -s $work/loader.efi ]] || die "$in carries no $loader_name"
python3 - "$work/loader.efi" "$machine" <<'PY' || die "the boot loader in the image is not this machine's EFI application"
import struct, sys
path, expected = sys.argv[1], int(sys.argv[2], 0)
data = open(path, 'rb').read()
assert data[:2] == b'MZ', 'missing DOS header'
pe = struct.unpack_from('<I', data, 0x3C)[0]
assert data[pe:pe + 4] == b'PE\0\0', 'missing PE header'
assert struct.unpack_from('<H', data, pe + 4)[0] == expected, 'PE machine differs'
assert struct.unpack_from('<H', data, pe + 24)[0] == 0x20B, 'not PE32+'
assert struct.unpack_from('<H', data, pe + 24 + 68)[0] == 10, 'not an EFI application'
PY

bash tools/signing/boot-signer.sh export-efi-cert "$work/loader-signing.crt"
if [[ -n $dry ]]; then
  # The throwaway identity the rehearsal and the boot check enrol in firmware.
  printf 'signed with the throwaway keys in %s\n' "$dry" >&2
else
  [[ $(image_crypto certificate-sha256 "$work/loader-signing.crt") \
     == "$(publication TILO_EFI_SIGNING_CERT_SHA256)" ]] \
    || die 'the signer exports a certificate other than the pinned Tilo release EFI certificate'
fi
bash tools/signing/boot-signer.sh sign-efi "$work/loader.efi" "$work/loader.efi.signed"
[[ $(image_crypto efi-signature-count "$work/loader.efi.signed") =~ ^[1-9][0-9]*$ ]] \
  || die 'the signer returned an unsigned boot loader'
image_crypto verify-efi "$work/loader-signing.crt" "$work/loader.efi.signed" >/dev/null \
  || die 'the boot loader does not verify against the certificate that signed it'
python3 os/dev/verify-authenticode-envelope.py "$work/loader.efi" "$work/loader.efi.signed" \
  || die 'signing the boot loader changed bytes outside its Authenticode envelope'

# Only the companion is added: the loader itself stays as the package ships it,
# so the same bytes have one identity and `bootctl` selects the signed copy.
cat > "$work/Containerfile" <<EOF
ARG BASE_IMAGE
FROM \${BASE_IMAGE}
COPY loader.efi.signed /usr/lib/systemd/boot/efi/$loader_name.signed
EOF
build_args=(-t "$name" -f "$work/Containerfile" --build-arg "BASE_IMAGE=$name" --pull=never)
# Dated as the image is, so the same signatures make the same image. Podman 5
# clamps with --source-date-epoch; 4 has only --timestamp (as os/dev/_common.sh's
# podman_epoch_args decides, which a publishing step does not source).
SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(podman image inspect --format '{{.Created.Unix}}' "$name")}
if [[ -n $SOURCE_DATE_EPOCH ]]; then
  if podman build --help 2>/dev/null | grep -q -- '--source-date-epoch'; then
    build_args+=(--source-date-epoch "$SOURCE_DATE_EPOCH" --rewrite-timestamp)
  else
    build_args+=(--timestamp "$SOURCE_DATE_EPOCH")
  fi
fi
podman build "${build_args[@]}" "$work" >/dev/null
installed=$(podman run --rm --pull=never --entrypoint cat "$name" \
  "/usr/lib/systemd/boot/efi/$loader_name.signed" | sha256sum | cut -d' ' -f1)
[[ $installed == "$(sha256sum < "$work/loader.efi.signed" | cut -d' ' -f1)" ]] \
  || die 'the signed boot loader in the image is not the one that was verified'

if [[ -f $in/image.oci.tar ]]; then
  podman push -q --compression-format zstd --compression-level 3 "$name" "oci-archive:$out/image.oci.tar"
else
  # Named, as os/image/build-layer.sh writes one: a layout descriptor without a
  # name loads with no repository digest, and the seal proves the image against it.
  podman push -q "$name" "oci:$out/oci:tilo"
fi
built_digest "$out" > "$out/digest"
podman run --rm --pull=never --entrypoint /bin/cat "$name" /usr/lib/tilo/generation > "$out/generation"
[[ $(cat "$out/generation") =~ ^[1-9][0-9]*$ ]] || die "$in carries no generation"
[[ ! -f $in/rpms-source ]] || cp "$in/rpms-source" "$out/rpms-source"
printf 'boot loader %s signed into %s (%s)\n' "$loader_name" "$out" "$(cat "$out/digest")"
