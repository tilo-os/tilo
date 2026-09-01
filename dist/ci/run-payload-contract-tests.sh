#!/usr/bin/env bash
# Exercise format-independent installed-layout verifiers without compiling qOS.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
work=$(mktemp -d)
cleanup() { rm -rf -- "$work"; }
trap cleanup EXIT
true_bin=$(type -P true)

fhs=$work/fhs
mkdir -p "$fhs"/{usr/bin,usr/libexec,etc/pam.d,usr/share/wayland-sessions,usr/share/xdg-desktop-portal/portals,usr/share/licenses/qos-session}
for target in "$fhs/usr/bin/qos-shell" "$fhs/usr/bin/qos-launcher" \
              "$fhs/usr/libexec/qos-unlock-helper"; do
    install -m755 "$true_bin" "$target"
done
install -m755 "$repo_root/dist/session/qos-session" "$fhs/usr/libexec/qos-session"
install -m755 "$repo_root/dist/session/qos-session-dev" "$fhs/usr/libexec/qos-session-dev"
install -m644 "$repo_root/dist/session/qos.desktop" "$fhs/usr/share/wayland-sessions/qos.desktop"
install -m644 "$repo_root/dist/session/qos-dev.desktop" "$fhs/usr/share/wayland-sessions/qos-dev.desktop"
install -m644 "$repo_root/os/etc/pam.d/qos-unlock" "$fhs/etc/pam.d/qos-unlock"
install -m644 "$repo_root/os/usr/share/xdg-desktop-portal/portals/qos.portal" \
    "$fhs/usr/share/xdg-desktop-portal/portals/qos.portal"
install -m644 "$repo_root/LICENSE" "$fhs/usr/share/licenses/qos-session/LICENSE"
QOS_EXPECT_ARCH=amd64 "$repo_root/dist/verify-payload.sh" "$fhs"
"$repo_root/dist/verify-devel-payload.sh" "$fhs"

sed -i 's#^TryExec=.*#TryExec=/wrong#' "$fhs/usr/share/wayland-sessions/qos-dev.desktop"
if "$repo_root/dist/verify-devel-payload.sh" "$fhs" >/dev/null 2>&1; then
    echo "developer payload verifier accepted a wrong TryExec" >&2
    exit 1
fi

nix=$work/nix-result
mkdir -p "$nix"/{bin,libexec,etc/pam.d,share/wayland-sessions,share/xdg-desktop-portal/portals,share/licenses/qos-session}
for target in "$nix/bin/qos-shell" "$nix/bin/qos-launcher" \
              "$nix/libexec/qos-unlock-helper"; do
    install -m755 "$true_bin" "$target"
done
install -m755 "$repo_root/dist/session/qos-session" "$nix/libexec/qos-session"
install -m644 "$repo_root/dist/session/qos.desktop" "$nix/share/wayland-sessions/qos.desktop"
install -m644 "$repo_root/os/etc/pam.d/qos-unlock" "$nix/etc/pam.d/qos-unlock"
install -m644 "$repo_root/os/usr/share/xdg-desktop-portal/portals/qos.portal" \
    "$nix/share/xdg-desktop-portal/portals/qos.portal"
install -m644 "$repo_root/LICENSE" "$nix/share/licenses/qos-session/LICENSE"
sed -i \
    -e "s#QOS_SHELL=/usr/bin/qos-shell#QOS_SHELL=$nix/bin/qos-shell#" \
    "$nix/libexec/qos-session"
sed -i \
    -e "s#Exec=/usr/libexec/qos-session#Exec=$nix/libexec/qos-session#" \
    -e "s#TryExec=/usr/bin/qos-shell#TryExec=$nix/bin/qos-shell#" \
    "$nix/share/wayland-sessions/qos.desktop"
"$repo_root/dist/ci/verify-nix-result.sh" "$nix" amd64

echo "payload contract fixtures passed"
