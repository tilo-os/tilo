#!/usr/bin/env bash
# Install the tilo-session RPMs into the *running* system via
# `rpm-ostree usroverlay`, without a reboot.
#
# `rpm-ostree install` creates a new deployment by layering the package into it.
# The machine boots into that deployment; this is the long-term path and needs a
# reboot. `usroverlay` instead mounts a writable overlay over
# /usr for as long as this boot lasts, so the files are there immediately and
# gone at the next reboot. Good for "does the session entry show up in the
# login screen", not a way to run Tilo day to day.
#
#   sudo dist/rpm/usroverlay-install.sh             # runtime package only
#   sudo dist/rpm/usroverlay-install.sh --arm-devel # runtime + developer entry
#   sudo dist/rpm/usroverlay-install.sh --disarm-devel
#
# Everything this writes lives under /usr (transient) except the PAM stack in
# /etc, which is writable on an ostree host and so
# persists; it is inert without the compositor, and `--uninstall` removes it.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
rpm_arch=$(rpm --eval '%{_arch}')
rpms_dir=$repo_root/dist/rpm/build/RPMS/$rpm_arch

runtime_files=(
    /usr/bin/tilo-shell
    /usr/libexec/tilo/tilo-launcher
    /usr/libexec/tilo-session
    /usr/libexec/tilo-unlock-helper
    /usr/lib/systemd/user/tilo-graphical-session.target
    /usr/share/wayland-sessions/tilo.desktop
    /usr/share/xdg-desktop-portal/tilo-portals.conf
    /usr/share/xdg-desktop-portal/portals/tilo.portal
    /etc/pam.d/tilo-unlock
)
devel_files=(
    /usr/libexec/tilo-session-dev
    /usr/share/wayland-sessions/tilo-app.desktop
)

files=("${runtime_files[@]}" "${devel_files[@]}")

if [[ ${1:-} == --disarm-devel ]]; then
    [[ $(id -u) -eq 0 ]] || { echo "run me with sudo" >&2; exit 1; }
    for f in "${devel_files[@]}"; do
        [[ -e $f ]] && rm -f -- "$f" && echo "removed $f"
    done
    echo "The developer session is disarmed; the packaged Tilo entry is unchanged."
    exit 0
fi

if [[ ${1:-} == --arm-devel ]]; then
    include_devel=1
    shift
else
    include_devel=0
fi

if [[ ${1:-} == --uninstall ]]; then
    [[ $(id -u) -eq 0 ]] || { echo "run me with sudo" >&2; exit 1; }
    for f in "${files[@]}"; do
        [[ -e $f ]] && rm -f -- "$f" && echo "removed $f"
    done
    echo "note: the /usr overlay itself goes away at the next reboot."
    exit 0
fi

[[ $(id -u) -eq 0 ]] || { echo "run me with sudo" >&2; exit 1; }

# A normal host trial must not silently install the optional developer session.
# The devel package is included only in the explicit --arm-devel mode; the runtime
# pattern also prevents an older devel package from being unpacked by accident.
if (( include_devel )); then
    mapfile -t rpms < <(find "$rpms_dir" -maxdepth 1 -type f \
        \( -name 'tilo-session-[0-9]*.rpm' -o -name 'tilo-session-devel-[0-9]*.rpm' \) \
        -print | sort)
else
    mapfile -t rpms < <(find "$rpms_dir" -maxdepth 1 -type f \
        -name 'tilo-session-[0-9]*.rpm' -print | sort)
fi
(( ${#rpms[@]} )) || { echo "no runtime RPMs in $rpms_dir — run dist/rpm/build-rpm.sh first" >&2; exit 1; }
if (( ! include_devel )); then
    files=("${runtime_files[@]}")
fi

# Refuse to clobber anything that is not ours. On a host that runs some other
# desktop the rest of the time, quietly replacing a file under /usr is exactly
# the thing that must not happen by accident.
for f in "${files[@]}"; do
    if [[ -e $f && ! -L $f ]]; then
        # A file from a previous run of this script is fine to replace; a file
        # owned by another package is not.
        if rpm -qf "$f" >/dev/null 2>&1; then
            echo "refusing: $f belongs to $(rpm -qf "$f")" >&2
            exit 1
        fi
    fi
done

if ! findmnt -no FSTYPE /usr | grep -q overlay; then
    echo "== rpm-ostree usroverlay"
    rpm-ostree usroverlay
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
for rpm_file in "${rpms[@]}"; do
    echo "== unpacking $(basename "$rpm_file")"
    ( cd "$tmp" && rpm2cpio "$rpm_file" | cpio -idm --quiet )
done

for f in "${files[@]}"; do
    src=$tmp${f}
    [[ -f $src ]] || { echo "missing from payload: $f" >&2; exit 1; }
    install -Dpm "$(stat -c '%a' "$src")" "$src" "$f"
    echo "installed $f"
done

echo
echo "The login manager reads /usr/share/wayland-sessions at greeter start,"
echo "so log out (not just lock) for the new entries to appear."
