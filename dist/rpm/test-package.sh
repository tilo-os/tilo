#!/usr/bin/env bash
set -euo pipefail
[[ $# -ge 2 ]] || {
    echo "usage: $0 tilo-session.rpm tilo-session-apps.rpm [tilo-session-devel.rpm]" >&2
    exit 2
}
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
expect_devel=0
for package in "$@"; do
    [[ $(basename "$package") != tilo-session-devel-* ]] || expect_devel=1
done
# Package lanes deliberately install build dependencies without weak deps, so
# install the recommended software Vulkan ICD explicitly before exercising the
# display-free compositor. Real Fedora desktops normally already carry a GPU
# ICD; the isolated package userspace must still prove the fallback path.
dnf install -y "$@" file mesa-vulkan-drivers
"$repo_root/dist/verify-payload.sh" /
rpm -q tilo-session >/dev/null
rpm -q tilo-session-apps >/dev/null
rpm -q --requires tilo-session | grep -Fxq bubblewrap
rpm -q --requires tilo-session | grep -Fxq xwayland-satellite
rpm -q --requires tilo-session | grep -Fxq xdg-desktop-portal
rpm -q --requires tilo-session | grep -Fxq xdg-dbus-proxy
rpm -q --requires tilo-session | grep -Fxq dbus-tools
rpm -q --requires tilo-session | grep -Fxq upower
rpm -q --recommends tilo-session | grep -Fxq mesa-vulkan-drivers
rpm -q --requires tilo-session | grep -Eq '^tilo-session-apps(\([^)]*\))? = '
test -s /usr/share/tilo-apps/registry.toml
grep -Fxq 'id = "dev.tilo.settings"' /usr/share/tilo-apps/registry.toml
test -f /usr/lib/tilo-runtime/2026.07/bundle.toml
test -s /etc/tilo/runtime-bundle-trusted-publishers.txt
for unit in tilo-app-host tilo-model-storage tilo-models tilo-model-gateway tilo-ush; do
    test -f "/usr/lib/systemd/user/$unit.service"
done
if (( expect_devel )); then
    "$repo_root/dist/verify-devel-payload.sh" /
    rpm -q tilo-session-devel >/dev/null
fi
"$repo_root/dist/ci/test-installed-host-startup.sh"
