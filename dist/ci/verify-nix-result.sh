#!/usr/bin/env bash
# Verify the installed Nix result without assuming an FHS /usr prefix.
set -euo pipefail

[[ $# == 2 ]] || { echo "usage: $0 RESULT amd64|arm64" >&2; exit 2; }
result=$(realpath "$1")
arch=$2
case "$arch" in amd64|arm64) ;; *) exit 2 ;; esac
case "$(uname -m)" in
x86_64) execution_arch=amd64 ;;
aarch64) execution_arch=arm64 ;;
*) echo "unsupported execution architecture: $(uname -m)" >&2; exit 1 ;;
esac

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
    # e_ident[EI_CLASS] is byte 4 (2 = ELF64), and little-endian e_machine is
    # the 16-bit word at byte 18 (62 = x86-64, 183 = AArch64). Using POSIX
    # tooling already present in the Nix image avoids mutating its emulated
    # user profile merely to install `file` before the real build starts.
    class=$(od -An -t u1 -j 4 -N 1 "$result/$elf" | tr -d ' ')
    machine=$(od -An -t u2 -j 18 -N 2 "$result/$elf" | tr -d ' ')
    [[ $class == 2 ]] || { echo "not a 64-bit ELF: $elf" >&2; exit 1; }
    if [[ $arch == amd64 ]]; then
        [[ $machine == 62 ]]
    else
        [[ $machine == 183 ]]
    fi
    if [[ $execution_arch == "$arch" ]]; then
        if ldd "$result/$elf" | tee /dev/stderr | grep -q 'not found'; then
            echo "unresolved shared library in $elf" >&2
            exit 1
        fi
    fi
done

# Cross-built binaries cannot be executed by host `ldd`. Verify the output's
# content hash and require Nix to resolve its complete registered closure; the
# native architecture job retains the executable ldd check above.
if command -v nix-store >/dev/null 2>&1; then
    nix-store --verify-path "$result"
    nix-store --query --requisites "$result" >/dev/null
fi
