#!/usr/bin/env bash
# The EFI/PCR signer a release signs its boot artifacts with, in the protocol
# os/dev/sign-target-prediction.sh speaks (its `sign` phase, which runs nothing of the image):
#
#   boot-signer.sh export-efi-cert OUT          the EFI certificate signatures name
#   boot-signer.sh sign-efi IN OUT              IN with an Authenticode signature, untimestamped
#                                               (a timestamp would make the UKI irreproducible)
#   boot-signer.sh sign-sha256-rsa-pkcs1 IN OUT RSASSA-PKCS1-v1_5 over SHA-256 of IN, raw
#
# The keys stay in AWS KMS: the EFI key TILO_EFI_SIGNING_KMS_KEY, whose certificate is
# keys/efi-signing.crt (jsign asks KMS to sign the digest), and the PCR prediction authority
# TILO_PCR_SIGNING_KMS_KEY (keys/tpm2-pcr-signing.public.pem), both key or alias ARNs, signed with
# by the role whose credentials are in the environment (tools/ci/publish/lib.sh aws_role).
#
# With TILO_BOOT_SIGNER_DRY_RUN=DIR, throwaway keys made in DIR on first use sign instead, with
# no credentials: export-efi-cert also leaves the PCR public key beside OUT as dry-run-pcr.pem,
# which sign-target-prediction.sh's apply takes with TILO_BOOT_SIGNING_DRY_RUN=1.
set -euo pipefail
repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
die() { printf 'boot-signer: %s\n' "$*" >&2; exit 1; }

dry=${TILO_BOOT_SIGNER_DRY_RUN:-}
if [[ -n $dry ]]; then
  if [[ ! -s $dry/efi.key ]]; then
    mkdir -p "$dry"
    (umask 077
     openssl req -quiet -x509 -newkey rsa:2048 -nodes -days 1 -subj '/CN=Tilo dry-run EFI signing' \
       -addext keyUsage=digitalSignature -addext extendedKeyUsage=codeSigning \
       -keyout "$dry/efi.key" -out "$dry/efi.crt" 2>/dev/null
     openssl genpkey -quiet -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "$dry/pcr.key")
    openssl pkey -in "$dry/pcr.key" -pubout -out "$dry/pcr.pem"
  fi
else
  for key in TILO_EFI_SIGNING_KMS_KEY TILO_PCR_SIGNING_KMS_KEY; do
    [[ ${!key:-} =~ ^arn:aws:kms:([a-z0-9-]+):[0-9]{12}:(alias|key)/.+$ ]] || die "$key must be a KMS key or alias ARN"
  done
  [[ -n ${AWS_ACCESS_KEY_ID:-} && -n ${AWS_SECRET_ACCESS_KEY:-} ]] || die 'no AWS credentials to sign with'
fi
region() { [[ $1 =~ ^arn:aws:kms:([a-z0-9-]+): ]] && printf '%s' "${BASH_REMATCH[1]}"; }

case ${1:-} in
  export-efi-cert)
    out=${2:?usage: boot-signer.sh export-efi-cert OUT}
    if [[ -n $dry ]]; then
      cp "$dry/efi.crt" "$out"
      cp "$dry/pcr.pem" "$(dirname -- "$out")/dry-run-pcr.pem"
    else
      cp "$repo_root/keys/efi-signing.crt" "$out"
    fi ;;
  sign-efi)
    in=${2:?usage: boot-signer.sh sign-efi IN OUT}
    out=${3:?}
    cp "$in" "$out"
    if [[ -n $dry ]]; then
      jsign --keyfile "$dry/efi.key" --certfile "$dry/efi.crt" --alg SHA-256 "$out" >/dev/null
    else
      # jsign reads the credentials, session token included, from the environment.
      AWS_REGION=$(region "$TILO_EFI_SIGNING_KMS_KEY") jsign --storetype AWS \
        --keystore "$(region "$TILO_EFI_SIGNING_KMS_KEY")" --alias "$TILO_EFI_SIGNING_KMS_KEY" \
        --certfile "$repo_root/keys/efi-signing.crt" --alg SHA-256 "$out" >/dev/null
    fi ;;
  sign-sha256-rsa-pkcs1)
    in=${2:?usage: boot-signer.sh sign-sha256-rsa-pkcs1 IN OUT}
    out=${3:?}
    if [[ -n $dry ]]; then
      openssl dgst -sha256 -sign "$dry/pcr.key" -out "$out" "$in"
    else
      digest=$(mktemp)
      openssl dgst -sha256 -binary -out "$digest" "$in"
      aws kms sign --region "$(region "$TILO_PCR_SIGNING_KMS_KEY")" --key-id "$TILO_PCR_SIGNING_KMS_KEY" \
        --message "fileb://$digest" --message-type DIGEST --signing-algorithm RSASSA_PKCS1_V1_5_SHA_256 \
        --query Signature --output text | base64 -d > "$out"
      rm -f "$digest"
      openssl dgst -sha256 -verify "$repo_root/keys/tpm2-pcr-signing.public.pem" -signature "$out" "$in" >/dev/null \
        || die 'KMS signed with a key other than keys/tpm2-pcr-signing.public.pem'
    fi ;;
  *) die 'usage: boot-signer.sh export-efi-cert OUT | sign-efi IN OUT | sign-sha256-rsa-pkcs1 IN OUT' ;;
esac
