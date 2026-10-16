#!/usr/bin/env bash
# The FEX payload candidate's limited SLSA provenance (development-fex-payload/v1, as
# build-windows-compat.yml recorded it): a development build whose runtime acceptance nothing
# has verified, naming the commit (left as {commit} for oci-attest.sh), the FEX buildroot and
# the x86 SDK it was compiled with, by the digests they are published as, and the payload's own
# digest. Written to OUT/provenance.json; holds no secrets.
#
# usage: fex-provenance.sh OUT BUILDROOT_OUT SDK_OUT
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
out=${1:?usage: fex-provenance.sh OUT BUILDROOT_OUT SDK_OUT}
buildroot_out=${2:?}
sdk_out=${3:?}
digest() {
  local d
  d=$(cat "$1/digest")
  [[ $d =~ ^sha256:[0-9a-f]{64}$ ]] || die "not a digest in $1: $d"
  printf '%s' "$d"
}
payload=$(digest "$out")
buildroot=registry.tilo.so/tilo/tilo-fex-buildroot-candidate@$(digest "$buildroot_out")
sdk=registry.tilo.so/tilo/tilo-fex-x86-sdk-candidate@$(digest "$sdk_out")
printf '%s\n' '{' \
  '  "buildDefinition": {' \
  '    "buildType": "https://tilo.sh/buildtypes/development-fex-payload/v1",' \
  '    "externalParameters": {"developmentOnly": true, "runtimeAcceptanceVerified": false},' \
  "    \"internalParameters\": {\"expectedReleaseDigest\": \"$payload\"}," \
  "    \"resolvedDependencies\": [{\"uri\": \"git+ssh://code.tilo.so/tilo/tilo@{commit}\"}, {\"uri\": \"$buildroot\"}, {\"uri\": \"$sdk\"}]" \
  '  },' \
  "  \"runDetails\": {\"builder\": {\"id\": \"https://code.tilo.so/t/checks/publish-fex-payload\"}, \"metadata\": {\"output\": \"registry.tilo.so/tilo/tilo-fex-payload-candidate@$payload\"}}" \
  '}' > "$out/provenance.json"
printf 'FEX payload %s FROM %s, %s\n' "$payload" "$buildroot" "$sdk"
