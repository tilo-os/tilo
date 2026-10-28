#!/usr/bin/env bash
# The provenance half of signing: sign OUT/provenance.json, a SLSA provenance predicate the
# chain's provenance check wrote for the image OUT/image.oci.tar will be published as, into an
# in-toto attestation for that image's digest (OUT/attestation.json, a DSSE envelope), with the
# release key in KMS. Like oci-sign.sh it holds signing credentials and no registry token;
# oci-publish.sh attaches the attestation, so `cosign verify-attestation --type slsaprovenance1`
# finds it on the published image.
#
# The predicate names the commit it was built from as `{commit}`, which only a trusted run knows
# (T_COMMIT); it is filled in here. With TILO_SIGN_TLOG=1 the attestation is entered in the
# public transparency log, which a plain `cosign verify-attestation --key keys/cosign.pub`
# (os/dev/resolve-rpm-snapshot.sh, os/dev/verify-kernel-provenance.sh) requires, and the entry
# kept as OUT/attestation.bundle.json.
#
# With TILO_DEV_SIGNING=1 (`dev_signed`) it attests a development candidate of main with the
# development key (keys/cosign-dev.pub), as oci-sign.sh signs it.
#
# With TILO_PUBLISH_DRY_RUN set, the throwaway key oci-sign.sh's dry run made signs instead,
# with no identity token and no transparency log.
#
# With PREDICATE and TYPE, OUT/PREDICATE is attested as TYPE instead, into
# OUT/<PREDICATE without .json>.attestation.json (the OS release's freshness statement).
#
# usage: oci-attest.sh REPOSITORY OUT [PREDICATE TYPE] (after oci-sign.sh REPOSITORY OUT)
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
repository=${1:?usage: oci-attest.sh REPOSITORY OUT [PREDICATE TYPE]}
out=${2:?}
name=${3:-provenance.json}
type=${4:-slsaprovenance1}
envelope=$out/attestation.json
[[ $name == provenance.json ]] || envelope=$out/${name%.json}.attestation.json
run_channel=$(channel)
if [[ ${TILO_DEV_SIGNING:-} == 1 ]]; then
  [[ $run_channel == dev ]] || die "the development key attests main's candidates only; this run is for $T_REF"
  [[ ${COSIGN_KEY_REF:-} == *alias/tilo-dev-signing || -n ${TILO_PUBLISH_DRY_RUN:-} ]] \
    || die 'development attestations use the development key alias/tilo-dev-signing'
else
  [[ $run_channel == release ]] || die "only releases are attested; this run is for $T_REF"
fi
require_commit
expected=$(cat "$out/digest")
[[ $(archive_digest "$out/image.oci.tar") == "$expected" ]] || die 'the archive is not the build'
[[ -s $out/$name ]] || die "no predicate $name in $out"
if [[ $name == provenance.json ]]; then
  grep -q "\"expectedReleaseDigest\": *\"$expected\"" "$out/$name" \
    || die "the provenance predicate is not for $expected"
else
  grep -q "\"$expected\"" "$out/$name" || die "$name does not name $expected"
fi
use_cosign
predicate=$scratch/${name%.json}-$(basename -- "$out").json
sed "s/{commit}/$T_COMMIT/g" "$out/$name" > "$predicate"
args=(--yes --type "$type" --predicate "$predicate" --hash "${expected#sha256:}")
if [[ -n ${TILO_PUBLISH_DRY_RUN:-} ]]; then
  [[ -e $scratch/dry-run-signing/cosign.key ]] || die 'no dry-run key: run oci-sign.sh first'
  COSIGN_PASSWORD= cosign attest-blob "${args[@]}" --tlog-upload=false \
    --key "$scratch/dry-run-signing/cosign.key" "$repository" > "$envelope"
else
  . "$repo_root/os/dev/configure-cosign-kms.sh"
  tlog=(--tlog-upload=false)
  rm -f "${envelope%.json}.bundle.json"
  [[ ${TILO_SIGN_TLOG:-} == 1 ]] && tlog=(--tlog-upload=true --bundle "${envelope%.json}.bundle.json")
  cosign attest-blob "${args[@]}" "${tlog[@]}" --key "$COSIGN_KEY_REF" "$repository" > "$envelope"
fi
grep -q '"payloadType":"application/vnd.in-toto+json"' "$envelope" \
  || die 'cosign wrote no attestation envelope'
printf 'attested %s@%s\n' "$repository" "$expected"
