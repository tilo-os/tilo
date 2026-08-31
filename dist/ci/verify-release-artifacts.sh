#!/usr/bin/env bash
# Assert that a release contains every package variant and no silent omissions.
set -euo pipefail

[[ $# == 2 ]] || { echo "usage: $0 RELEASE_DIR VERSION" >&2; exit 2; }
release_dir=${1%/}
version=$2
[[ -d $release_dir ]] || { echo "missing release directory: $release_dir" >&2; exit 1; }

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
    expect_one "qos-session_${version}-${distro_revision}_amd64.deb"
    expect_one "qos-session_${version}-${distro_revision}_arm64.deb"
    expect_one "qos-session-devel_${version}-${distro_revision}_all.deb"
done
for rpm_arch in x86_64 aarch64; do
    expect_one "qos-session-${version}-1.fc42.${rpm_arch}.rpm"
    expect_one "qos-session-devel-${version}-1.fc42.${rpm_arch}.rpm"
done
for arch_arch in x86_64 aarch64; do
    expect_one "qos-session-${version}-1-${arch_arch}.pkg.tar.zst"
    expect_one "qos-session-devel-${version}-1-${arch_arch}.pkg.tar.zst"
done
expect_one "qos-session-amd64.nar.gz"
expect_one "qos-session-arm64.nar.gz"

if find "$release_dir" -mindepth 1 -maxdepth 1 ! -type f -print -quit | grep -q .; then
    echo "release artifacts must be flattened into one directory" >&2
    exit 1
fi
artifact_count=$(find "$release_dir" -mindepth 1 -maxdepth 1 -type f | wc -l)
[[ $artifact_count == 16 ]] || {
    echo "expected 16 release artifacts, found $artifact_count" >&2
    exit 1
}
printf 'verified all %s release artifacts for %s\n' "$artifact_count" "$version"
