#!/usr/bin/env bash
# Sign and immediately verify the RPMs at the protected release boundary.
# The private key is supplied only through the tag-job environment; it is never
# written into the checkout or release artifacts.
set -euo pipefail
export LC_ALL=C

[[ $# == 1 ]] || {
    printf 'usage: %s RELEASE_DIR\n' "$0" >&2
    exit 2
}
release_dir=$(realpath -- "$1")
[[ -d $release_dir ]] || { printf 'missing RPM release directory: %s\n' "$release_dir" >&2; exit 1; }

: "${QOS_RPM_SIGNING_KEY_B64:?missing QOS_RPM_SIGNING_KEY_B64}"
: "${QOS_RPM_SIGNING_KEY_ID:?missing QOS_RPM_SIGNING_KEY_ID}"
expected=${QOS_RPM_SIGNING_FINGERPRINT:?missing QOS_RPM_SIGNING_FINGERPRINT}
expected=${expected^^}
expected=${expected//[[:space:]]/}
[[ $expected =~ ^([A-F0-9]{40}|[A-F0-9]{64})$ ]] || {
    printf 'QOS_RPM_SIGNING_FINGERPRINT must be a full OpenPGP fingerprint\n' >&2
    exit 1
}

for tool in base64 gpg rpmkeys rpmsign sha256sum; do
    command -v "$tool" >/dev/null || {
        printf '%s is required to sign RPM releases\n' "$tool" >&2
        exit 1
    }
done

mapfile -t rpms < <(find "$release_dir" -maxdepth 1 -type f -name '*.rpm' -print | sort)
[[ ${#rpms[@]} -gt 0 ]] || { printf 'no RPMs found in %s\n' "$release_dir" >&2; exit 1; }

work=$(mktemp -d /var/tmp/qos-rpm-sign.XXXXXX)
trap 'find "$work" -depth -delete' EXIT
chmod 0700 "$work"
gpg_home=$work/gnupg
mkdir -m 0700 "$gpg_home"
private_key=$work/signing-key.asc
public_key=$work/signing-key-public.asc
rpmdb=$work/rpmdb
mkdir -p "$rpmdb"

# Decode through stdin so the armored private key is not present in a process
# argument, shell history, or a checked-out file.
(umask 077; printf '%s' "$QOS_RPM_SIGNING_KEY_B64" | base64 --decode > "$private_key")
GNUPGHOME="$gpg_home" gpg --batch --import "$private_key" >/dev/null 2>&1
GNUPGHOME="$gpg_home" gpg --batch --armor --export "$QOS_RPM_SIGNING_KEY_ID" > "$public_key"

mapfile -t fingerprints < <(
    GNUPGHOME="$gpg_home" gpg --batch --with-colons --fingerprint "$QOS_RPM_SIGNING_KEY_ID" 2>/dev/null \
        | awk -F: '$1 == "pub" { want = 1; next }
                   want && $1 == "fpr" { print toupper($10); want = 0 }'
)
[[ ${#fingerprints[@]} == 1 && ${fingerprints[0]} == "$expected" ]] || {
    printf 'RPM signing key does not match protected fingerprint\n' >&2
    exit 1
}
mapfile -t signing_fingerprints < <(
    GNUPGHOME="$gpg_home" gpg --batch --with-colons --fingerprint "$QOS_RPM_SIGNING_KEY_ID" 2>/dev/null \
        | awk -F: '$1 == "fpr" { print tolower($10) }'
)
[[ ${#signing_fingerprints[@]} -ge 1 ]] || {
    printf 'RPM signing key has no signing fingerprint\n' >&2
    exit 1
}
GNUPGHOME="$gpg_home" gpg --batch --with-colons --list-secret-keys "$expected" 2>/dev/null \
    | grep -q '^sec:' || { printf 'RPM signing key has no private key\n' >&2; exit 1; }

# rpmkeys verifies package signatures against an RPM keyring, independently of
# the GnuPG keyring used by rpmsign.
rpmkeys --dbpath "$rpmdb" --import "$public_key" >/dev/null
key_id_matches() {
    local key_id=${1,,} signing_fingerprint
    [[ -n $key_id ]] || return 1
    for signing_fingerprint in "${signing_fingerprints[@]}"; do
        [[ $signing_fingerprint == *"$key_id" ]] && return 0
    done
    return 1
}
for rpm_file in "${rpms[@]}"; do
    GNUPGHOME="$gpg_home" rpmsign \
        --define "_gpg_path $gpg_home" \
        --define "_gpg_name $QOS_RPM_SIGNING_KEY_ID" \
        --addsign "$rpm_file" >/dev/null
    check_output=$(rpmkeys --dbpath "$rpmdb" --checksig --verbose "$rpm_file" 2>&1)
    grep -Eq 'Signature, key ID [0-9A-Fa-f]+: OK' <<<"$check_output" || {
        printf 'RPM signature verification failed: %s\n%s\n' \
            "$(basename "$rpm_file")" "$check_output" >&2
        exit 1
    }
    key_id=$(sed -nE 's/.*Signature, key ID ([0-9A-Fa-f]+): OK/\1/p' <<<"$check_output" | head -n1)
    key_id_matches "$key_id" || {
        printf 'RPM has the wrong signing key: %s\n' "$(basename "$rpm_file")" >&2
        exit 1
    }
done

# Only the public key and its independently checked fingerprint leave the
# signing boundary. The optional flag prevents duplicate key artifacts when a
# future ARM lane signs its own RPMs.
if [[ ${QOS_RPM_SIGNING_KEY_ARTIFACT:-0} == 1 ]]; then
    install -m 0644 "$public_key" "$release_dir/qos-rpm-signing-key.asc"
    printf '%s\n' "$expected" > "$release_dir/qos-rpm-signing-key.fingerprint"
    (cd "$release_dir" && sha256sum qos-rpm-signing-key.asc > qos-rpm-signing-key.asc.sha256)
    (cd "$release_dir" && sha256sum qos-rpm-signing-key.fingerprint > qos-rpm-signing-key.fingerprint.sha256)
fi
printf 'verified %s signed RPMs with release key %s\n' "${#rpms[@]}" "$expected"
