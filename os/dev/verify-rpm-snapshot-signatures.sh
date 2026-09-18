#!/usr/bin/env bash
# Authenticate all Fedora inputs; the reviewed VirtIO checksum is the sole exception.
set -euo pipefail
export LC_ALL=C
repo=${1:?usage: verify-rpm-snapshot-signatures.sh REPO ASSET_MANIFEST BASE_KEY}
manifest=${2:?asset manifest required}
key=${3:?key copied from original immutable base required}
# This trust anchor comes from the same immutable BASE_REF as the resolver.
test -s "$key"
sha256sum "$key"
trust=$(mktemp -d)
trap 'rm -rf -- "$trust"' EXIT
rpmkeys --dbpath "$trust" --import "$key"
# The isolated database cannot inherit keys installed by unrelated packages.
signed=0
for package in "$repo"/*.rpm; do
    test -f "$package" && test ! -L "$package"
    [[ "${package##*/}" =~ ^[A-Za-z0-9][A-Za-z0-9._+~^%-]*\.rpm$ ]] \
        || { echo "invalid RPM filename: $package" >&2; exit 1; }
    if [ "${package##*/}" = virtio-win-0.1.302-1.noarch.rpm ]; then
        checksum=$(awk -F '\t' '$2 == "virtio-win-0.1.302-1.noarch.rpm" { print $1; n++ } END { if (n != 1) exit 1 }' "$manifest")
        [[ "$checksum" =~ ^[0-9a-f]{64}$ ]]
        printf '%s  %s\n' "$checksum" "$package" | sha256sum --check --strict -
    else
        # rpmkeys also returns success for unsigned, digest-only RPMs. Require
        # a positive cryptographic signature result for EACH individual RPM.
        if ! result=$(rpmkeys --dbpath "$trust" --checksig --verbose "$package" 2>&1); then
            printf '%s\n' "$result" >&2
            exit 1
        fi
        printf '%s\n' "$result"
        if ! grep -Eq '^[[:space:]]+.*[Ss]ignature.*: OK$' <<< "$result"; then
            echo "RPM has no verified cryptographic signature: $package" >&2
            exit 1
        fi
        signed=$((signed + 1))
    fi
done
test "$signed" -gt 0
