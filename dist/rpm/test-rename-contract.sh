#!/usr/bin/env bash
# Validate the Tilo -> Tilo RPM rename in metadata and, when old/new artifacts
# are supplied, in an isolated RPM database.  The latter catches metadata that
# looks plausible but does not actually let the solver remove the old package.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
spec=$repo_root/dist/rpm/tilo-session.spec

for suffix in '' -apps -devel; do
    old=tilo-session$suffix
    new=tilo-session$suffix
    grep -Eq "^Provides:[[:space:]]+$old = %\{version\}-%\{release\}$" "$spec"
    grep -Eq "^Obsoletes:[[:space:]]+$old < 0\.1\.9$" "$spec"
    ! grep -Eq "^Conflicts:[[:space:]]+($old|$new)([[:space:]]|$)" "$spec"
done

(( $# == 0 )) && exit 0
(( $# == 2 )) || {
    echo "usage: $0 [old-rpm-directory new-rpm-directory]" >&2
    exit 2
}

old_dir=$(realpath "$1")
new_dir=$(realpath "$2")
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT

# rpm --test validates the transaction graph without running scriptlets or
# touching the host.  Ignore unrelated host-library dependencies: this test is
# specifically about replacement semantics and runs in an empty database.
rpm --root "$root" --initdb
mapfile -t old_rpms < <(find "$old_dir" -maxdepth 1 -type f \
    \( -name 'tilo-session-[0-9]*.rpm' -o -name 'tilo-session-apps-[0-9]*.rpm' \
       -o -name 'tilo-session-devel-[0-9]*.rpm' \) -print | sort)
mapfile -t new_rpms < <(find "$new_dir" -maxdepth 1 -type f \
    \( -name 'tilo-session-[0-9]*.rpm' -o -name 'tilo-session-apps-[0-9]*.rpm' \
       -o -name 'tilo-session-devel-[0-9]*.rpm' \) -print | sort)
(( ${#old_rpms[@]} == 3 )) || { echo "expected three old Tilo RPMs" >&2; exit 1; }
(( ${#new_rpms[@]} == 3 )) || { echo "expected three new Tilo RPMs" >&2; exit 1; }

rpm --root "$root" -Uvh --nodeps --noscripts "${old_rpms[@]}" >/dev/null
rpm --root "$root" -Uvh --nodeps --noscripts --test "${new_rpms[@]}" >/dev/null
rpm --root "$root" -Uvh --nodeps --noscripts "${new_rpms[@]}" >/dev/null

for suffix in '' -apps -devel; do
    rpm --root "$root" -q "tilo-session$suffix" >/dev/null
    ! rpm --root "$root" -q "tilo-session$suffix" >/dev/null 2>&1
done

# The old session descriptors and helpers were owned exclusively by the old
# package.  Successful erasure proves they do not survive as duplicate GDM and
# portal entries after the renamed package is installed.
for path in \
    /usr/share/wayland-sessions/tilo.desktop \
    /usr/share/wayland-sessions/tilo-dev.desktop \
    /usr/share/xdg-desktop-portal/portals/tilo.portal \
    /usr/libexec/tilo-session \
    /usr/bin/tilo-shell; do
    test ! -e "$root$path" || { echo "stale Tilo path after rename: $path" >&2; exit 1; }
done
