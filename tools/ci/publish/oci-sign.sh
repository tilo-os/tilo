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
# usage: oci-sign.sh REPOSITORY OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
repository=${1:?usage: oci-sign.sh REPOSITORY OUT}
out=${2:?}
[[ $(channel) == release ]] || die "only releases are signed; this run is for $T_REF"
expected=$(cat "$out/digest")
actual=$(archive_digest "$out/image.oci.tar")
[[ $actual == "$expected" ]] || die "the archive is $actual, the build recorded $expected"
use_cosign
. "$repo_root/os/dev/configure-cosign-kms.sh"
signing_payload "$repository" "$expected" > "$out/payload.json"
if [[ ${TILO_SIGN_TLOG:-} == 1 ]]; then
  cosign sign-blob --yes --key "$COSIGN_KEY_REF" --bundle "$out/bundle.json" \
    --output-signature "$out/signature" "$out/payload.json"
  cosign verify-blob --key "$repo_root/keys/cosign.pub" --bundle "$out/bundle.json" "$out/payload.json"
else
  cosign sign-blob --yes --tlog-upload=false --key "$COSIGN_KEY_REF" \
    --output-signature "$out/signature" "$out/payload.json"
  cosign verify-blob --insecure-ignore-tlog=true --key "$repo_root/keys/cosign.pub" \
    --signature "$out/signature" "$out/payload.json"
fi
printf 'signed %s@%s\n' "$repository" "$expected"
