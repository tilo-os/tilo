#!/usr/bin/env bash
# Sign only the board this protected manual lane just built. Never promote it
# into a production channel or claim independent release reproduction.
set -euo pipefail
[[ $# == 2 && $2 == orangepi-3b-v2-1 ]]
image=$1
[[ $image =~ ^registry\.tilo\.so/tilo/tilo-vm@sha256:[0-9a-f]{64}$ ]]
[[ ${GITHUB_REF:-} == refs/heads/main && ${GITHUB_EVENT_NAME:-} == workflow_dispatch \
   && ${TILO_SIGNED_DEVELOPMENT_BOARD:-0} == 1 ]]
[[ ${SOURCE_REVISION:-} =~ ^[0-9a-f]{40}$ && $SOURCE_REVISION == "${GITHUB_SHA:-}" ]]
repo=$(git rev-parse --show-toplevel)
work=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/signed-development-board.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
skopeo inspect --config "docker://$image" > "$work/config.json"
python3 - "$work" "$image" "$SOURCE_REVISION" "$repo" <<'PY'
import datetime, hashlib, json, os, pathlib, sys
work, image, source, repo = pathlib.Path(sys.argv[1]), sys.argv[2], sys.argv[3], pathlib.Path(sys.argv[4])
config = json.loads((work/'config.json').read_text())
labels = config['config']['Labels']
assert config['architecture'] == 'arm64' and config['os'] == 'linux'
assert labels['org.opencontainers.image.revision'] == source
assert labels['sh.tilo.image.flavour'] == 'dev'
profile = 'orangepi-3b-v2-1'
definition = hashlib.sha256((repo/f'os/hardware/profiles/{profile}.json').read_bytes()).hexdigest()
now = int(datetime.datetime.now(datetime.timezone.utc).timestamp())
provenance = {
 'buildDefinition': {'buildType': 'https://tilo.so/buildtypes/bootc-development-board/v1',
  'externalParameters': {'sourceRevision': source, 'architecture': 'aarch64', 'hardwareProfile': profile,
   'profileDefinitionSha256': definition, 'flavour': 'dev', 'independentReproduction': False},
  'resolvedDependencies': [{'uri': 'git+https://code.tilo.so/tilo/tilo', 'digest': {'gitCommit': source}}]},
 'runDetails': {'builder': {'id': 'https://code.tilo.so/tilo/tilo/.forgejo/workflows/build-image.yml@refs/heads/main'},
  'metadata': {'invocationId': os.environ.get('GITHUB_RUN_ID', '')}}}
freshness = {'repository': 'registry.tilo.so/tilo/tilo-vm', 'imageDigest': image.split('@')[1],
 'sourceRevision': source, 'hardwareProfile': profile, 'flavour': 'dev', 'issuedAt': now, 'expiresAt': now+86400}
for name, value in [('provenance', provenance), ('freshness', freshness)]:
 (work/f'{name}.json').write_text(json.dumps(value)+'\n')
PY
./os/dev/install-cached-syft.sh
"$RUNNER_TEMP/tilo-syft-tools/syft" "$image" -o spdx-json > "$work/sbom.spdx.json"
# Existing main workflow, role, audience and pinned signer. Stop on denials.
python3 - "$work" <<'PY'
import hashlib, json, pathlib, sys
w = pathlib.Path(sys.argv[1])
p = json.loads((w/'provenance.json').read_text())
p['buildDefinition']['externalParameters']['sbomSha256'] = hashlib.sha256((w/'sbom.spdx.json').read_bytes()).hexdigest()
(w/'provenance.json').write_text(json.dumps(p)+'\n')
PY
. os/dev/configure-cosign-kms.sh
trap 'rm -rf -- "$work"; rm -f -- "${AWS_WEB_IDENTITY_TOKEN_FILE:-}"' EXIT
cosign sign --yes --key "$COSIGN_KEY_REF" "$image"
cosign attest --yes --key "$COSIGN_KEY_REF" --type slsaprovenance1 --predicate "$work/provenance.json" "$image"
cosign attest --yes --key "$COSIGN_KEY_REF" --type https://tilo.so/attestations/development-board-freshness/v1 --predicate "$work/freshness.json" "$image"
cosign attach sbom --type spdx --input-format json --sbom "$work/sbom.spdx.json" "$image"
cosign verify --key keys/cosign.pub "$image" >/dev/null
cosign verify-attestation --key keys/cosign.pub --type slsaprovenance1 "$image" >/dev/null
cosign verify-attestation --key keys/cosign.pub --type https://tilo.so/attestations/development-board-freshness/v1 "$image" >/dev/null
printf '::notice::signed development board (not production): %s\n' "$image"
