#!/usr/bin/env bash
# Reissue an already-built Debian-family payload for another distribution
# revision without compiling its native binaries again. The caller must still
# install-test the resulting packages in the target userspace.
set -euo pipefail

[[ $# == 3 ]] || {
    echo "usage: $0 INPUT_DIR OUTPUT_DIR REVISION" >&2
    exit 2
}
input_dir=$1
output_dir=$2
revision=$3
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
version=$(<"$repo_root/dist/VERSION")
new_version=$version-$revision

[[ $revision =~ ^[0-9][A-Za-z0-9.+~]*$ ]] || {
    echo "invalid Debian revision: $revision" >&2
    exit 2
}
mkdir -p "$output_dir"
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT

shopt -s nullglob
inputs=("$input_dir"/qos-session_*.deb "$input_dir"/qos-session-devel_*.deb)
(( ${#inputs[@]} >= 1 )) || {
    echo "no qOS Debian packages in $input_dir" >&2
    exit 1
}

main=0
for package in "${inputs[@]}"; do
    package_name=$(dpkg-deb -f "$package" Package)
    old_version=$(dpkg-deb -f "$package" Version)
    architecture=$(dpkg-deb -f "$package" Architecture)
    [[ $old_version == "$version-"* ]] || {
        echo "unexpected source version $old_version in $package" >&2
        exit 1
    }
    case "$package_name:$architecture" in
        qos-session:amd64|qos-session:arm64) main=$((main + 1)) ;;
        qos-session-devel:all) ;;
        *) echo "unexpected package identity: $package_name/$architecture" >&2; exit 1 ;;
    esac

    root=$work/$package_name
    rm -rf -- "$root"
    dpkg-deb --raw-extract "$package" "$root"
    sed -i "s/^Version: .*/Version: $new_version/" "$root/DEBIAN/control"
    if [[ $package_name == qos-session-devel ]]; then
        sed -i "s/qos-session (= $old_version)/qos-session (= $new_version)/" \
            "$root/DEBIAN/control"
        grep -Fqx "Depends: qos-session (= $new_version), coreutils, grep" \
            "$root/DEBIAN/control"
    fi
    output=$output_dir/${package_name}_${new_version}_${architecture}.deb
    dpkg-deb --build --root-owner-group "$root" "$output"
    [[ $(dpkg-deb -f "$output" Version) == "$new_version" ]]
done

(( main == 1 )) || {
    echo "expected exactly one architecture package, found $main" >&2
    exit 1
}
