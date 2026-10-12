#!/usr/bin/env bash
# Exercise the release verifier with a real temporary OpenPGP public key and
# synthetic RPM-check results. This covers trust-bundle substitution and the
# unsigned/wrong-key failures without requiring a live Forgejo registry.
set -euo pipefail
export LC_ALL=C

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
gpg_home=$work/gpg
mkdir -m 0700 "$gpg_home"
GNUPGHOME="$gpg_home" gpg --batch --passphrase '' --quick-gen-key \
    'Tilo RPM verifier test' rsa2048 sign 0 >/dev/null 2>&1
fingerprint=$(GNUPGHOME="$gpg_home" gpg --batch --with-colons --list-keys \
    | awk -F: '$1 == "fpr" { print toupper($10); exit }')
[[ $fingerprint =~ ^[A-F0-9]{40}$ ]] || { echo "test key fingerprint missing" >&2; exit 1; }
key_id=${fingerprint: -8}

fixture=$work/release
mkdir "$fixture"
for name in \
    tilo-session_0.1.5-1+ubuntu24.04_amd64.deb \
    tilo-session-devel_0.1.5-1+ubuntu24.04_all.deb \
    tilo-session_0.1.5-1+deb13_amd64.deb \
    tilo-session-devel_0.1.5-1+deb13_all.deb \
    tilo-session-0.1.5-1.fc44.x86_64.rpm \
    tilo-session-apps-0.1.5-1.fc44.x86_64.rpm \
    tilo-session-devel-0.1.5-1.fc44.x86_64.rpm \
    tilo-session-0.1.5-1-x86_64.pkg.tar.zst \
    tilo-session-devel-0.1.5-1-x86_64.pkg.tar.zst \
    tilo-session-amd64.nar.gz \
    tilo-desktop-os-x86_64.tar.gz; do
    : > "$fixture/$name"
done
GNUPGHOME="$gpg_home" gpg --batch --armor --export "$fingerprint" \
    > "$fixture/tilo-rpm-signing-key.asc"
printf '%s\n' "$fingerprint" > "$fixture/tilo-rpm-signing-key.fingerprint"
"$repo_root/dist/rpm/make-repo-file.sh" 0.1.5 "$fixture/tilo-fedora-44.repo"
# Retain the public broker provenance pair, using synthetic immutable RPM bytes.
python3 - "$repo_root" "$fixture" "$fingerprint" <<'PYFIXTURE'
import importlib.util
import json
import os
from pathlib import Path
import struct
import sys
root, directory, fingerprint = sys.argv[1:]
directory = Path(directory)
spec = importlib.util.spec_from_file_location('broker', Path(root) / 'dist/rpm/sign-rpms-broker.py')
broker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(broker)
for file in directory.glob('*.rpm'):
    file.write_bytes(bytes.fromhex('edabeedb') + bytes(92) + broker.MAGIC + bytes(4) + struct.pack('>II', 0, 0) + broker.MAGIC + b'fixture payload')
env = dict(os.environ, GITHUB_REPOSITORY='tilo/tilo', GITHUB_EVENT_NAME='push',
           GITHUB_REF='refs/tags/desktop-v0.1.5', GITHUB_SHA=os.environ.get('GITHUB_SHA', 'b' * 40),
           GITHUB_RUN_ID=os.environ.get('GITHUB_RUN_ID', '123'),
           GITHUB_RUN_ATTEMPT=os.environ.get('GITHUB_RUN_ATTEMPT', '1'),
           TILO_RPM_ARCH='amd64', TILO_RPM_SIGNING_FINGERPRINT=fingerprint,
           ACTIONS_ID_TOKEN_REQUEST_URL='test-only-not-fetched', ACTIONS_ID_TOKEN_REQUEST_TOKEN='test-only')
request = broker.request_for(directory, env)
request_path = directory / 'tilo-rpm-signing-request-amd64.json'
request_path.write_text(json.dumps(request) + '\n')
receipt = {'schema': 1, 'request_sha256': broker.digest(request_path), 'fingerprint': fingerprint,
           'signed_sha256': {p.name: broker.digest(p) for p in directory.glob('*.rpm')}}
(directory / 'tilo-rpm-signing-receipt-amd64.json').write_text(json.dumps(receipt) + '\n')
PYFIXTURE

(cd "$fixture" && sha256sum \
    tilo-rpm-signing-key.asc > tilo-rpm-signing-key.asc.sha256 && sha256sum \
    tilo-rpm-signing-key.fingerprint > tilo-rpm-signing-key.fingerprint.sha256 && sha256sum \
    tilo-fedora-44.repo > tilo-fedora-44.repo.sha256 && sha256sum \
    tilo-desktop-os-x86_64.tar.gz > tilo-desktop-os-x86_64.tar.gz.sha256)

