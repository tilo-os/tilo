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
# display-free compositor. Fedora desktops normally already carry a GPU
# ICD; the isolated package userspace must still prove the fallback path.
dnf install -y "$@" file mesa-vulkan-drivers
"$repo_root/dist/verify-payload.sh" /
rpm -q tilo-session >/dev/null
rpm -q tilo-session-apps >/dev/null
rpm -q --provides tilo-session | grep -Eq '^tilo-session = '
rpm -q --obsoletes tilo-session | grep -Fxq 'tilo-session < 0.1.9'
rpm -q --provides tilo-session-apps | grep -Eq '^tilo-session-apps = '
rpm -q --obsoletes tilo-session-apps | grep -Fxq 'tilo-session-apps < 0.1.9'
rpm -q --requires tilo-session | grep -Fxq bubblewrap
rpm -q --requires tilo-session | grep -Fxq xwayland-satellite
rpm -q --requires tilo-session | grep -Fxq xdg-desktop-portal
rpm -q --requires tilo-session | grep -Fxq xdg-dbus-proxy
rpm -q --requires tilo-session | grep -Fxq dbus-tools
rpm -q --requires tilo-session | grep -Fxq fuse3
rpm -q --requires tilo-session | grep -Fxq python3
rpm -q --requires tilo-session | grep -Fxq upower
rpm -q --recommends tilo-session | grep -Fxq mesa-vulkan-drivers
rpm -q --requires tilo-session | grep -Eq '^tilo-session-apps(\([^)]*\))? = '
! rpm -q --requires tilo-session-apps | grep -Eq \
    '^(ld-linux|libc\.so|libstd-|libruntime_dylib\.so)'
! rpm -q --provides tilo-session-apps | grep -Eq \
    '^(ld-linux|libc\.so|libstd-|libruntime_dylib\.so)'
# Inspect installed, package-owned tools and parse help only. This must never
# enroll, initialize, start a Files service or create credentials during QA.
for program in fs tilo-relay-admin tilo-enroll; do
    test -x "/usr/bin/$program"
    test "$(rpm -qf "/usr/bin/$program")" = "$(rpm -q tilo-session)"
    "/usr/bin/$program" --help >/dev/null
done
test -x /usr/bin/app
/usr/bin/app --help >/dev/null
owner=/usr/share/tilo/trust/tilo-main-registry-owner.pub
test "$(rpm -qf "$owner")" = "$(rpm -q tilo-session-apps)"
grep -Eiq '^[0-9a-f]{64}$' "$owner"
! grep -Eq '^0{64}$' "$owner"
# An older Store could save an empty registry list when the pin was missing.
# The installed reader must migrate it using the packaged authority.
config=$(mktemp -d)
mkdir -p "$config/tilo-store"
printf 'registry = []\n' > "$config/tilo-store/registries.toml"
XDG_CONFIG_HOME=$config /usr/bin/app repo list > "$config/repos.txt"
grep -Fq 'Tilo main' "$config/repos.txt"
grep -Fq "$(cat "$owner")" "$config/tilo-store/registries.toml"
rm -rf -- "$config"
test -s /usr/share/tilo-apps/registry.toml
grep -Fxq 'id = "so.tilo.settings"' /usr/share/tilo-apps/registry.toml
grep -q '^payload_root = "/usr/lib/tilo-apps/' /usr/share/tilo-apps/registry.toml
test -d /usr/lib/tilo-apps/pkgs
test ! -e /usr/share/tilo-apps/pkgs
test ! -e /usr/lib/tilo-apps/content
# One runtime build of the 2026.07 release, named by its ABI digest.
runtime_dirs=(/usr/lib/tilo-runtime/2026.07+native.*)
test "${#runtime_dirs[@]}" -eq 1
runtime_dir=${runtime_dirs[0]}
test -f "$runtime_dir/bundle.toml"
test "$(stat -c %a "$runtime_dir/bundle.toml")" = 644
grep -q '/usr/lib/tilo-apps/bin' /etc/profile.d/tilo-apps.sh
test -s /etc/tilo/runtime-bundle-trusted-publishers.txt
tilo-runtime-bundle verify "$runtime_dir" \
    --require-publisher "$(head -n 1 /etc/tilo/runtime-bundle-trusted-publishers.txt)"
# The trust pin and signed runtime must remain one package-owned generation.
# Merely verifying them independently missed an upgrade where `%config(noreplace)`
# retained the previous key and every app failed only at launcher policy time.
test "$(rpm -qf /etc/tilo/runtime-bundle-trusted-publishers.txt)" = \
    "$(rpm -q tilo-session-apps)"
bundle_publisher=$(sed -n 's/^publisher_hex = "\([0-9a-fA-F]\{64\}\)"$/\1/p' \
    "$runtime_dir/bundle.toml")
test "${#bundle_publisher}" -eq 64
grep -Fxiq "$bundle_publisher" /etc/tilo/runtime-bundle-trusted-publishers.txt
# The ush supervisor runs these: a relay environment's pty bridge, the relay
# picker's asker, and the legacy VM service behind app shims.
for program in /usr/libexec/tilo-ush /usr/libexec/tilo-relay-environment \
               /usr/libexec/tilo-relay-offers /usr/libexec/tilo/tilo-legacy; do
    test -x "$program"
    test "$(rpm -qf "$program")" = "$(rpm -q tilo-session-apps)"
done
/usr/libexec/tilo-relay-environment --help >/dev/null
/usr/libexec/tilo-relay-offers --help >/dev/null
for unit in tilo-app-host tilo-models tilo-model-gateway tilo-ush; do
    test -f "/usr/lib/systemd/user/$unit.service"
    test -f "/usr/lib/systemd/user/$unit.socket"
    grep -q "^Service=$unit.service$" "/usr/lib/systemd/user/$unit.socket"
done
if (( expect_devel )); then
    "$repo_root/dist/verify-devel-payload.sh" /
    rpm -q tilo-session-devel >/dev/null
    rpm -q --provides tilo-session-devel | grep -Eq '^tilo-session-devel = '
    rpm -q --obsoletes tilo-session-devel | grep -Fxq 'tilo-session-devel < 0.1.9'
fi
"$repo_root/dist/ci/test-installed-host-startup.sh"
