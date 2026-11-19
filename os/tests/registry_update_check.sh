#!/usr/bin/env bash
# A registry update of the server image on a booted server, as root: the image's
# own update step (tilo-verified-update) fetches the next image from the
# repository the server follows, the container policy verifies its signature,
# and it stages; an image of the same repository that nobody signed is refused.
#
#   registry_update_check.sh MIRROR KEY UNSIGNED STAGED_DIGEST
#
# MIRROR (host:port/repository) serves registry.tilo.so/tilo/tilo-host, as a site
# mirror would; the policy keeps verifying by that repository's name. KEY is the
# public key the test signed with, which stands in for the release key the
# policy names: the scope keeps its type and identity rule, and drops only the
# transparency-log key, as nothing the test signs is logged. UNSIGNED is a tag of
# the repository with no signature, STAGED_DIGEST the manifest digest of the
# signed image the server's own tag names.
# Test tooling, not part of the image: tools/ci/host-assurance.sh hands it over
# on the disk the update comes on.
set -euo pipefail
mirror=${1:?usage: registry_update_check.sh MIRROR KEY UNSIGNED STAGED_DIGEST}
key=${2:?}
unsigned=${3:?}
staged=${4:?}
repository=registry.tilo.so/tilo/tilo-host
fail() { printf 'FAIL  registry update: %s\n' "$*" >&2; exit 1; }

cat > /etc/containers/registries.conf.d/90-registry-update-check.conf <<CONF
[[registry]]
prefix = "$repository"
location = "$repository"
[[registry.mirror]]
location = "$mirror"
insecure = true
CONF
printf 'docker:\n  %s:\n    use-sigstore-attachments: true\n' "${mirror%%/*}" \
  > /etc/containers/registries.d/90-registry-update-check.yaml
install -m 0644 "$key" /etc/containers/registry-update-check.pub
python3 - <<'PY'
import json
path = '/etc/containers/policy.json'
policy = json.load(open(path))
scope = policy['transports']['docker']['registry.tilo.so/tilo/tilo-host']
assert len(scope) == 1 and scope[0]['type'] == 'sigstoreSigned', scope
assert scope[0]['signedIdentity'] == {'type': 'matchRepository'}, scope
scope[0]['keyPath'] = '/etc/containers/registry-update-check.pub'
scope[0].pop('rekorPublicKeyPath', None)
json.dump(policy, open(path, 'w'), indent=2)
PY

# Refused: the policy finds no signature on it.
if bootc switch --download-only "$repository:$unsigned" > /run/registry-update-unsigned.txt 2>&1; then
  fail "bootc fetched $repository:$unsigned, which nobody signed"
fi
grep -qi 'signature' /run/registry-update-unsigned.txt \
  || { cat /run/registry-update-unsigned.txt >&2; fail "fetching $unsigned failed, but not on its signature"; }
printf 'ok    an image of the repository nobody signed is refused\n'

# Staged: the server's own update step, as its timer starts it.
/usr/libexec/tilo-verified-update || fail 'tilo-verified-update failed'
bootc status --format json > /run/registry-update-status.json
python3 - "$staged" <<'PY' || fail "nothing staged, or not the signed image $staged"
import json, sys
status = json.load(open('/run/registry-update-status.json'))['status']
staged = (status.get('staged') or {}).get('image') or {}
print('staged', staged.get('image', {}).get('image'), staged.get('imageDigest'))
sys.exit(staged.get('imageDigest') != sys.argv[1])
PY
printf 'ok    the signed image of the repository it follows is verified and staged\n'
