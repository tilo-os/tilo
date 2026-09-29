#!/usr/bin/env bash
# Install a built tilo-runtime package in this distribution userspace and prove
# it: dependencies, file ownership, installed layout, then the smoke test.
set -euo pipefail

[[ $# == 1 ]] || { echo "usage: $0 tilo-runtime.{deb,rpm,pkg.tar.zst}" >&2; exit 2; }
package=$(realpath "$1")
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
refuse_session_files=$repo_root/dist/runtime/refuse-session-files.sh

requires_each() {
    local listed=$1 dependency
    shift
    for dependency in "$@"; do
        grep -Eq "(^|[ ,])$dependency([ ,(<>=]|$)" <<<"$listed" \
            || { echo "tilo-runtime does not depend on $dependency" >&2; exit 1; }
    done
}

# The software Vulkan driver lets the headless app render in this container;
# desktops carry their own GPU driver. strace only explains a failed start.
case $package in
*.deb)
    apt-get install -y --no-install-recommends "$package" mesa-vulkan-drivers apparmor passwd strace
    dpkg-deb --fsys-tarfile "$package" | tar -t | "$refuse_session_files"
    requires_each "$(dpkg-query -W -f='${Depends}' tilo-runtime)" \
        bubblewrap xdg-desktop-portal xdg-dbus-proxy
    apparmor_parser --skip-kernel-load --skip-cache /etc/apparmor.d/tilo-runtime-launcher
    ;;
*.rpm)
    dnf install -y "$package" mesa-vulkan-drivers shadow-utils util-linux strace
    rpm -qlp "$package" | "$refuse_session_files"
    requires_each "$(rpm -q --requires tilo-runtime | paste -sd' ')" \
        bubblewrap xdg-desktop-portal xdg-dbus-proxy
    ;;
*.pkg.tar.*)
    export TILO_RUNTIME_DIR=/usr/lib/tilo/runtime
    pacman -S --noconfirm --needed vulkan-swrast strace
    pacman -U --noconfirm "$package"
    pacman -Qlp "$package" | awk '{ print $2 }' | "$refuse_session_files"
    requires_each "$(pacman -Qi tilo-runtime | sed -n '/^Depends On/,/^Optional Deps/p' | paste -sd' ')" \
        bubblewrap xdg-desktop-portal xdg-dbus-proxy
    ;;
*) echo "unknown package format: $package" >&2; exit 2 ;;
esac

"$repo_root/dist/runtime/verify-payload.sh" /
"$repo_root/dist/runtime/smoke-test.sh"
