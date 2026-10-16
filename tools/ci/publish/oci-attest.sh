#!/usr/bin/env bash
# The provenance half of signing: sign OUT/provenance.json, a SLSA provenance predicate the
# chain's provenance check wrote for the image OUT/image.oci.tar will be published as, into an
# in-toto attestation for that image's digest (OUT/attestation.json, a DSSE envelope), with the
# release key in KMS. Like oci-sign.sh it holds signing credentials and no registry token;
# oci-publish.sh attaches the attestation, so `cosign verify-attestation --type slsaprovenance1`
# finds it on the published image.
#
# The predicate names the commit it was built from as `{commit}`, which only a trusted run knows
# (T_COMMIT); it is filled in here. Unless TILO_SIGN_TLOG=0 the attestation is entered in the
# public transparency log, which a plain `cosign verify-attestation --key keys/cosign.pub`
# (os/dev/resolve-rpm-snapshot.sh, os/dev/verify-kernel-provenance.sh) requires.
#
# With TILO_PUBLISH_DRY_RUN set, the throwaway key oci-sign.sh's dry run made signs instead,
# with no identity token and no transparency log.
#
# usage: oci-attest.sh REPOSITORY OUT (after oci-sign.sh REPOSITORY OUT)
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
repository=${1:?usage: oci-attest.sh REPOSITORY OUT}
out=${2:?}
[[ $(channel) == release ]] || die "only releases are attested; this run is for $T_REF"
require_commit
expected=$(cat "$out/digest")
[[ $(archive_digest "$out/image.oci.tar") == "$expected" ]] || die 'the archive is not the build'
[[ -s $out/provenance.json ]] || die "no provenance predicate in $out"
grep -q "\"expectedReleaseDigest\": *\"$expected\"" "$out/provenance.json" \
  || die "the provenance predicate is not for $expected"
use_cosign
predicate=$scratch/provenance-$(basename -- "$out").json
sed "s/{commit}/$T_COMMIT/g" "$out/provenance.json" > "$predicate"
args=(--yes --type slsaprovenance1 --predicate "$predicate" --hash "${expected#sha256:}")
if [[ -n ${TILO_PUBLISH_DRY_RUN:-} ]]; then
  [[ -e $scratch/dry-run-signing/cosign.key ]] || die 'no dry-run key: run oci-sign.sh first'
  COSIGN_PASSWORD= cosign attest-blob "${args[@]}" --tlog-upload=false \
    --key "$scratch/dry-run-signing/cosign.key" "$repository" > "$out/attestation.json"
else
  . "$repo_root/os/dev/configure-cosign-kms.sh"
  tlog=(--tlog-upload=true)
  [[ ${TILO_SIGN_TLOG:-1} == 0 ]] && tlog=(--tlog-upload=false)
  cosign attest-blob "${args[@]}" "${tlog[@]}" --key "$COSIGN_KEY_REF" "$repository" > "$out/attestation.json"
fi
grep -q '"payloadType":"application/vnd.in-toto+json"' "$out/attestation.json" \
  || die 'cosign wrote no attestation envelope'
printf 'attested %s@%s\n' "$repository" "$expected"
