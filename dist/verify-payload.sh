#!/usr/bin/env bash
# Format-independent assertions run before and after package installation.
set -euo pipefail

[[ $# == 1 ]] || { echo "usage: $0 ROOT" >&2; exit 2; }
root=${1%/}
[[ -n $root ]] || root=/
path() { printf '%s%s' "${root%/}" "$1"; }

for file in /usr/bin/qos-shell /usr/bin/qos-launcher \
            /usr/libexec/qos-unlock-helper /usr/libexec/qos-session; do
    [[ -x $(path "$file") ]] || { echo "missing executable: $file" >&2; exit 1; }
done
for file in /etc/pam.d/qos-unlock \
            /usr/share/wayland-sessions/qos.desktop \
            /usr/share/xdg-desktop-portal/portals/qos.portal; do
    [[ -f $(path "$file") ]] || { echo "missing file: $file" >&2; exit 1; }
done
if [[ ! -f $(path /usr/share/licenses/qos-session/LICENSE) \
      && ! -f $(path /usr/share/doc/qos-session/copyright) ]]; then
    echo "missing qOS license text" >&2
    exit 1
fi

grep -q '^Exec=/usr/libexec/qos-session$' "$(path /usr/share/wayland-sessions/qos.desktop)"
grep -q '^TryExec=/usr/bin/qos-shell$' "$(path /usr/share/wayland-sessions/qos.desktop)"
grep -q '^DesktopNames=nirios$' "$(path /usr/share/wayland-sessions/qos.desktop)"
grep -q '^UseIn=nirios$' "$(path /usr/share/xdg-desktop-portal/portals/qos.portal)"
grep -q '^QOS_SHELL=/usr/bin/qos-shell$' "$(path /usr/libexec/qos-session)"
grep -q -- '--firstboot' "$(path /usr/libexec/qos-session)"
# The compositor owns the portal bus name in-process. A D-Bus activator or
# separate user unit would race it and serve an empty workspace.
[[ ! -e $(path /usr/share/dbus-1/services/org.freedesktop.impl.portal.desktop.qos.service) ]]
[[ ! -e $(path /usr/lib/systemd/user/qos-portal.service) ]]

for elf in /usr/bin/qos-shell /usr/bin/qos-launcher /usr/libexec/qos-unlock-helper; do
    elf_description=$(file "$(path "$elf")")
    case ${QOS_EXPECT_ARCH:-} in
        amd64) grep -Eq 'ELF 64-bit.*x86-64' <<<"$elf_description" ;;
        arm64) grep -Eq 'ELF 64-bit.*ARM aarch64' <<<"$elf_description" ;;
        "") grep -Eq 'ELF 64-bit.*(x86-64|ARM aarch64)' <<<"$elf_description" ;;
        *) echo "unsupported QOS_EXPECT_ARCH: $QOS_EXPECT_ARCH" >&2; exit 2 ;;
    esac
    if ldd "$(path "$elf")" | tee /dev/stderr | grep -q 'not found'; then
        echo "unresolved shared library in $elf" >&2
        exit 1
    fi
done
