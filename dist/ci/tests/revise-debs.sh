#!/usr/bin/env bash
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
version=$(<"$repo/dist/VERSION")

make_package() {
    local name=$1 architecture=$2 root
    root=$work/root-$name
    mkdir -p "$root/DEBIAN" "$root/usr/share/$name"
    cat > "$root/DEBIAN/control" <<EOF
Package: $name
Version: $version-1+ubuntu24.04
Architecture: $architecture
Maintainer: qOS CI <qos@example.invalid>
Description: fixture
EOF
    if [[ $name == qos-session-devel ]]; then
        printf 'Depends: qos-session (= %s-1+ubuntu24.04), coreutils, grep\n' "$version" \
            >> "$root/DEBIAN/control"
    fi
    printf 'payload-%s\n' "$name" > "$root/usr/share/$name/payload"
    dpkg-deb --build --root-owner-group "$root" \
        "$work/in/${name}_${version}-1+ubuntu24.04_${architecture}.deb" >/dev/null
}

mkdir -p "$work/in" "$work/out"
make_package qos-session amd64
make_package qos-session-devel all
"$repo/dist/debian/revise-debs.sh" "$work/in" "$work/out" 1+deb13 >/dev/null

main=$work/out/qos-session_${version}-1+deb13_amd64.deb
devel=$work/out/qos-session-devel_${version}-1+deb13_all.deb
[[ $(dpkg-deb -f "$main" Version) == "$version-1+deb13" ]]
[[ $(dpkg-deb -f "$devel" Depends) == "qos-session (= $version-1+deb13), coreutils, grep" ]]
for package in qos-session qos-session-devel; do
    rm -rf "$work/before" "$work/after"
    dpkg-deb --extract "$work/in/${package}_${version}-1+ubuntu24.04_"*.deb "$work/before"
    dpkg-deb --extract "$work/out/${package}_${version}-1+deb13_"*.deb "$work/after"
    diff -ru "$work/before" "$work/after"
done
echo 'Debian revision reuse OK'
