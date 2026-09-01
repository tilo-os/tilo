#!/usr/bin/env bash
# Assert that a release contains every package variant and no silent omissions.
set -euo pipefail

[[ $# == 3 ]] || { echo "usage: $0 RELEASE_DIR VERSION ARCHES" >&2; exit 2; }
release_dir=${1%/}
version=$2
arches=$3
[[ -d $release_dir ]] || { echo "missing release directory: $release_dir" >&2; exit 1; }

case $arches in
    amd64) expected_count=9 ;;
    amd64,arm64) expected_count=16 ;;
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
    expect_one "qos-session_${version}-${distro_revision}_amd64.deb"
    expect_one "qos-session-devel_${version}-${distro_revision}_all.deb"
    if [[ $arches == amd64,arm64 ]]; then
        expect_one "qos-session_${version}-${distro_revision}_arm64.deb"
    fi
done
for name in qos-session qos-session-devel; do
    expect_one "${name}-${version}-1.fc44.x86_64.rpm"
    expect_one "${name}-${version}-1-x86_64.pkg.tar.zst"
    if [[ $arches == amd64,arm64 ]]; then
        expect_one "${name}-${version}-1.fc44.aarch64.rpm"
        expect_one "${name}-${version}-1-aarch64.pkg.tar.zst"
    fi
done
expect_one "qos-session-amd64.nar.gz"
if [[ $arches == amd64,arm64 ]]; then
    expect_one "qos-session-arm64.nar.gz"
fi

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
