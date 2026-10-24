#!/usr/bin/env bash
# Measure the exact, already-resealed target UKI with the image's own
# systemd-measure, obtain signatures from a broker-owned executable, verify
# them with Tilo's pinned public key, and add only /boot sidecars to IMAGE.
#
#   sign-target-prediction.sh IMAGE SIGNER           all three phases below, in one
#   sign-target-prediction.sh prepare IMAGE REQUEST  what is to be signed, into REQUEST: every
#                                                    live UKI, the target and the PCR 11 policy
#                                                    message of its generation; no signer
#   sign-target-prediction.sh sign REQUEST SIGNER    the signer's half: it reads and writes
#                                                    REQUEST only, runs nothing of the image
#   sign-target-prediction.sh apply IMAGE REQUEST    every signature verified with the image's
#                                                    own tilo-crypto against the pinned authority,
#                                                    then the /boot sidecars added to IMAGE
#
# The phases let the signing run apart from the image, as a release does: the
# step holding the signing keys never runs code of what it signs. SIGNER speaks
# `export-efi-cert OUT`, `sign-efi IN OUT` and `sign-sha256-rsa-pkcs1 IN OUT`.
# With TILO_BOOT_SIGNING_DRY_RUN=1, apply verifies against the certificate and
# PCR public key the signer left in REQUEST (a throwaway signer's), not the
# pinned ones: a rehearsal, never a release.
set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

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
    --entrypoint /usr/libexec/tilo/tilo-crypto "$image" "$command" "${arguments[@]}"
}

