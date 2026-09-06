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
    'qOS RPM verifier test' rsa2048 sign 0 >/dev/null 2>&1
fingerprint=$(GNUPGHOME="$gpg_home" gpg --batch --with-colons --list-keys \
    | awk -F: '$1 == "fpr" { print toupper($10); exit }')
[[ $fingerprint =~ ^[A-F0-9]{40}$ ]] || { echo "test key fingerprint missing" >&2; exit 1; }
key_id=${fingerprint: -8}

fixture=$work/release
mkdir "$fixture"
for name in \
    qos-session_0.1.5-1+ubuntu24.04_amd64.deb \
    qos-session-devel_0.1.5-1+ubuntu24.04_all.deb \
    qos-session_0.1.5-1+deb13_amd64.deb \
    qos-session-devel_0.1.5-1+deb13_all.deb \
    qos-session-0.1.5-1.fc44.x86_64.rpm \
    qos-session-devel-0.1.5-1.fc44.x86_64.rpm \
    qos-session-0.1.5-1-x86_64.pkg.tar.zst \
    qos-session-devel-0.1.5-1-x86_64.pkg.tar.zst \
    qos-session-amd64.nar.gz \
    qos-desktop-os-x86_64.tar.gz; do
    : > "$fixture/$name"
done
GNUPGHOME="$gpg_home" gpg --batch --armor --export "$fingerprint" \
    > "$fixture/qos-rpm-signing-key.asc"
printf '%s\n' "$fingerprint" > "$fixture/qos-rpm-signing-key.fingerprint"
"$repo_root/dist/rpm/make-repo-file.sh" 0.1.5 "$fixture/qos-fedora-44.repo"
(cd "$fixture" && sha256sum \
    qos-rpm-signing-key.asc > qos-rpm-signing-key.asc.sha256 && sha256sum \
    qos-rpm-signing-key.fingerprint > qos-rpm-signing-key.fingerprint.sha256 && sha256sum \
    qos-fedora-44.repo > qos-fedora-44.repo.sha256 && sha256sum \
    qos-desktop-os-x86_64.tar.gz > qos-desktop-os-x86_64.tar.gz.sha256)

fake_bin=$work/bin
mkdir "$fake_bin"
cat > "$fake_bin/rpmkeys" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case " $* " in
  *' --import '*) exit 0 ;;
  *' --checksig '*)
    case "${QOS_FAKE_RPM_MODE:-valid}" in
      valid) printf 'Header V4 RSA/SHA256 Signature, key ID %s: OK\n' "$QOS_FAKE_KEY_ID" ;;
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
    PATH="$fake_bin:$PATH" QOS_FAKE_KEY_ID="$key_id" \
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
    'qOS RPM verifier second test' rsa2048 sign 0 >/dev/null 2>&1
second_key=$work/second-key.asc
GNUPGHOME="$second_home" gpg --batch --armor --export > "$second_key"
substituted_key=$work/substituted-key
cp -a "$fixture" "$substituted_key"
cp "$second_key" "$substituted_key/qos-rpm-signing-key.asc"
(cd "$substituted_key" && sha256sum qos-rpm-signing-key.asc > qos-rpm-signing-key.asc.sha256)
expect_reject run_verify "$substituted_key"

# A substituted fingerprint must not make the original key appear trusted.
second_fingerprint=$(GNUPGHOME="$second_home" gpg --batch --with-colons --list-keys \
    | awk -F: '$1 == "fpr" { print toupper($10); exit }')
substituted_fingerprint=$work/substituted-fingerprint
cp -a "$fixture" "$substituted_fingerprint"
printf '%s\n' "$second_fingerprint" > "$substituted_fingerprint/qos-rpm-signing-key.fingerprint"
(cd "$substituted_fingerprint" && sha256sum qos-rpm-signing-key.fingerprint > qos-rpm-signing-key.fingerprint.sha256)
expect_reject run_verify "$substituted_fingerprint"

# An extra primary key must not be smuggled into an otherwise valid trust
# bundle; every accepted RPM key must descend from the one published primary.
extra_primary=$work/extra-primary
cp -a "$fixture" "$extra_primary"
GNUPGHOME="$second_home" gpg --batch --armor --export >> \
    "$extra_primary/qos-rpm-signing-key.asc"
(cd "$extra_primary" && sha256sum qos-rpm-signing-key.asc > qos-rpm-signing-key.asc.sha256)
expect_reject run_verify "$extra_primary"

# The repo asset is part of the trust bootstrap and must be the generated
# definition for this exact release, even if an attacker refreshes its digest.
altered_repo=$work/altered-repo
cp -a "$fixture" "$altered_repo"
printf '\nbaseurl=https://attacker.invalid/repo\n' >> "$altered_repo/qos-fedora-44.repo"
(cd "$altered_repo" && sha256sum qos-fedora-44.repo > qos-fedora-44.repo.sha256)
expect_reject run_verify "$altered_repo"

expect_reject env QOS_FAKE_RPM_MODE=unsigned PATH="$fake_bin:$PATH" \
    QOS_FAKE_KEY_ID="$key_id" "$verify" "$fixture" 0.1.5 amd64
expect_reject env QOS_FAKE_RPM_MODE=wrong PATH="$fake_bin:$PATH" \
    QOS_FAKE_KEY_ID="$key_id" "$verify" "$fixture" 0.1.5 amd64
printf 'RPM signing trust-contract negatives: PASS\n'
