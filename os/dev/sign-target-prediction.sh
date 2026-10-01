#!/usr/bin/env bash
# Measure the exact, already-resealed target UKI with the image's own
# systemd-measure, obtain signatures from a broker-owned executable, verify
# them with Tilo's pinned public key, and add only /boot sidecars to IMAGE.
set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

[ "$#" -eq 2 ] || die "usage: $0 IMAGE SIGNER"
image="$1"
signer="$2"
[[ "$signer" = /* && -x "$signer" && ! -L "$signer" ]] \
  || die "PCR prediction signer must be an absolute executable non-symlink"
[ "$(realpath -e "$signer")" = "$signer" ] \
  || die "PCR prediction signer path must be canonical"
case "$signer" in
  "$TILO_REPO"/*|"${RUNNER_TEMP:-/path-that-cannot-match}"/*)
    die "PCR prediction signer must not come from the checkout or job temporary directory" ;;
esac
current="$signer"
while [ "$current" != / ]; do
  read -r owner mode < <(stat -Lc '%u %a' "$current")
  [ "$owner" -eq 0 ] && [ $((8#$mode & 0022)) -eq 0 ] \
    || die "PCR prediction signer and its parents must be root-owned and not group/world writable: $current"
  current="$(dirname "$current")"
done

# Every signature and certificate check runs the image's own tilo-crypto, so
# native release runners need no separately installed copy of it.
for tool in objcopy objdump podman python3; do command -v "$tool" >/dev/null || die "missing $tool"; done
podman image exists "$image" || die "image is not present in local storage: $image"
efi_cert_sha256="${TILO_EFI_SIGNING_CERT_SHA256:-}"
case "$efi_cert_sha256" in
  *[!0-9a-f]*|'') die "TILO_EFI_SIGNING_CERT_SHA256 must pin the release EFI certificate" ;;
esac
[ "${#efi_cert_sha256}" -eq 64 ] \
  || die "TILO_EFI_SIGNING_CERT_SHA256 must be exactly 64 lowercase hex characters"
scratch_root="${TILO_INSTALLER_SCRATCH:-${TMPDIR:-/var/tmp}}"
work="$(mktemp -d "$scratch_root/tilo-pcr-prediction.XXXXXX")"
trap 'rm -rf "$work"' EXIT
install -d -m 0700 "$work/layer/boot/tilo-target/predictions" \
  "$work/layer/boot/EFI/tilo/pcr-predictions"

# Run the image's tilo-crypto over files in the workspace, mounted read-only.
image_crypto() {
  local command="$1" argument
  local -a arguments=()
  shift
  for argument in "$@"; do
    case "$argument" in
      "$work"/*) arguments+=("/work/${argument#"$work/"}") ;;
      *) die "tilo-crypto input is outside the signing workspace: $argument" ;;
    esac
  done
  podman run --rm -v "$work:/work:ro,Z" \
    --entrypoint /usr/bin/tilo-crypto "$image" "$command" "${arguments[@]}"
}

# Copy through a container so no image mount (and no root storage access) is
# exposed to this checkout-controlled script.
podman run --rm -v "$work:/out:Z" --entrypoint /bin/bash "$image" -ceu '
  install -d -m 0700 /out/live
  : > /out/live.manifest
  n=0
  while IFS= read -r source; do
    n=$((n + 1))
    install -m 0644 "$source" "/out/live/$n.efi"
    printf "%s\t%s\n" "${source#/boot/}" "$n.efi" >> /out/live.manifest
  done < <(find /boot/EFI/Linux -type f -name "*.efi" -print | sort)
  [ "$n" -gt 0 ]
  if [ -f /boot/tilo-target/tilo-target.efi ]; then
    source=/boot/tilo-target/tilo-target.efi
    echo separate > /out/target-source
  else
    [ "$n" -eq 1 ] || { echo "explicit target UKI required when multiple live UKIs exist" >&2; exit 1; }
    source="$(find /boot/EFI/Linux -type f -name "*.efi" -print -quit)"
    awk -F "\t" -v source="${source#/boot/}" "\$1 == source {print \$2}" \
      /out/live.manifest > /out/target-source
  fi
  [ -n "$source" ]
  install -m 0644 "$source" /out/target.efi
  install -m 0644 /usr/share/tilo/tpm2-pcr-signing.public.pem /out/public.pem
  cat /usr/lib/tilo/generation > /out/generation
'
generation="$(tr -d '[:space:]' < "$work/generation")"
case "$generation" in ''|*[!0-9]*) die "image has no valid Tilo generation" ;; esac
"$signer" export-efi-cert "$work/efi-signing.crt"
[ -f "$work/efi-signing.crt" ] && [ ! -L "$work/efi-signing.crt" ] \
  || die "broker did not export a regular EFI signing certificate"
actual_efi_cert_sha256="$(image_crypto certificate-sha256 "$work/efi-signing.crt")" \
  || die "broker did not export a valid X.509 EFI signing certificate"
[ "$actual_efi_cert_sha256" = "$efi_cert_sha256" ] \
  || die "broker's EFI signing certificate does not match the independently pinned fingerprint"

verify_signed_efi() {
  local original="$1" signed="$2" label="$3" count
  [ -f "$signed" ] && [ ! -L "$signed" ] \
    || die "broker did not return a regular signed $label"
  count="$(podman run --rm -v "$work:/work:ro,Z" \
    --entrypoint /usr/bin/tilo-crypto "$image" efi-signature-count \
    "/work/${signed#"$work/"}")" \
    || die "broker did not return a structurally valid signed $label"
  case "$count" in ''|*[!0-9]*|0) die "broker returned an unsigned $label" ;; esac
  podman run --rm -v "$work:/work:ro,Z" \
    --entrypoint /usr/bin/tilo-crypto "$image" verify-efi /work/efi-signing.crt \
    "/work/${signed#"$work/"}" >/dev/null \
    || die "broker signed $label with an identity other than the pinned EFI certificate"

  "$TILO_OS_DIR/dev/verify-authenticode-envelope.py" "$original" "$signed" \
    || die "broker changed bytes outside the Authenticode envelope of $label"
}

# A measured but unsigned target is not bootable under the Secure Boot state
# whose PCR7 we bind. The broker's EFI operation uses the non-exportable key of
# the pinned Tilo release certificate, which owners enroll in firmware db (see
# docs/security/boot-chain.md), and no timestamp (reproducibility).
while IFS=$'\t' read -r destination source; do
  case "$destination" in EFI/Linux/*.efi) : ;; *) die "unsafe live UKI path from image" ;; esac
  "$signer" sign-efi "$work/live/$source" "$work/live/$source.signed"
  verify_signed_efi "$work/live/$source" "$work/live/$source.signed" "live EFI image"
  install -Dm0644 "$work/live/$source.signed" "$work/layer/boot/$destination"
done < "$work/live.manifest"
target_source="$(cat "$work/target-source")"
if [ "$target_source" = separate ]; then
  "$signer" sign-efi "$work/target.efi" "$work/target.efi.signed"
  verify_signed_efi "$work/target.efi" "$work/target.efi.signed" "target EFI image"
  mv "$work/target.efi.signed" "$work/target.efi"
else
  [ -s "$work/live/$target_source.signed" ] || die "could not bind the signed release UKI"
  cp "$work/live/$target_source.signed" "$work/target.efi"
fi
# Project the exact selected and signature-verified UKI for consumers that
# require an explicit target. This changes only /boot, outside the root digest.
install -Dm0644 "$work/target.efi" \
  "$work/layer/boot/tilo-target/tilo-target.efi"

# systemd-measure is the implementation systemd-stub is paired with. Feed it
# every UKI component it documents as measured; refuse unknown allocatable UKI
# payload sections instead of silently predicting only part of a future format.
declare -A measure_opt=(
  [linux]=linux [osrel]=osrel [cmdline]=cmdline [initrd]=initrd [ucode]=ucode
  [splash]=splash [dtb]=dtb [dtbauto]=dtbauto [uname]=uname [sbat]=sbat
  [pcrpkey]=pcrpkey [profile]=profile [hwids]=hwids
)
measure_args=()
while read -r section; do
  name="${section#.}"
  if [ -n "${measure_opt[$name]:-}" ]; then
    objcopy --dump-section "$section=$work/$name" \
      "$work/target.efi" "$work/ignored.efi"
    measure_args+=("--${measure_opt[$name]}=/work/$name")
  else
    case "$section" in
      .text|.rodata|.data|.sdmagic|.reloc|.pcrsig) : ;;
      *) die "target UKI contains an unclassified section $section; update the authoritative measurement mapping" ;;
    esac
  fi
done < <(objdump -h "$work/target.efi" | awk '$2 ~ /^\./ {print $2}')
rm -f "$work/ignored.efi"
[ -s "$work/linux" ] || die "target UKI has no measurable .linux section"

podman run --rm -v "$work:/work:ro,Z" \
  --entrypoint /usr/lib/systemd/systemd-measure "$image" \
  policy-digest --json=short --bank=sha256 --phase=enter-initrd \
  "${measure_args[@]}" > "$work/policy.json"
python3 - "$work/policy.json" "$work/approved.bin" <<'PY'
import json, sys
source, output = sys.argv[1:]
d = json.load(open(source, encoding="utf-8"))
try:
    rows = d["sha256"]
except (KeyError, TypeError):
    raise SystemExit("systemd-measure returned no sha256 policy")
matches = [r for r in rows if r.get("pcrs") == [11] and isinstance(r.get("pol"), str)]
if len(matches) != 1 or len(matches[0]["pol"]) != 64:
    raise SystemExit("systemd-measure returned an ambiguous PCR11 policy")
open(output, "wb").write(bytes.fromhex(matches[0]["pol"]))
PY

prediction="$work/layer/boot/tilo-target/predictions/generation-$generation.json"
signature="$prediction.signature"
python3 - "$work/approved.bin" "$work/message.bin" "$generation" <<'PY'
import struct, sys
approved, output, generation = sys.argv[1:]
open(output, "wb").write(open(approved, "rb").read() + struct.pack(">Q", int(generation)))
PY
"$signer" sign-sha256-rsa-pkcs1 "$work/message.bin" "$signature"
image_crypto verify-rsa-sha256 "$work/public.pem" "$work/message.bin" "$signature" \
  || die "broker signed the prediction with a key other than Tilo's pinned authority"
python3 - "$work/approved.bin" "$signature" "$prediction" "$generation" <<'PY'
import base64, json, sys
approved, signature, output, generation = sys.argv[1:]
json.dump({"generation": int(generation), "pcr_bank": "sha256", "pcrs": [11],
           "approved_policy": base64.b64encode(open(approved, "rb").read()).decode(),
           "signature": base64.b64encode(open(signature, "rb").read()).decode(),
           "label": "systemd-measure enter-initrd"}, open(output, "w"), separators=(",", ":"))
PY
rm -f "$signature"

"$signer" sign-sha256-rsa-pkcs1 "$work/target.efi" "$prediction.uki.sig"
image_crypto verify-rsa-sha256-efi "$work/public.pem" "$work/target.efi" "$prediction.uki.sig" \
  || die "broker's detached target-UKI signature does not match Tilo's pinned authority"
install -m 0644 "$prediction" "$prediction.uki.sig" \
  "$work/layer/boot/EFI/tilo/pcr-predictions/"

# Signature output directories are private while signer output is created;
# the finalized layer contains public-only files with normal directory modes.
chmod 0755 "$work/layer/boot/tilo-target/predictions" \
  "$work/layer/boot/EFI/tilo/pcr-predictions"

build_args=(-t "$image" -f - --build-arg "BASE_IMAGE=$image")
if [ -n "${SOURCE_DATE_EPOCH:-}" ]; then
  mapfile -t reproducible < <(podman_epoch_args "$SOURCE_DATE_EPOCH")
  build_args+=("${reproducible[@]}")
fi
podman build "${build_args[@]}" "$work/layer" <<'EOF'
ARG BASE_IMAGE
FROM ${BASE_IMAGE}
COPY boot/ /boot/
EOF
msg "signed PCR11 prediction for generation $generation over the exact resealed target UKI"
