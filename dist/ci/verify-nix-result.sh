#!/usr/bin/env bash
# Verify the installed Nix result without assuming an FHS /usr prefix.
set -euo pipefail

[[ $# == 2 ]] || { echo "usage: $0 RESULT amd64|arm64" >&2; exit 2; }
result=$(realpath "$1")
arch=$2
case "$arch" in amd64|arm64) ;; *) exit 2 ;; esac

for file in bin/qos-shell bin/qos-launcher libexec/qos-unlock-helper libexec/qos-session; do
    [[ -x $result/$file ]] || { echo "missing executable: $file" >&2; exit 1; }
done
for file in etc/pam.d/qos-unlock share/wayland-sessions/qos.desktop \
            share/xdg-desktop-portal/portals/qos.portal \
            share/licenses/qos-session/LICENSE; do
    [[ -f $result/$file ]] || { echo "missing file: $file" >&2; exit 1; }
done

entry=$result/share/wayland-sessions/qos.desktop
portal=$result/share/xdg-desktop-portal/portals/qos.portal
wrapper=$result/libexec/qos-session
grep -Fqx "Exec=$result/libexec/qos-session" "$entry"
grep -Fqx "TryExec=$result/bin/qos-shell" "$entry"
grep -q '^DesktopNames=nirios$' "$entry"
grep -q '^UseIn=nirios$' "$portal"
grep -Fqx "QOS_SHELL=$result/bin/qos-shell" "$wrapper"
grep -q -- '--firstboot' "$wrapper"

for elf in bin/qos-shell bin/qos-launcher libexec/qos-unlock-helper; do
    description=$(file "$result/$elf")
    if [[ $arch == amd64 ]]; then
        grep -Eq 'ELF 64-bit.*x86-64' <<<"$description"
    else
        grep -Eq 'ELF 64-bit.*ARM aarch64' <<<"$description"
    fi
    if ldd "$result/$elf" | tee /dev/stderr | grep -q 'not found'; then
        echo "unresolved shared library in $elf" >&2
        exit 1
    fi
done
