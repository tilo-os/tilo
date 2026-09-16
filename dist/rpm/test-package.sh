#!/usr/bin/env bash
set -euo pipefail
[[ $# -ge 1 ]] || { echo "usage: $0 qos-session.rpm [qos-session-devel.rpm]" >&2; exit 2; }
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
expect_devel=0
for package in "$@"; do
    [[ $(basename "$package") != qos-session-devel-* ]] || expect_devel=1
done
# Package lanes deliberately install build dependencies without weak deps, so
# install the recommended software Vulkan ICD explicitly before exercising the
# display-free compositor. Real Fedora desktops normally already carry a GPU
# ICD; the isolated package userspace must still prove the fallback path.
dnf install -y "$@" file mesa-vulkan-drivers
"$repo_root/dist/verify-payload.sh" /
rpm -q qos-session >/dev/null
rpm -q --requires qos-session | grep -Fxq bubblewrap
rpm -q --requires qos-session | grep -Fxq xwayland-satellite
rpm -q --requires qos-session | grep -Fxq xdg-desktop-portal
rpm -q --requires qos-session | grep -Fxq xdg-dbus-proxy
rpm -q --requires qos-session | grep -Fxq dbus-tools
rpm -q --requires qos-session | grep -Fxq upower
rpm -q --recommends qos-session | grep -Fxq mesa-vulkan-drivers
if (( expect_devel )); then
    "$repo_root/dist/verify-devel-payload.sh" /
    rpm -q qos-session-devel >/dev/null
fi
"$repo_root/dist/ci/test-installed-host-startup.sh"
