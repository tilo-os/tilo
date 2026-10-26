#!/usr/bin/env bash
# The signing half of an image: sign the image OUT/image.oci.tar (or the layout OUT/oci) will be
# published as, with the release key in KMS (keys/cosign.pub), and write the signature beside it
# (OUT/payload.json, OUT/signature). It holds signing credentials and nothing else: no registry
# token, so it publishes nothing itself. It refuses to sign anything but the digest the build
# produced.
#
# With TILO_SIGN_TLOG=1 the signature is also entered in the public transparency log (Rekor) and
# its entry kept as OUT/bundle.json, which oci-publish.sh attaches: images that release builds
# verify with a plain `cosign verify --key keys/cosign.pub` (the kernel buildroot, the
# env-runtime) need that log entry, since such a verification requires one.
#
# A repository installed systems verify (lib.sh device_verified) gets, once the release root
# exists, one more signature per operational certificate (keys/operational/os-images.crt and, after
# a revocation, its .bridge.crt): tools/signing/sign-blob-certified.sh logs it under the
# certificate, and OUT/certified/N.{sig,bundle,crt,epoch} keep it for oci-publish.sh.
#
# With TILO_PUBLISH_DRY_RUN set, a throwaway key made here signs instead of the one in KMS, with
# no identity token and no transparency log, and its public half is kept as OUT/dry-run.pub for
# oci-publish.sh's dry run to verify against. T_REF says what the run pretends to sign for.
#
# usage: oci-sign.sh REPOSITORY OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
repository=${1:?usage: oci-sign.sh REPOSITORY OUT}
out=${2:?}
# A development candidate may be signed too, with the development key only (TILO_DEV_SIGNING=1,
# `dev_signed` in products.toml): its signature says the candidate came from t's trusted build of
# main or of a branch of this repository, and no release verification accepts that key.
run_channel=$(channel)
if [[ ${TILO_DEV_SIGNING:-} == 1 ]]; then
  [[ $run_channel == dev || $run_channel == branch ]] \
    || die "the development key signs development candidates only; this run is for $T_REF"
  [[ ${COSIGN_KEY_REF:-} == *alias/tilo-dev-signing || -n ${TILO_PUBLISH_DRY_RUN:-} ]] \
    || die 'development signing uses the development key alias/tilo-dev-signing'
  public_key=$repo_root/keys/cosign-dev.pub
else
  [[ $run_channel == release ]] || die "only releases are signed; this run is for $T_REF"
  public_key=$repo_root/keys/cosign.pub
fi
expected=$(cat "$out/digest")
actual=$(built_digest "$out")
[[ $actual == "$expected" ]] || die "the image is $actual, the build recorded $expected"
rm -rf "$out/dry-run.pub" "$out/certified"
use_cosign
signing_payload "$repository" "$expected" > "$out/payload.json"
if [[ -n ${TILO_PUBLISH_DRY_RUN:-} ]]; then
  keys=$scratch/dry-run-signing
  rm -rf "$keys"
  mkdir -p "$keys"
  (cd "$keys" && COSIGN_PASSWORD= cosign generate-key-pair >/dev/null)
  COSIGN_PASSWORD= cosign sign-blob --yes --tlog-upload=false --key "$keys/cosign.key" \
    --output-signature "$out/signature" "$out/payload.json"
  cp "$keys/cosign.pub" "$out/dry-run.pub"
  cosign verify-blob --insecure-ignore-tlog=true --key "$out/dry-run.pub" \
    --signature "$out/signature" "$out/payload.json"
  printf 'signed %s@%s with a throwaway key (dry run)\n' "$repository" "$expected"
  exit 0
fi
. "$repo_root/os/dev/configure-cosign-kms.sh"
if [[ ${TILO_SIGN_TLOG:-} == 1 ]]; then
  cosign sign-blob --yes --key "$COSIGN_KEY_REF" --bundle "$out/bundle.json" \
    --output-signature "$out/signature" "$out/payload.json"
  cosign verify-blob --key "$public_key" --bundle "$out/bundle.json" "$out/payload.json"
else
  cosign sign-blob --yes --tlog-upload=false --key "$COSIGN_KEY_REF" \
    --output-signature "$out/signature" "$out/payload.json"
  cosign verify-blob --insecure-ignore-tlog=true --key "$public_key" \
    --signature "$out/signature" "$out/payload.json"
fi
if [[ $run_channel == release && -e $repo_root/keys/release/root.crt ]] && device_verified "$repository"; then
  . "$repo_root/keys/release/trust.env"
  mkdir -p "$out/certified"
  n=0
  for cert in "$repo_root/keys/operational/os-images.crt" "$repo_root/keys/operational/os-images.bridge.crt"; do
    [[ -e $cert ]] || continue
    epoch=$TILO_RELEASE_EPOCH
    [[ $cert == *.bridge.crt ]] && epoch=$((TILO_RELEASE_EPOCH - 1))
    n=$((n + 1))
    bash "$repo_root/tools/signing/sign-blob-certified.sh" os-images "$cert" "$out/payload.json" \
      "$out/certified/$n" "$epoch"
    cp "$cert" "$out/certified/$n.crt"
    printf '%s\n' "$epoch" > "$out/certified/$n.epoch"
  done
  ((n)) || die "keys/release/root.crt exists but keys/operational/os-images.crt does not"
fi
printf 'signed %s@%s\n' "$repository" "$expected"