fake_bin=$work/bin
mkdir "$fake_bin"
cat > "$fake_bin/rpmkeys" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case " $* " in
  *' --import '*) exit 0 ;;
  *' --checksig '*)
    case "${TILO_FAKE_RPM_MODE:-valid}" in
      valid) printf 'Header V4 RSA/SHA256 Signature, key ID %s: OK\n' "$TILO_FAKE_KEY_ID" ;;
      unsigned) printf 'Header SHA256 digest: OK\n' ;;
      wrong) printf 'Header V4 RSA/SHA256 Signature, key ID deadbeef: NOKEY\n' ;;
      *) exit 2 ;;
    esac
    ;;
  *) exit 2 ;;
esac
SH
chmod 0755 "$fake_bin/rpmkeys"
verify="$repo_root/dist/ci/verify-release-artifacts.sh"
run_verify() {
    PATH="$fake_bin:$PATH" TILO_FAKE_KEY_ID="$key_id" TILO_RPM_SIGNING_FINGERPRINT="$fingerprint" \
        "$verify" "$1" 0.1.5 amd64
}
run_verify "$fixture" >/dev/null

expect_reject() {
    if "$@" >/dev/null 2>&1; then
        echo "release verifier accepted an invalid trust case" >&2
        exit 1
    fi
}

# A substituted public key must not be accepted with the old fingerprint.
second_home=$work/gpg-second
mkdir -m 0700 "$second_home"
GNUPGHOME="$second_home" gpg --batch --passphrase '' --quick-gen-key \
    'Tilo RPM verifier second test' rsa2048 sign 0 >/dev/null 2>&1
second_key=$work/second-key.asc
GNUPGHOME="$second_home" gpg --batch --armor --export > "$second_key"
substituted_key=$work/substituted-key
cp -a "$fixture" "$substituted_key"
cp "$second_key" "$substituted_key/tilo-rpm-signing-key.asc"
(cd "$substituted_key" && sha256sum tilo-rpm-signing-key.asc > tilo-rpm-signing-key.asc.sha256)
expect_reject run_verify "$substituted_key"

# A substituted fingerprint must not make the original key appear trusted.
second_fingerprint=$(GNUPGHOME="$second_home" gpg --batch --with-colons --list-keys \
    | awk -F: '$1 == "fpr" { print toupper($10); exit }')
substituted_fingerprint=$work/substituted-fingerprint
cp -a "$fixture" "$substituted_fingerprint"
printf '%s\n' "$second_fingerprint" > "$substituted_fingerprint/tilo-rpm-signing-key.fingerprint"
(cd "$substituted_fingerprint" && sha256sum tilo-rpm-signing-key.fingerprint > tilo-rpm-signing-key.fingerprint.sha256)
expect_reject run_verify "$substituted_fingerprint"

# An extra primary key must not be smuggled into an otherwise valid trust
# bundle; every accepted RPM key must descend from the one published primary.
extra_primary=$work/extra-primary
cp -a "$fixture" "$extra_primary"
GNUPGHOME="$second_home" gpg --batch --armor --export >> \
    "$extra_primary/tilo-rpm-signing-key.asc"
(cd "$extra_primary" && sha256sum tilo-rpm-signing-key.asc > tilo-rpm-signing-key.asc.sha256)
expect_reject run_verify "$extra_primary"

# The repo asset is part of the trust bootstrap and must be the generated
# definition for this exact release, even if an attacker refreshes its digest.
altered_repo=$work/altered-repo
cp -a "$fixture" "$altered_repo"
printf '\nbaseurl=https://attacker.invalid/repo\n' >> "$altered_repo/tilo-fedora-44.repo"
(cd "$altered_repo" && sha256sum tilo-fedora-44.repo > tilo-fedora-44.repo.sha256)
expect_reject run_verify "$altered_repo"

expect_reject env -u TILO_RPM_SIGNING_FINGERPRINT PATH="$fake_bin:$PATH" \
    TILO_FAKE_KEY_ID="$key_id" "$verify" "$fixture" 0.1.5 amd64
expect_reject env TILO_FAKE_RPM_MODE=unsigned PATH="$fake_bin:$PATH" \
    TILO_RPM_SIGNING_FINGERPRINT="$fingerprint" \
    TILO_FAKE_KEY_ID="$key_id" "$verify" "$fixture" 0.1.5 amd64
expect_reject env TILO_FAKE_RPM_MODE=wrong PATH="$fake_bin:$PATH" \
    TILO_RPM_SIGNING_FINGERPRINT="$fingerprint" \
    TILO_FAKE_KEY_ID="$key_id" "$verify" "$fixture" 0.1.5 amd64
printf 'RPM signing trust-contract negatives: PASS\n'
