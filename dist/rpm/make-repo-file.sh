#!/usr/bin/env bash
# Produce the Tilo RPM repo definition shipped with each signed desktop release.
# Forgejo's generated .repo is intentionally not used: it cannot advertise the
# Tilo signing key or enable package verification.
set -euo pipefail

[[ $# == 2 ]] || {
    printf 'usage: %s VERSION OUTPUT\n' "$0" >&2
    exit 2
}
version=$1
output=$(realpath -m -- "$2")
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
    printf 'invalid desktop release version: %s\n' "$version" >&2
    exit 1
}
mkdir -p "$(dirname "$output")"
cat > "$output" <<EOF
[tilo-fedora-44]
name=Tilo Desktop session (Fedora 44)
# Bootstrap /etc/pki/rpm-gpg/tilo-rpm-signing-key.asc from desktop-v${version}.
baseurl=https://code.sandbox.example/api/packages/qos/rpm/fedora/44
enabled=1
gpgcheck=1
repo_gpgcheck=0
gpgkey=file:///etc/pki/rpm-gpg/tilo-rpm-signing-key.asc
EOF
chmod 0644 "$output"
