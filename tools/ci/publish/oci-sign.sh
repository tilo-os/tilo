#!/usr/bin/env bash
# The signing half of an image: sign the image OUT/image.oci.tar will be published as, with the
# release key in KMS (keys/cosign.pub), and write the signature beside it (OUT/payload.json,
# OUT/signature). It holds signing credentials and nothing else: no registry token, so it
# publishes nothing itself. It refuses to sign anything but the digest the build produced.
#
# With TILO_SIGN_TLOG=1 the signature is also entered in the public transparency log (Rekor) and
# its entry kept as OUT/bundle.json, which oci-publish.sh attaches: images that release builds
# verify with a plain `cosign verify --key keys/cosign.pub` (the kernel buildroot, the
# env-runtime) need that log entry, since such a verification requires one.
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
# `dev_signed` in products.toml): its signature says the candidate came from main's build, and
# no release verification accepts that key.
run_channel=$(channel)
if [[ ${TILO_DEV_SIGNING:-} == 1 ]]; then
  [[ $run_channel == dev ]] || die "the development key signs main's candidates only; this run is for $T_REF"
  [[ ${COSIGN_KEY_REF:-} == *alias/tilo-dev-signing || -n ${TILO_PUBLISH_DRY_RUN:-} ]] \
    || die 'development signing uses the development key alias/tilo-dev-signing'
  public_key=$repo_root/keys/cosign-dev.pub
else
  [[ $run_channel == release ]] || die "only releases are signed; this run is for $T_REF"
  public_key=$repo_root/keys/cosign.pub
fi
expected=$(cat "$out/digest")
actual=$(archive_digest "$out/image.oci.tar")
[[ $actual == "$expected" ]] || die "the archive is $actual, the build recorded $expected"
rm -f "$out/dry-run.pub"
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
printf 'signed %s@%s\n' "$repository" "$expected"
