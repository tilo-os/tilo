#!/usr/bin/env bash
# Assert that a release contains every package variant and no silent omissions.
set -euo pipefail
export LC_ALL=C

[[ $# == 3 ]] || { echo "usage: $0 RELEASE_DIR VERSION ARCHES" >&2; exit 2; }
release_dir=${1%/}
version=$2
arches=$3
[[ -d $release_dir ]] || { echo "missing release directory: $release_dir" >&2; exit 1; }
for tool in gpg rpmkeys; do
    command -v "$tool" >/dev/null || {
        printf '%s is required to verify signed RPM release artifacts\n' "$tool" >&2
        exit 1
    }
done

case $arches in
    # Ten package artifacts plus the native OS payload pair, four public
    # RPM-key trust artifacts, the maintained verified repo definition, and
    # a public broker request/receipt pair per architecture.
    # ARM adds its package/payload set, but only the x86 lane publishes the
    # duplicate-free key and repo trust bundle.
    amd64) expected_count=20 ;;
    amd64,arm64) expected_count=30 ;;
    *) echo "unsupported release architecture set: $arches" >&2; exit 2 ;;
esac

expect_one() {
    local pattern=$1
    local matches=()
    shopt -s nullglob
    matches=("$release_dir"/$pattern)
    shopt -u nullglob
    if (( ${#matches[@]} != 1 )); then
        echo "expected exactly one release artifact matching $pattern, found ${#matches[@]}" >&2
        return 1
    fi
}

# Ubuntu 24.04 and Debian 13 are separate ABI/package-manager build roots.
for distro_revision in 1+ubuntu24.04 1+deb13; do
    expect_one "tilo-session_${version}-${distro_revision}_amd64.deb"
    expect_one "tilo-session-devel_${version}-${distro_revision}_all.deb"
    if [[ $arches == amd64,arm64 ]]; then
        expect_one "tilo-session_${version}-${distro_revision}_arm64.deb"
    fi
done
for name in tilo-session tilo-session-devel; do
    expect_one "${name}-${version}-1.fc44.x86_64.rpm"
    expect_one "${name}-${version}-1-x86_64.pkg.tar.zst"
    if [[ $arches == amd64,arm64 ]]; then
        expect_one "${name}-${version}-1.fc44.aarch64.rpm"
        expect_one "${name}-${version}-1-aarch64.pkg.tar.zst"
    fi
done
expect_one "tilo-session-amd64.nar.gz"
expect_one "tilo-session-apps-${version}-1.fc44.x86_64.rpm"
if [[ $arches == amd64,arm64 ]]; then
    expect_one "tilo-session-apps-${version}-1.fc44.aarch64.rpm"
fi
if [[ $arches == amd64,arm64 ]]; then
    expect_one "tilo-session-arm64.nar.gz"
fi
for payload_arch in x86_64; do
    expect_one "tilo-desktop-os-${payload_arch}.tar.gz"
    expect_one "tilo-desktop-os-${payload_arch}.tar.gz.sha256"
done
if [[ $arches == amd64,arm64 ]]; then
    expect_one "tilo-desktop-os-aarch64.tar.gz"
    expect_one "tilo-desktop-os-aarch64.tar.gz.sha256"
fi
for key_artifact in tilo-rpm-signing-key.asc tilo-rpm-signing-key.fingerprint \
                    tilo-rpm-signing-key.asc.sha256 tilo-rpm-signing-key.fingerprint.sha256; do
    expect_one "$key_artifact"
done
(cd "$release_dir" && sha256sum --check --strict \
    tilo-rpm-signing-key.asc.sha256 tilo-rpm-signing-key.fingerprint.sha256)
expect_one tilo-fedora-44.repo
expect_one tilo-fedora-44.repo.sha256
(cd "$release_dir" && sha256sum --check --strict tilo-fedora-44.repo.sha256)
fingerprint=$(tr -d '[:space:]' <"$release_dir/tilo-rpm-signing-key.fingerprint")
[[ $fingerprint =~ ^[A-Fa-f0-9]{40}$|^[A-Fa-f0-9]{64}$ ]] || {
    echo "RPM release key fingerprint is malformed" >&2
    exit 1
}
fingerprint=${fingerprint^^}
rpm_keyring=$(mktemp -d /var/tmp/tilo-rpm-release-keyring.XXXXXX)
gpg_home=$(mktemp -d /var/tmp/tilo-rpm-release-gpg.XXXXXX)
expected_repo=$(mktemp /var/tmp/tilo-fedora-44.expected.XXXXXX)
repo_generator=$(dirname "${BASH_SOURCE[0]}")/../rpm/make-repo-file.sh
trap 'find "$rpm_keyring" "$gpg_home" -depth -delete; rm -f -- "$expected_repo"' EXIT
chmod 0700 "$rpm_keyring" "$gpg_home"
"$repo_generator" "$version" "$expected_repo"
cmp -- "$expected_repo" "$release_dir/tilo-fedora-44.repo" || {
    echo "RPM release repo definition is not the maintained versioned definition" >&2
    exit 1
}
GNUPGHOME="$gpg_home" gpg --batch --import "$release_dir/tilo-rpm-signing-key.asc" >/dev/null 2>&1
mapfile -t key_fingerprints < <(
    GNUPGHOME="$gpg_home" gpg --batch --with-colons --fingerprint 2>/dev/null \
        | awk -F: '$1 == "fpr" { print toupper($10) }'
)
mapfile -t primary_fingerprints < <(
    GNUPGHOME="$gpg_home" gpg --batch --with-colons --fingerprint 2>/dev/null \
        | awk -F: '$1 == "pub" { want = 1; next }
                   want && $1 == "fpr" { print toupper($10); want = 0 }'
)
[[ ${#primary_fingerprints[@]} == 1 && ${primary_fingerprints[0]} == "$fingerprint" ]] || {
    echo "RPM release key does not match its published fingerprint" >&2
    exit 1
}
if GNUPGHOME="$gpg_home" gpg --batch --with-colons --list-secret-keys 2>/dev/null \
    | grep -q '^sec:'; then
    echo "RPM release key artifact contains private key material" >&2
    exit 1
fi
rpmkeys --dbpath "$rpm_keyring" --import "$release_dir/tilo-rpm-signing-key.asc" >/dev/null
mapfile -t release_rpms < <(find "$release_dir" -maxdepth 1 -type f -name '*.rpm' -print | sort)
[[ ${#release_rpms[@]} -gt 0 ]] || { echo "release has no RPMs" >&2; exit 1; }
for rpm_file in "${release_rpms[@]}"; do
    check_output=$(rpmkeys --dbpath "$rpm_keyring" --checksig --verbose "$rpm_file" 2>&1)
    mapfile -t signature_lines < <(grep -Ei 'Signature, key ID [0-9A-Fa-f]+: ' <<<"$check_output" || true)
    [[ ${#signature_lines[@]} -gt 0 ]] || {
        printf 'RPM is unsigned: %s\n%s\n' "$(basename "$rpm_file")" "$check_output" >&2
        exit 1
    }
    for signature_line in "${signature_lines[@]}"; do
        [[ $signature_line =~ Signature,\ key\ ID\ ([0-9A-Fa-f]+):\ OK$ ]] || {
            printf 'RPM signature failed: %s\n%s\n' "$(basename "$rpm_file")" "$check_output" >&2
            exit 1
        }
        key_id=${BASH_REMATCH[1],,}
        key_id_matches=1
        for key_fingerprint in "${key_fingerprints[@]}"; do
            [[ ${key_fingerprint,,} == *"$key_id" ]] && key_id_matches=0 && break
        done
        (( key_id_matches == 0 )) || {
            printf 'RPM is signed by an unexpected key: %s\n' "$(basename "$rpm_file")" >&2
            exit 1
        }
    done
done

# Preserve the broker's same-run/source binding as public release provenance.
for arch in ${arches//,/ }; do
    expect_one "tilo-rpm-signing-request-$arch.json"
    expect_one "tilo-rpm-signing-receipt-$arch.json"
    python3 "$(dirname -- "$0")/../rpm/sign-rpms-broker.py" \
        --verify-published "$release_dir" "$arch" "$version"
done

if find "$release_dir" -mindepth 1 -maxdepth 1 ! -type f -print -quit | grep -q .; then
    echo "release artifacts must be flattened into one directory" >&2
    exit 1
fi
artifact_count=$(find "$release_dir" -mindepth 1 -maxdepth 1 -type f | wc -l)
[[ $artifact_count == "$expected_count" ]] || {
    echo "expected $expected_count release artifacts, found $artifact_count" >&2
    exit 1
}
printf 'verified all %s release artifacts for %s\n' "$artifact_count" "$version"