require_signer() {
  [[ "$signer" = /* && -x "$signer" && ! -L "$signer" ]] \
    || die "PCR prediction signer must be an absolute executable non-symlink"
  [ "$(realpath -e "$signer")" = "$signer" ] \
    || die "PCR prediction signer path must be canonical"
}

# The broker's signer in one run with the image: it must not come from what the
# run could change, and nothing writable by others may lead to it.
require_owned_signer() {
  local current
  require_signer
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
}

# The pinned EFI certificate's fingerprint, or in a dry run nothing (the signer's own).
pinned_efi_cert_sha256() {
  local efi_cert_sha256="${TILO_EFI_SIGNING_CERT_SHA256:-}"
  case "$efi_cert_sha256" in
    *[!0-9a-f]*|'') die "TILO_EFI_SIGNING_CERT_SHA256 must pin the release EFI certificate" ;;
  esac
  [ "${#efi_cert_sha256}" -eq 64 ] \
    || die "TILO_EFI_SIGNING_CERT_SHA256 must be exactly 64 lowercase hex characters"
  printf '%s' "$efi_cert_sha256"
}

# systemd-measure's PCR 11 policy for the UKI in $work/$1, as $work/$2, and the message the
# authority signs for this generation, as $work/message.bin.
measure() {
  local uki="$work/$1" approved="$work/$2" section name
  # systemd-measure is the implementation systemd-stub is paired with. Feed it
  # every UKI component it documents as measured; refuse unknown allocatable UKI
  # payload sections instead of silently predicting only part of a future format.
  declare -A measure_opt=(
    [linux]=linux [osrel]=osrel [cmdline]=cmdline [initrd]=initrd [ucode]=ucode
    [splash]=splash [dtb]=dtb [dtbauto]=dtbauto [uname]=uname [sbat]=sbat
    [pcrpkey]=pcrpkey [profile]=profile [hwids]=hwids
  )
  local -a measure_args=()
  rm -rf "$work/sections"
  mkdir "$work/sections"
  while read -r section; do
    name="${section#.}"
    if [ -n "${measure_opt[$name]:-}" ]; then
      objcopy --dump-section "$section=$work/sections/$name" \
        "$uki" "$work/sections/ignored.efi"
      measure_args+=("--${measure_opt[$name]}=/work/sections/$name")
    else
      case "$section" in
        .text|.rodata|.data|.sdmagic|.reloc|.pcrsig) : ;;
        *) die "target UKI contains an unclassified section $section; update the authoritative measurement mapping" ;;
      esac
    fi
  done < <(objdump -h "$uki" | awk '$2 ~ /^\./ {print $2}')
  rm -f "$work/sections/ignored.efi"
  [ -s "$work/sections/linux" ] || die "target UKI has no measurable .linux section"

  podman run --rm -v "$work:/work:ro,Z" \
    --entrypoint /usr/lib/systemd/systemd-measure "$image" \
    policy-digest --json=short --bank=sha256 --phase=enter-initrd \
    "${measure_args[@]}" > "$work/policy.json"
  python3 - "$work/policy.json" "$approved" <<'PY'
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
  rm -rf "$work/sections" "$work/policy.json"
}

prepare() {
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
  cat /usr/lib/tilo/generation > /out/generation
'
  generation="$(tr -d '[:space:]' < "$work/generation")"
  case "$generation" in ''|*[!0-9]*) die "image has no valid Tilo generation" ;; esac
  while IFS=$'\t' read -r destination _; do
    case "$destination" in EFI/Linux/*.efi) : ;; *) die "unsafe live UKI path from image" ;; esac
  done < "$work/live.manifest"
  # Authenticode signing changes no measured section, so the unsigned target's
  # policy is the signed one's; apply measures the signed target again.
  measure target.efi approved.bin
  python3 - "$work/approved.bin" "$work/message.bin" "$generation" <<'PY'
import struct, sys
approved, output, generation = sys.argv[1:]
open(output, "wb").write(open(approved, "rb").read() + struct.pack(">Q", int(generation)))
PY
}

# A measured but unsigned target is not bootable under the Secure Boot state
# whose PCR7 we bind. The broker's EFI operation uses the non-exportable key of
# the pinned Tilo release certificate, which owners enroll in firmware db (see
# docs/security/boot-chain.md), and no timestamp (reproducibility).
sign() {
  local destination source
  [ -s "$work/message.bin" ] && [ -s "$work/live.manifest" ] || die "$work holds no signing request"
  "$signer" export-efi-cert "$work/efi-signing.crt"
  while IFS=$'\t' read -r destination source; do
    case "$source" in [0-9]*.efi) : ;; *) die "unsafe live UKI name in the request" ;; esac
    "$signer" sign-efi "$work/live/$source" "$work/live/$source.signed"
  done < "$work/live.manifest"
  source="$(cat "$work/target-source")"
  case "$source" in
    separate) "$signer" sign-efi "$work/target.efi" "$work/target.efi.signed" ;;
    [0-9]*.efi) cp "$work/live/$source.signed" "$work/target.efi.signed" ;;
    *) die "unsafe target in the request" ;;
  esac
  "$signer" sign-sha256-rsa-pkcs1 "$work/message.bin" "$work/message.sig"
  "$signer" sign-sha256-rsa-pkcs1 "$work/target.efi.signed" "$work/target.uki.sig"
}

verify_signed_efi() {
  local original="$1" signed="$2" label="$3" count
  [ -f "$signed" ] && [ ! -L "$signed" ] \
    || die "broker did not return a regular signed $label"
  count="$(image_crypto efi-signature-count "$signed")" \
    || die "broker did not return a structurally valid signed $label"
  case "$count" in ''|*[!0-9]*|0) die "broker returned an unsigned $label" ;; esac
  image_crypto verify-efi "$work/efi-signing.crt" "$signed" >/dev/null \
    || die "broker signed $label with an identity other than the pinned EFI certificate"

  "$TILO_OS_DIR/dev/verify-authenticode-envelope.py" "$original" "$signed" \
    || die "broker changed bytes outside the Authenticode envelope of $label"
}

apply() {
  local destination source target_source public generation prediction
  generation="$(tr -d '[:space:]' < "$work/generation")"
  case "$generation" in ''|*[!0-9]*) die "the request names no valid Tilo generation" ;; esac
  # The authority the image itself trusts, and the EFI certificate pinned apart from it.
  if [ "${TILO_BOOT_SIGNING_DRY_RUN:-0}" = 1 ]; then
    [ -s "$work/dry-run-pcr.pem" ] || die "a dry run's signer leaves its PCR public key"
    public="$work/dry-run-pcr.pem"
  else
    podman run --rm --entrypoint /bin/cat "$image" /usr/share/tilo/tpm2-pcr-signing.public.pem > "$work/public.pem"
    public="$work/public.pem"
    [ -f "$work/efi-signing.crt" ] && [ ! -L "$work/efi-signing.crt" ] \
      || die "broker did not export a regular EFI signing certificate"
    [ "$(image_crypto certificate-sha256 "$work/efi-signing.crt")" = "$(pinned_efi_cert_sha256)" ] \
      || die "broker's EFI signing certificate does not match the independently pinned fingerprint"
  fi
  rm -rf "$work/layer"
  install -d -m 0700 "$work/layer/boot/tilo-target/predictions" \
    "$work/layer/boot/EFI/tilo/pcr-predictions"
  while IFS=$'\t' read -r destination source; do
    case "$destination" in EFI/Linux/*.efi) : ;; *) die "unsafe live UKI path from image" ;; esac
    verify_signed_efi "$work/live/$source" "$work/live/$source.signed" "live EFI image"
    install -Dm0644 "$work/live/$source.signed" "$work/layer/boot/$destination"
  done < "$work/live.manifest"
  target_source="$(cat "$work/target-source")"
  if [ "$target_source" = separate ]; then
    verify_signed_efi "$work/target.efi" "$work/target.efi.signed" "target EFI image"
  else
    [ -s "$work/live/$target_source.signed" ] || die "could not bind the signed release UKI"
    cmp -s "$work/live/$target_source.signed" "$work/target.efi.signed" \
      || die "the signed target is not the signed live UKI it is"
  fi
  # Project the exact selected and signature-verified UKI for consumers that
  # require an explicit target. This changes only /boot, outside the root digest.
  install -Dm0644 "$work/target.efi.signed" \
    "$work/layer/boot/tilo-target/tilo-target.efi"

  measure target.efi.signed signed-approved.bin
  cmp -s "$work/approved.bin" "$work/signed-approved.bin" \
    || die "signing the target changed its PCR 11 policy"
  image_crypto verify-rsa-sha256 "$public" "$work/message.bin" "$work/message.sig" \
    || die "broker signed the prediction with a key other than Tilo's pinned authority"
  prediction="$work/layer/boot/tilo-target/predictions/generation-$generation.json"
  python3 - "$work/approved.bin" "$work/message.sig" "$prediction" "$generation" <<'PY'
import base64, json, sys
approved, signature, output, generation = sys.argv[1:]
json.dump({"generation": int(generation), "pcr_bank": "sha256", "pcrs": [11],
           "approved_policy": base64.b64encode(open(approved, "rb").read()).decode(),
           "signature": base64.b64encode(open(signature, "rb").read()).decode(),
           "label": "systemd-measure enter-initrd"}, open(output, "w"), separators=(",", ":"))
PY
  image_crypto verify-rsa-sha256-efi "$public" "$work/target.efi.signed" "$work/target.uki.sig" \
    || die "broker's detached target-UKI signature does not match Tilo's pinned authority"
  install -m 0644 "$work/target.uki.sig" "$prediction.uki.sig"
  install -m 0644 "$prediction" "$prediction.uki.sig" \
    "$work/layer/boot/EFI/tilo/pcr-predictions/"

  # Signature output directories are private while signer output is created;
  # the finalized layer contains public-only files with normal directory modes.
  chmod 0755 "$work/layer/boot/tilo-target/predictions" \
    "$work/layer/boot/EFI/tilo/pcr-predictions"

  local -a build_args=(-t "$image" -f - --build-arg "BASE_IMAGE=$image")
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
}

# Every signature and certificate check runs the image's own tilo-crypto, so
# native release runners need no separately installed copy of it.
need_image() {
  local tool
  for tool in objcopy objdump podman python3; do command -v "$tool" >/dev/null || die "missing $tool"; done
  podman image exists "$image" || die "image is not present in local storage: $image"
}

case "${1:-}" in
  prepare)
    [ "$#" -eq 3 ] || die "usage: $0 prepare IMAGE REQUEST"
    image="$2" work="$3"
    need_image
    rm -rf "$work"
    install -d -m 0700 "$work"
    work="$(cd "$work" && pwd)"
    prepare ;;
  sign)
    [ "$#" -eq 3 ] || die "usage: $0 sign REQUEST SIGNER"
    work="$(cd "$2" && pwd)" signer="$3"
    require_signer
    sign ;;
  apply)
    [ "$#" -eq 3 ] || die "usage: $0 apply IMAGE REQUEST"
    image="$2" work="$(cd "$3" && pwd)"
    need_image
    apply ;;
  *)
    [ "$#" -eq 2 ] || die "usage: $0 IMAGE SIGNER | prepare IMAGE REQUEST | sign REQUEST SIGNER | apply IMAGE REQUEST"
    image="$1" signer="$2"
    require_owned_signer
    need_image
    pinned_efi_cert_sha256 >/dev/null
    scratch_root="${TILO_INSTALLER_SCRATCH:-${TMPDIR:-/var/tmp}}"
    work="$(mktemp -d "$scratch_root/tilo-pcr-prediction.XXXXXX")"
    trap 'rm -rf "$work"' EXIT
    prepare
    sign
    apply ;;
esac
